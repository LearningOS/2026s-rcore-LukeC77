//! Implementation of [`MapArea`] and [`MemorySet`].
use super::{frame_alloc, FrameTracker};
use super::{PTEFlags, PageTable, PageTableEntry};
use super::{PhysAddr, PhysPageNum, VirtAddr, VirtPageNum};
use super::{StepByOne, VPNRange};
use crate::config::{MEMORY_END, PAGE_SIZE, TRAMPOLINE, TRAP_CONTEXT_BASE, USER_STACK_SIZE};
use crate::sync::UPSafeCell;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::arch::asm;
use lazy_static::*;
use riscv::register::satp;

extern "C" {
    fn stext();
    fn etext();
    fn srodata();
    fn erodata();
    fn sdata();
    fn edata();
    fn sbss_with_stack();
    fn ebss();
    fn ekernel();
    fn strampoline();
}

lazy_static! {
    /// The kernel's initial memory mapping(kernel address space)
    pub static ref KERNEL_SPACE: Arc<UPSafeCell<MemorySet>> =
        Arc::new(unsafe { UPSafeCell::new(MemorySet::new_kernel()) });
}
/// address space
pub struct MemorySet {
    page_table: PageTable,
    areas: Vec<MapArea>,
}

impl MemorySet {
    /// Create a new empty `MemorySet`.
    pub fn new_bare() -> Self {
        Self {
            page_table: PageTable::new(),
            areas: Vec::new(),
        }
    }
    /// Get the page table token
    pub fn token(&self) -> usize {
        self.page_table.token()
    }
    /// Assume that no conflicts.
    pub fn insert_framed_area(
        &mut self,
        start_va: VirtAddr,
        end_va: VirtAddr,
        permission: MapPermission,
    ) {
        self.push(
            MapArea::new(start_va, end_va, MapType::Framed, permission),
            None,
        );
    }
    /// remove a area
    pub fn remove_area_with_start_vpn(&mut self, start_vpn: VirtPageNum) {
        if let Some((idx, area)) = self
            .areas
            .iter_mut()
            .enumerate()
            .find(|(_, area)| area.vpn_range.get_start() == start_vpn)
        {
            area.unmap(&mut self.page_table);
            self.areas.remove(idx);
        }
    }
    /// Add a new MapArea into this MemorySet.
    /// Assuming that there are no conflicts in the virtual address
    /// space.
    fn push(&mut self, mut map_area: MapArea, data: Option<&[u8]>) {
        map_area.map(&mut self.page_table);
        if let Some(data) = data {
            map_area.copy_data(&mut self.page_table, data);
        }
        self.areas.push(map_area);
    }
    /// Mention that trampoline is not collected by areas.
    fn map_trampoline(&mut self) {
        self.page_table.map(    // yifan 2026/5/26: 调用页表映射接口，建立 trampoline 的页表项。
            VirtAddr::from(TRAMPOLINE).into(),    // yifan 2026/5/26: 映射目标虚拟地址为固定 TRAMPOLINE 高地址（转换为 VPN）。
            PhysAddr::from(strampoline as usize).into(),    // yifan 2026/5/26: 映射源物理地址为内核 trampoline 代码符号 strampoline（转换为 PPN）。
            PTEFlags::R | PTEFlags::X,    // yifan 2026/5/26: 权限设置为可读+可执行，不可写。
        );
    }
    /// Without kernel stacks.
    pub fn new_kernel() -> Self {
        let mut memory_set = Self::new_bare();
        // map trampoline
        memory_set.map_trampoline();
        // map kernel sections
        info!(".text [{:#x}, {:#x})", stext as usize, etext as usize);
        info!(".rodata [{:#x}, {:#x})", srodata as usize, erodata as usize);
        info!(".data [{:#x}, {:#x})", sdata as usize, edata as usize);
        info!(
            ".bss [{:#x}, {:#x})",
            sbss_with_stack as usize, ebss as usize
        );
        info!("mapping .text section");
        memory_set.push(
            MapArea::new(
                (stext as usize).into(),
                (etext as usize).into(),
                MapType::Identical,
                MapPermission::R | MapPermission::X,
            ),
            None,
        );
        info!("mapping .rodata section");
        memory_set.push(
            MapArea::new(
                (srodata as usize).into(),
                (erodata as usize).into(),
                MapType::Identical,
                MapPermission::R,
            ),
            None,
        );
        info!("mapping .data section");
        memory_set.push(
            MapArea::new(
                (sdata as usize).into(),
                (edata as usize).into(),
                MapType::Identical,
                MapPermission::R | MapPermission::W,
            ),
            None,
        );
        info!("mapping .bss section");
        memory_set.push(
            MapArea::new(
                (sbss_with_stack as usize).into(),
                (ebss as usize).into(),
                MapType::Identical,
                MapPermission::R | MapPermission::W,
            ),
            None,
        );
        info!("mapping physical memory");
        memory_set.push(
            MapArea::new(
                (ekernel as usize).into(),
                MEMORY_END.into(),
                MapType::Identical,
                MapPermission::R | MapPermission::W,
            ),
            None,
        );
        memory_set
    }
    /// Include sections in elf and trampoline and TrapContext and user stack,
    /// also returns user_sp_base and entry point.
    pub fn from_elf(elf_data: &[u8]) -> (Self, usize, usize) {
        let mut memory_set = Self::new_bare();
        // map trampoline
        memory_set.map_trampoline();
        // map program headers of elf, with U flag
        let elf = xmas_elf::ElfFile::new(elf_data).unwrap();
        let elf_header = elf.header;
        let magic = elf_header.pt1.magic;
        assert_eq!(magic, [0x7f, 0x45, 0x4c, 0x46], "invalid elf!");
        let ph_count = elf_header.pt2.ph_count();
        let mut max_end_vpn = VirtPageNum(0);
        for i in 0..ph_count {
            let ph = elf.program_header(i).unwrap();
            if ph.get_type().unwrap() == xmas_elf::program::Type::Load {
                let start_va: VirtAddr = (ph.virtual_addr() as usize).into();
                let end_va: VirtAddr = ((ph.virtual_addr() + ph.mem_size()) as usize).into();
                let mut map_perm = MapPermission::U;
                let ph_flags = ph.flags();
                if ph_flags.is_read() {
                    map_perm |= MapPermission::R;
                }
                if ph_flags.is_write() {
                    map_perm |= MapPermission::W;
                }
                if ph_flags.is_execute() {
                    map_perm |= MapPermission::X;
                }
                let map_area = MapArea::new(start_va, end_va, MapType::Framed, map_perm);
                max_end_vpn = map_area.vpn_range.get_end();
                memory_set.push(
                    map_area,
                    Some(&elf.input[ph.offset() as usize..(ph.offset() + ph.file_size()) as usize]),
                );
            }
        }
        // map user stack with U flags
        let max_end_va: VirtAddr = max_end_vpn.into();
        let mut user_stack_bottom: usize = max_end_va.into();
        // guard page
        user_stack_bottom += PAGE_SIZE;
        let user_stack_top = user_stack_bottom + USER_STACK_SIZE;
        memory_set.push(
            MapArea::new(
                user_stack_bottom.into(),
                user_stack_top.into(),
                MapType::Framed,
                MapPermission::R | MapPermission::W | MapPermission::U,
            ),
            None,
        );
        // used in sbrk
        memory_set.push(
            MapArea::new(
                user_stack_top.into(),
                user_stack_top.into(),
                MapType::Framed,
                MapPermission::R | MapPermission::W | MapPermission::U,
            ),
            None,
        );
        // map TrapContext
        memory_set.push(
            MapArea::new(
                TRAP_CONTEXT_BASE.into(),
                TRAMPOLINE.into(),
                MapType::Framed,
                MapPermission::R | MapPermission::W,
            ),
            None,
        );
        (
            memory_set,
            user_stack_top,
            elf.header.pt2.entry_point() as usize,
        )
    }
    /// Create a new address space by copy code&data from a exited process's address space.
    pub fn from_existed_user(user_space: &Self) -> Self {
        let mut memory_set = Self::new_bare();    // yifan 2026/5/26: 新建空地址空间（空页表与空 areas），作为子进程地址空间骨架。
        // map trampoline
        memory_set.map_trampoline();    // yifan 2026/5/26: 先映射 trampoline，保证 trap 进入/返回路径可用。这是因为我们解析 ELF 创建地址空间的时候，并没有将跳板页作为一个单独的逻辑段插入到地址空间的逻辑段向量 areas 中，所以这里需要单独映射上。
        // copy data sections/trap_context/user_stack
        for area in user_space.areas.iter() {    // yifan 2026/5/26: 遍历父进程每个 MapArea（代码段/数据段/用户栈/trap context 等）。
            let new_area = MapArea::from_another(area);    // yifan 2026/5/26: 复制区域元信息（VPN 范围、映射类型、权限），不复制物理帧。
            memory_set.push(new_area, None);    // yifan 2026/5/26: 将区域加入新地址空间并建立映射；Framed 区域会为子进程分配新物理页。
            // copy data from another space
            for vpn in area.vpn_range {    // yifan 2026/5/26: 按虚拟页逐页复制父进程内容到子进程。
                let src_ppn = user_space.translate(vpn).unwrap().ppn();    // yifan 2026/5/26: 查询父地址空间该 VPN 对应的物理页号。
                let dst_ppn = memory_set.translate(vpn).unwrap().ppn();    // yifan 2026/5/26: 查询子地址空间该 VPN 对应的物理页号。
                dst_ppn
                    .get_bytes_array()
                    .copy_from_slice(src_ppn.get_bytes_array());    // yifan 2026/5/26: 将父页整页字节拷贝到子页，实现深拷贝而非 COW 共享。
            }
        }
        memory_set    // yifan 2026/5/26: 返回构造完成的新用户地址空间（虚拟布局相同、物理页独立）。
    }
    /// Change page table by writing satp CSR Register.
    pub fn activate(&self) {
        let satp = self.page_table.token();
        unsafe {
            satp::write(satp);
            asm!("sfence.vma");
        }
    }
    /// Translate a virtual page number to a page table entry
    pub fn translate(&self, vpn: VirtPageNum) -> Option<PageTableEntry> {
        self.page_table.translate(vpn)
    }

    ///Remove all `MapArea`
    pub fn recycle_data_pages(&mut self) {
        self.areas.clear();
    }

    /// shrink the area to new_end
    #[allow(unused)]
    pub fn shrink_to(&mut self, start: VirtAddr, new_end: VirtAddr) -> bool {
        if let Some(area) = self
            .areas
            .iter_mut()
            .find(|area| area.vpn_range.get_start() == start.floor())
        {
            area.shrink_to(&mut self.page_table, new_end.ceil());
            true
        } else {
            false
        }
    }

    /// append the area to new_end
    #[allow(unused)]
    pub fn append_to(&mut self, start: VirtAddr, new_end: VirtAddr) -> bool {
        if let Some(area) = self
            .areas
            .iter_mut()
            .find(|area| area.vpn_range.get_start() == start.floor())
        {
            area.append_to(&mut self.page_table, new_end.ceil());
            true
        } else {
            false
        }
    }

    /// yifan 2026/5/15 为sys_mmap写的方法，检查是否内存已经映射
    pub fn overlap(&self, start_va: VirtAddr, end_va: VirtAddr) -> bool {
        let start_vpn = start_va.floor();
        let end_vpn = end_va.ceil();
        // 映射的范围是[start_vpn, end_vpn)，所以当 area 的 vpn_range 在 end_vpn 之前或者在 start_vpn 之后时才不重叠
        for area in self.areas.iter() {
            let area_l = area.vpn_range.get_start();
            let area_r = area.vpn_range.get_end();
            if area_l >= end_vpn || area_r <= start_vpn{
                continue;
            } else {
                return true;
            }
        }
        return false;
    }


    /// yifan 2026/5/15 为sys_munmap写的方法，检查是否内存已经映射
    pub fn full_mapped(&self, start_va: VirtAddr, end_va: VirtAddr) -> bool {
        let start_vpn = start_va.floor();
        let end_vpn = end_va.ceil();
        for vpn in VPNRange::new(start_vpn, end_vpn) {
            let pte = self.translate(vpn);
            let mut mapped = false;
            if pte.is_some() && pte.unwrap().is_valid() {
                mapped = true;
            }
            trace!("full_mapped check vpn={:?} mapped={}", vpn, mapped);
            if !mapped {
                return false;
            }
        }
        true
    }

    /// yifan 2026/5/15 为sys_munmap写的方法, 取消映射并回收物理页帧
    pub fn unmap(&mut self, start_va: VirtAddr, end_va: VirtAddr) {
        let start_vpn = start_va.floor();
        let end_vpn = end_va.ceil();
        let mut v: Vec<VPNRange> = Vec::new();
        let (areas, page_table) = (&mut self.areas, &mut self.page_table);

        //遍历areas，找到与[start_vpn, end_vpn)重叠的部分，记录在vec中
        for area in areas.iter() {
            let area_l = area.vpn_range.get_start();
            let area_r = area.vpn_range.get_end();
            if area_l >= end_vpn || area_r <= start_vpn { // 不重叠
                v.push(VPNRange::new(area_l, area_l)); // 不重叠的部分记录为一个空区间，后续遍历vec时会跳过
            } else if area_l >= start_vpn && area_r <= end_vpn { // 完全重叠
                v.push(VPNRange::new(area_l, area_r));
            } else if area_l >= start_vpn && area_r > end_vpn { // 左边界重叠
                v.push(VPNRange::new(area_l, end_vpn));
            } else if area_l < start_vpn && area_r <= end_vpn { // 右边界重叠
                v.push(VPNRange::new(start_vpn, area_r));
            } else { // 中间重叠: area_l < start_vpn && area_r > end_vpn
                v.push(VPNRange::new(start_vpn, end_vpn));
            }
        }

        // 记录需要取消映射的区间的索引，之后统一从后向前删除
        let mut delete_indices: Vec<usize> = Vec::new();
        //遍历vec，取消映射
        for (i, range) in v.iter().enumerate() {
            let new_l = range.get_start();
            let new_r = range.get_end();
            if new_l == new_r { // 空区间，跳过
                continue;
            }
            
            let area_l = areas[i].vpn_range.get_start();
            let area_r = areas[i].vpn_range.get_end();
            
            if area_l == new_l && area_r == new_r { // 完全重叠，直接删除
                areas[i].unmap(page_table);
                //暂时不删除vector中的areas[i]，否则后续遍历会出问题。可以先做标记，最后统一删除
                delete_indices.push(i);
            } else if area_l == new_l && area_r > new_r { // 左边界重叠，修改左边界，保留右边界
                for vpn in VPNRange::new(area_l, new_r) {
                    areas[i].unmap_one(page_table, vpn);
                }
                areas[i].vpn_range = VPNRange::new(new_r, area_r);
            } else if area_l < new_l && area_r == new_r { // 右边界重叠，修改右边界，保留左边界
                areas[i].shrink_to(page_table, new_l);
            } else { // 中间重叠: area_l < new_l && area_r > new_r, 修改右边界，保留左边界
                let right_data_frames = areas[i].data_frames.split_off(&new_r); // 将右边界之后的映射关系分离出来，保存在right_data_frames中
                let _mid_data_frames = areas[i].data_frames.split_off(&new_l); // 将中间区间的映射关系分离出来，保存在mid_data_frames中, 同时也将左侧区间的映射关系保留在areas[i].data_frames中
                // 取消[new_l, new_r)的映射关系
                for vpn in VPNRange::new(new_l, new_r) {
                    areas[i].unmap_one(page_table, vpn);
                }
                // 修改原区间的右边界，保留左边界
                areas[i].vpn_range = VPNRange::new(area_l, new_l);
                // 将右边界之后的映射关系重新插入到areas中
                let map_type = areas[i].map_type;
                let map_perm = areas[i].map_perm;
                areas.push(MapArea {
                    vpn_range: VPNRange::new(new_r, area_r),
                    data_frames: right_data_frames,
                    map_type,
                    map_perm,
                });
            }
        }

        // 从后向前删除完全重叠的区间，避免索引问题
        for i in delete_indices.iter().rev() {
            areas.remove(*i);
        }


    }


}
/// map area structure, controls a contiguous piece of virtual memory
pub struct MapArea {
    vpn_range: VPNRange,
    data_frames: BTreeMap<VirtPageNum, FrameTracker>,    // yifan 2026/5/25: 记录本 MapArea 在 MapType::Framed 下的 VPN -> FrameTracker（物理帧所有权）映射；真正硬件使用的 VPN->PPN 映射在页表中。yifan 2026/5/25: fork 通过 from_existed_user 为子进程重新分配 Framed 页并逐页拷贝数据，因此子进程 data_frames 与父进程不同，不共享同一批物理帧。
    map_type: MapType,
    map_perm: MapPermission,
}

impl MapArea {
    pub fn new(
        start_va: VirtAddr,
        end_va: VirtAddr,
        map_type: MapType,
        map_perm: MapPermission,
    ) -> Self {
        let start_vpn: VirtPageNum = start_va.floor();
        let end_vpn: VirtPageNum = end_va.ceil();
        Self {
            vpn_range: VPNRange::new(start_vpn, end_vpn),
            data_frames: BTreeMap::new(),
            map_type,
            map_perm,
        }
    }
    pub fn from_another(another: &Self) -> Self {
        Self {
            vpn_range: VPNRange::new(another.vpn_range.get_start(), another.vpn_range.get_end()),    // yifan 2026/5/25: 复制原 MapArea 的虚拟页范围（起始/结束 VPN 一致）。
            data_frames: BTreeMap::new(),    // yifan 2026/5/25: 不复制原物理帧持有表，新的 MapArea 先为空，后续映射时再建立。
            map_type: another.map_type,    // yifan 2026/5/25: 复制映射类型（Identical 或 Framed）。
            map_perm: another.map_perm,    // yifan 2026/5/25: 复制页权限位（R/W/X/U）。
        }
    }
    pub fn map_one(&mut self, page_table: &mut PageTable, vpn: VirtPageNum) {
        let ppn: PhysPageNum;
        match self.map_type {
            MapType::Identical => {
                ppn = PhysPageNum(vpn.0);
            }
            MapType::Framed => {
                let frame = frame_alloc().unwrap();
                ppn = frame.ppn;
                self.data_frames.insert(vpn, frame);
            }
        }
        let pte_flags = PTEFlags::from_bits(self.map_perm.bits).unwrap();
        page_table.map(vpn, ppn, pte_flags);
    }
    pub fn unmap_one(&mut self, page_table: &mut PageTable, vpn: VirtPageNum) {
        if self.map_type == MapType::Framed {
            self.data_frames.remove(&vpn);
        }
        page_table.unmap(vpn);
    }
    pub fn map(&mut self, page_table: &mut PageTable) {
        for vpn in self.vpn_range {
            self.map_one(page_table, vpn);
        }
    }
    pub fn unmap(&mut self, page_table: &mut PageTable) {
        for vpn in self.vpn_range {
            self.unmap_one(page_table, vpn);
        }
    }
    #[allow(unused)]
    pub fn shrink_to(&mut self, page_table: &mut PageTable, new_end: VirtPageNum) {
        for vpn in VPNRange::new(new_end, self.vpn_range.get_end()) {
            self.unmap_one(page_table, vpn)
        }
        self.vpn_range = VPNRange::new(self.vpn_range.get_start(), new_end);
    }
    #[allow(unused)]
    pub fn append_to(&mut self, page_table: &mut PageTable, new_end: VirtPageNum) {
        for vpn in VPNRange::new(self.vpn_range.get_end(), new_end) {
            self.map_one(page_table, vpn)
        }
        self.vpn_range = VPNRange::new(self.vpn_range.get_start(), new_end);
    }
    /// data: start-aligned but maybe with shorter length
    /// assume that all frames were cleared before
    pub fn copy_data(&mut self, page_table: &mut PageTable, data: &[u8]) {
        assert_eq!(self.map_type, MapType::Framed);
        let mut start: usize = 0;
        let mut current_vpn = self.vpn_range.get_start();
        let len = data.len();
        loop {
            let src = &data[start..len.min(start + PAGE_SIZE)];
            let dst = &mut page_table
                .translate(current_vpn)
                .unwrap()
                .ppn()
                .get_bytes_array()[..src.len()];
            dst.copy_from_slice(src);
            start += PAGE_SIZE;
            if start >= len {
                break;
            }
            current_vpn.step();
        }
    }
}

#[derive(Copy, Clone, PartialEq, Debug)]
/// map type for memory set: identical or framed
pub enum MapType {
    Identical,
    Framed,
}

bitflags! {
    /// map permission corresponding to that in pte: `R W X U`
    pub struct MapPermission: u8 {
        ///Readable
        const R = 1 << 1;
        ///Writable
        const W = 1 << 2;
        ///Excutable
        const X = 1 << 3;
        ///Accessible in U mode
        const U = 1 << 4;
    }
}

/// remap test in kernel space
#[allow(unused)]
pub fn remap_test() {
    let mut kernel_space = KERNEL_SPACE.exclusive_access();
    let mid_text: VirtAddr = ((stext as usize + etext as usize) / 2).into();
    let mid_rodata: VirtAddr = ((srodata as usize + erodata as usize) / 2).into();
    let mid_data: VirtAddr = ((sdata as usize + edata as usize) / 2).into();
    assert!(!kernel_space
        .page_table
        .translate(mid_text.floor())
        .unwrap()
        .writable(),);
    assert!(!kernel_space
        .page_table
        .translate(mid_rodata.floor())
        .unwrap()
        .writable(),);
    assert!(!kernel_space
        .page_table
        .translate(mid_data.floor())
        .unwrap()
        .executable(),);
    println!("remap_test passed!");
}
