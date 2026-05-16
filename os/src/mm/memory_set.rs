//! Implementation of [`MapArea`] and [`MemorySet`].

use super::{frame_alloc, FrameTracker};
use super::{PTEFlags, PageTable, PageTableEntry};
use super::{PhysAddr, PhysPageNum, VirtAddr, VirtPageNum};
use super::{StepByOne, VPNRange};
use crate::config::{
    KERNEL_STACK_SIZE, MEMORY_END, PAGE_SIZE, TRAMPOLINE, TRAP_CONTEXT_BASE, USER_STACK_SIZE,
};
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
    fn push(&mut self, mut map_area: MapArea, data: Option<&[u8]>) {    /* yifan 2026/5/6: 定义私有方法；&mut self 表示可变借用当前 MemorySet，mut map_area 允许在函数内修改，data: Option<&[u8]> 表示可选初始化字节数据。 */
        map_area.map(&mut self.page_table);    /* yifan 2026/5/6: 调用 MapArea::map 在页表中建立该虚拟地址区间映射，传入 &mut page_table 因为会修改页表。 */
        if let Some(data) = data {    /* yifan 2026/5/6: 使用 if let 模式匹配 Option；仅当 data 为 Some(...) 时进入分支并绑定内部切片。 */
            map_area.copy_data(&mut self.page_table, data);    /* yifan 2026/5/6: 将初始化数据拷贝到刚映射的区域中（常用于装载程序段内容）。 */
        }
        self.areas.push(map_area);    /* yifan 2026/5/6: 将 MapArea 追加到 areas 向量做元数据记录，便于后续管理这段映射。 */
    }
    fn map_trampoline(&mut self) {    // yifan 2026/5/7: 在当前 MemorySet 的页表中建立 trampoline 映射，保证该地址空间可通过固定入口访问跳板代码。
        self.page_table.map(    // yifan 2026/5/7: 调用页表 map 接口写入一条 VA->PA 映射。
            VirtAddr::from(TRAMPOLINE).into(),    // yifan 2026/5/7: 将固定虚拟地址 TRAMPOLINE 转为虚拟页号，作为 trap 入口统一 VA。
            PhysAddr::from(strampoline as usize).into(),    // yifan 2026/5/7: 将 strampoline 符号所在地址转为物理页号，指向实际跳板代码页。
            PTEFlags::R | PTEFlags::X,    // yifan 2026/5/7: 页表权限设为可读+可执行，允许取指执行但不允许写入。
        );    // yifan 2026/5/7: 这样无论当前使用用户页表还是内核页表，都能在同一 TRAMPOLINE 虚拟地址取到跳板代码。
    }
    /// Without kernel stacks.
    pub fn new_kernel() -> Self {
        let mut memory_set = Self::new_bare();                       // yifan 2026/5/7: 创建空地址空间（空页表与空区域列表），作为内核映射初始化起点。
        memory_set.map_trampoline();                                            // yifan 2026/5/7: 先映射 trampoline 到固定 TRAMPOLINE 虚拟地址，保证 trap 进出路径可用。
        info!(".text [{:#x}, {:#x})", stext as usize, etext as usize);          // yifan 2026/5/7: 打印 .text 段地址范围，便于核对内核代码段布局。
        info!(".rodata [{:#x}, {:#x})", srodata as usize, erodata as usize);    // yifan 2026/5/7: 打印 .rodata 段地址范围，便于核对只读数据段布局。
        info!(".data [{:#x}, {:#x})", sdata as usize, edata as usize);          // yifan 2026/5/7: 打印 .data 段地址范围，便于核对可写数据段布局。
        info!(                                                                  // yifan 2026/5/7: 打印 .bss 段地址范围，这里仅记录日志，真正映射在后续 push 中完成。
            ".bss [{:#x}, {:#x})",
            sbss_with_stack as usize, ebss as usize
        );
        info!("mapping .text section");
        memory_set.push(                                                        // yifan 2026/5/7: 将一个映射区域加入内核地址空间，并同步写入页表。
            MapArea::new(                                                       // yifan 2026/5/7: 创建描述内核 .text 段的 MapArea。
                (stext as usize).into(),                               // yifan 2026/5/7: 以 stext 作为映射起始虚拟地址。
                (etext as usize).into(),                                 // yifan 2026/5/7: 以 etext 作为映射结束虚拟地址。
                MapType::Identical,                                             // yifan 2026/5/7: 使用恒等映射，使该段 VA 与 PA 按页号一一对应。
                MapPermission::R | MapPermission::X,                   // yifan 2026/5/7: 权限设为可读可执行，符合代码段不可写的要求。
            ),
            None,                                                          // yifan 2026/5/7: 不附带初始化数据拷贝；该段内容由已装载内核镜像提供。
        );                                                                      // yifan 2026/5/7: 完成 .text 段映射后返回，继续处理后续内核段。
        info!("mapping .rodata section");                                       // yifan 2026/5/7: 输出日志，表示下一步开始映射 .rodata 段。
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
        let mut memory_set = Self::new_bare();                             // yifan 2026/5/7: 为“当前应用/进程”新建独立用户地址空间（空页表+空区域），不是所有应用共用，也不是与内核共用。
        memory_set.map_trampoline();                                                  // yifan 2026/5/7: 先在该用户地址空间映射 trampoline，保证 trap 进出时在固定 VA 可取到跳板代码。
        let elf = xmas_elf::ElfFile::new(elf_data).unwrap();                         // yifan 2026/5/7: 用 xmas_elf crate 解析 ELF 字节；该 crate 来自 Cargo.toml 依赖，使用绝对路径调用无需额外 use。
        let elf_header = elf.header;                                                  // yifan 2026/5/7: 读取 ELF 文件头元数据（如魔数、位宽/端序、程序头信息与入口地址等）。
        let magic = elf_header.pt1.magic;                                             // yifan 2026/5/7: 取出魔数字段用于格式校验；这属于标准文件签名检查，不是“提前看答案”。
        assert_eq!(magic, [0x7f, 0x45, 0x4c, 0x46], "invalid elf!");            // yifan 2026/5/7: 验证 ELF 魔数是否正确（0x7F 'E' 'L' 'F'），确保输入数据确实是 ELF 格式；否则 panic 并输出错误信息。 ELF 文件的前 4 个字节固定是：0x7f  0x45  0x4c  0x46
        let ph_count = elf_header.pt2.ph_count();                                     // yifan 2026/5/7: 读取的是“当前 ELF 的 Program Header 表项数量”，不是应用/进程数量；每个表项(Phdr)描述一个运行时段的装载信息。
        let mut max_end_vpn = VirtPageNum(0);                      // yifan 2026/5/7: 以 0 作为最大结束 VPN 的初始哨兵值；遍历各 LOAD 段时会被真实结束页号更新。
        for i in 0..ph_count {
            let ph = elf.program_header(i).unwrap();                                   // yifan 2026/5/7: 取第 i 个 Program Header 表项；每项含类型、offset、vaddr、filesz、memsz、R/W/X、align 等字段。
            if ph.get_type().unwrap() == xmas_elf::program::Type::Load {              // yifan 2026/5/7: 仅处理可装载段 PT_LOAD，按表项描述将其映射/拷贝到对应虚拟地址。
                let start_va: VirtAddr = (ph.virtual_addr() as usize).into();           // yifan 2026/5/7: 从 Program Header 读取该段起始虚拟地址，作为映射区间左边界。
                let end_va: VirtAddr = ((ph.virtual_addr() + ph.mem_size()) as usize).into();    // yifan 2026/5/7: 结束虚拟地址=起始地址+mem_size（按内存大小而非文件大小确定区间右边界）。
                let mut map_perm = MapPermission::U;                                     // yifan 2026/5/7: 先赋予用户态访问标志 U，后续再叠加 R/W/X 权限。
                let ph_flags = ph.flags();                                               // yifan 2026/5/7: 读取该段 ELF 权限标志，后续据此把读写执行权限映射到页表权限位。
                if ph_flags.is_read() {
                    map_perm |= MapPermission::R;
                }
                if ph_flags.is_write() {
                    map_perm |= MapPermission::W;
                }
                if ph_flags.is_execute() {
                    map_perm |= MapPermission::X;
                }
                let map_area = MapArea::new(start_va, end_va, MapType::Framed, map_perm);    // yifan 2026/5/7: 创建该 ELF 段的映射区域对象：区间为 start_va..end_va，采用 Framed 分配独立物理页帧，权限使用前面汇总的 U/R/W/X。
                max_end_vpn = map_area.vpn_range.get_end();                           // yifan 2026/5/7: 记录该段映射后的页级结束位置（VPN 上界）；这里不用字节级 end_va，是因为后续分配/映射按页进行，vpn_range 已统一做过页边界处理，能避免重复 ceil 转换与边界错误。
                memory_set.push(                                                      // yifan 2026/5/7: 将该段映射区域加入当前地址空间，并在 push 内实际建立页表映射。
                    map_area,                                                         // yifan 2026/5/7: 使用前面构造好的 map_area（已包含地址范围、映射类型与权限）。
                    Some(&elf.input[ph.offset() as usize..(ph.offset() + ph.file_size()) as usize]),    // yifan 2026/5/7: 提供该段在 ELF 文件中的字节切片（offset..offset+file_size），用于把段内容拷贝到映射后内存。
                );
            }
        }
        // map user stack with U flags
        let max_end_va: VirtAddr = max_end_vpn.into();                          // yifan 2026/5/8: 将最后一个已映射页号上界 max_end_vpn 转为虚拟地址；max_end_va 表示 ELF 各段加载结束后的第一个空闲地址，ELF 已占用区间是 [start, max_end_va)。
        let mut user_stack_bottom: usize = max_end_va.into();                   // yifan 2026/5/8: 先以 max_end_va 作为用户栈底候选地址（转成 usize 便于做地址加法）；注意这是“栈可用区间下界候选”，不是当前已使用栈内容。
        // guard page                                                           // yifan 2026/5/8: 在 ELF 与用户栈之间预留一页不映射的保护页，防止栈向低地址溢出时覆盖 ELF 数据，并通过缺页异常及时暴露越界访问。
        user_stack_bottom += PAGE_SIZE;                                         // yifan 2026/5/8: 栈底上移一页，跳过 [max_end_va, max_end_va + PAGE_SIZE) 作为 guard page；真正可用栈底变为 max_end_va + PAGE_SIZE。
        let user_stack_top = user_stack_bottom + USER_STACK_SIZE;        // yifan 2026/5/8: 计算用户栈可用区间上界，栈区间为 [user_stack_bottom, user_stack_top)；初始 sp 置为 user_stack_top，后续压栈时 sp 向低地址移动。
        memory_set.push(                                                        // yifan 2026/5/8: 将用户栈对应的虚拟内存区域加入当前地址空间，并在 push 内建立该区域的页表映射。
            MapArea::new(                                                       // yifan 2026/5/8: 构造用户栈映射区域描述对象，下面依次给出起止地址、映射类型和访问权限。
                user_stack_bottom.into(),                              // yifan 2026/5/8: 映射起始地址（栈底，低地址端）；用户栈区间下界。
                user_stack_top.into(),                                   // yifan 2026/5/8: 映射结束地址（栈顶上界，高地址端）；与起始地址共同定义半开区间 [user_stack_bottom, user_stack_top)。
                MapType::Framed,                                                // yifan 2026/5/8: 使用 Framed 映射，为该虚拟区域逐页分配独立物理页帧，而非线性恒等映射。
                MapPermission::R | MapPermission::W | MapPermission::U,     // yifan 2026/5/8: 栈页权限为用户态可读可写（U+R+W）；不含 X，因此不可执行，降低在栈上执行代码的风险。
            ),
            None,                                                          // yifan 2026/5/8: 该区域不从 ELF 文件拷贝初始化数据，仅完成分配与映射；实际栈内容在运行时由程序压栈产生。
        );
        // used in sbrk                                                           // yifan 2026/5/8: 这一段用于给 sbrk 预置“堆区域锚点”，后续由 sbrk 通过 append_to/shrink_to 动态调整 program break。
        memory_set.push(                                                          // yifan 2026/5/8: 将一个新的内存区域描述注册到当前地址空间；该区域专门作为用户堆（heap）的管理对象。
            MapArea::new(                                                         // yifan 2026/5/8: 构造堆区域描述符，下面依次指定区间起止、映射类型和页权限。
                user_stack_top.into(),                                            // yifan 2026/5/8: 堆起始地址取 user_stack_top，使堆从用户栈上边界开始向更高地址扩展。
                user_stack_top.into(),                                            // yifan 2026/5/8: 结束地址与起始地址相同，形成 [start, end) 的 0 长度区间；初始不映射任何堆页，但“起点元数据”已建立。
                MapType::Framed,                                                  // yifan 2026/5/8: 使用 Framed 映射策略，后续堆扩展时按页分配独立物理页帧并建立映射。
                MapPermission::R | MapPermission::W | MapPermission::U,           // yifan 2026/5/8: 堆页权限为用户态可读可写（U+R+W）；不含执行权限，符合常规堆内存语义与安全预期。
            ),
            None,                                                                 // yifan 2026/5/8: 不从 ELF 拷贝初始化数据；该堆区内容由运行时分配/写入逐步产生。
        );
        // map TrapContext                                                         // yifan 2026/5/8: 映射当前进程的 TrapContext 区域；该区域属于用户地址空间的一部分，但仅供内核在 trap 进出时保存/恢复寄存器现场使用。
        memory_set.push(                                                           // yifan 2026/5/8: 将 TrapContext 对应虚拟内存区域加入当前进程 memory_set；若不映射，trap 入口/返回读写 TrapContext 时会因页表缺项触发页故障。
            MapArea::new(                                                          // yifan 2026/5/8: 构造该区域的映射描述对象；from_elf 内 user stack->sbrk->trapContext 的添加顺序主要是构建流程与可读性，通常不构成功能性硬要求。
                TRAP_CONTEXT_BASE.into(),                                          // yifan 2026/5/8: 区间起点为 TRAP_CONTEXT_BASE（trampoline 下方固定高地址），用于每个进程独立放置自身 TrapContext。
                TRAMPOLINE.into(),                                                 // yifan 2026/5/8: 区间终点为 TRAMPOLINE，故映射范围是 [TRAP_CONTEXT_BASE, TRAMPOLINE)；需保证与其他区域不冲突且最终与 trampoline 同时存在。
                MapType::Framed,                                                   // yifan 2026/5/8: 采用 Framed 映射，为该区间分配独立物理页帧，保证每个进程拥有隔离的 TrapContext 存储。
                MapPermission::R | MapPermission::W,                               // yifan 2026/5/8: 仅 R/W 且不含 U，用户态不可直接访问；这是内核私有上下文页而非用户可读写数据页。
            ),
            None,                                                                  // yifan 2026/5/8: 不从 ELF 拷贝初始化内容；TrapContext 的实际内容在运行时由内核写入。
        );
        (
            memory_set,                                                            // yifan 2026/5/8: 返回已构建完成的用户地址空间对象（含页表与各映射区域：ELF 段、用户栈、sbrk 堆锚点、TrapContext、trampoline）。
            user_stack_top,                                                        // yifan 2026/5/8: 返回用户栈顶地址，作为用户态初始 sp；后续初始化 TrapContext 时会把它写入用户栈指针寄存器。
            elf.header.pt2.entry_point() as usize,                                 // yifan 2026/5/8: 返回 ELF 入口虚拟地址（首条用户指令地址）；后续写入 TrapContext 的 sepc/pc，使进程从该入口开始执行。
        )
    }
    /// Change page table by writing satp CSR Register.
    pub fn activate(&self) {
        let satp = self.page_table.token();
        unsafe {
            satp::write(satp);
            asm!("sfence.vma");  // yifan 2026/5/9 切换地址空间后执行 sfence.vma 刷新 TLB，确保后续内存访问使用新页表；这是 RISC-V 架构要求的地址空间切换流程。
        }
    }
    /// Translate a virtual page number to a page table entry
    pub fn translate(&self, vpn: VirtPageNum) -> Option<PageTableEntry> {
        self.page_table.translate(vpn)
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
/// MapArea 是一段虚拟页区间；data_frames 就是在这种区间里记录“该区间内每个 VPN 对应的物理页帧（可得到 PPN）”。
/// 通常只对 Framed 类型需要这张表；Identical 映射一般不需要逐页分配并记录 data_frames。
pub struct MapArea {
    vpn_range: VPNRange,    /* yifan 2026/5/6: VPNRange 是类型别名，实际为 SimpleRange<VirtPageNum>，表示一段连续的虚拟页号范围（左闭右开，迭代到 end 停止）。 */
    data_frames: BTreeMap<VirtPageNum, FrameTracker>,    /* yifan 2026/5/6: BTreeMap 是基于 B-Tree 的有序键值结构，按键有序存储并支持 O(log n) 查找/插入/删除；这里用于按 VirtPageNum 有序管理映射到的物理页帧。 MapArea 持有 FrameTracker，在 MapArea 回收/销毁时自动 drop，触发页帧归还给 frame allocator。顺带也能按 vpn 快速定位对应帧。 */
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
    pub fn map_one(&mut self, page_table: &mut PageTable, vpn: VirtPageNum) {    /* yifan 2026/5/6: 为单个虚拟页 vpn 建立映射，最终写入 vpn -> ppn。 */
        let ppn: PhysPageNum;    /* yifan 2026/5/6: 先声明目标物理页号，后续按映射类型决定其取值。 */
        match self.map_type {    /* yifan 2026/5/6: 根据 MapArea 的映射类型选择 ppn 来源。 */
            MapType::Identical => {    /* yifan 2026/5/6: 恒等映射：虚拟页号与物理页号相同。 */
                ppn = PhysPageNum(vpn.0);    /* yifan 2026/5/6: 直接用 vpn 内部数值构造对应 ppn。 */
            }
            MapType::Framed => {    /* yifan 2026/5/6: 分帧映射：为该虚页新分配一个物理页帧。 */
                let frame = frame_alloc().unwrap();    /* yifan 2026/5/6: 从帧分配器申请页帧，失败则 panic。 */
                ppn = frame.ppn;    /* yifan 2026/5/6: 取新分配页帧的物理页号作为映射目标。 */
                self.data_frames.insert(vpn, frame);    /* yifan 2026/5/6: 记录 vpn->FrameTracker，持有生命周期以便后续自动回收。 */
            }
        }    /* yifan 2026/5/6: 映射类型分支结束，此时 ppn 已确定。 */
        let pte_flags = PTEFlags::from_bits(self.map_perm.bits).unwrap();    /* yifan 2026/5/6: 将 MapArea 权限位转换为页表项权限标志。 */
        page_table.map(vpn, ppn, pte_flags);    /* yifan 2026/5/6: 写入页表项，建立 vpn 到 ppn 的映射并设置权限。 */
    }
    #[allow(unused)]
    pub fn unmap_one(&mut self, page_table: &mut PageTable, vpn: VirtPageNum) {    /* yifan 2026/5/6: 取消单个虚拟页 vpn 的映射关系。 */
        if self.map_type == MapType::Framed {    /* yifan 2026/5/6: 仅 Framed 映射拥有通过 frame_alloc 分配的页帧，需要在取消映射时回收。 */
            self.data_frames.remove(&vpn);    /* yifan 2026/5/6: 删除 vpn 对应 FrameTracker；其 drop 会触发物理页帧回收。 */
        }    /* yifan 2026/5/6: Identical 映射不拥有页帧，不做 data_frames 删除。 */
        page_table.unmap(vpn);    /* yifan 2026/5/6: 从页表删除 vpn->ppn 映射，防止后续继续通过该虚页访问原物理页。 */
    }
    // yifan 2026/5/6: 该函数按 vpn_range 逐页调用 map_one 建立映射；map_one 会按 map_type 分支执行恒等映射或分配新物理页帧后再映射，并在 Framed 情况记录到 data_frames。
    pub fn map(&mut self, page_table: &mut PageTable) {
        for vpn in self.vpn_range {
            self.map_one(page_table, vpn);
        }
    }
    #[allow(unused)]
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
    pub fn copy_data(&mut self, page_table: &mut PageTable, data: &[u8]) {    /* yifan 2026/5/6: 将连续字节数据按页复制到该 MapArea 对应的物理页帧（常用于装载 ELF 的 .text/.data 段）。 */
        assert_eq!(self.map_type, MapType::Framed);    /* yifan 2026/5/6: 要求必须是 Framed 映射，确保每个虚页对应可写入的独立物理页帧。 */
        let mut start: usize = 0;    /* yifan 2026/5/6: 源数据偏移，表示当前从 data[start] 开始复制。 */
        let mut current_vpn = self.vpn_range.get_start();    /* yifan 2026/5/6: 当前写入虚拟页号，从该区域起始 VPN 开始。 */
        let len = data.len();    /* yifan 2026/5/6: 需要复制的数据总字节数。 */
        loop {    /* yifan 2026/5/6: 逐页循环复制，直到 start 覆盖到 len。 */
            let src = &data[start..len.min(start + PAGE_SIZE)];    /* yifan 2026/5/6: 取本轮最多一页的源片段；最后一页可能不足 PAGE_SIZE。 len.min(...) — 取 len 和 start + PAGE_SIZE 中的较小值，防止越界*/
            let dst = &mut page_table    /* yifan 2026/5/6: 通过页表把 current_vpn 翻译到物理页并拿到目标字节切片。 */
                .translate(current_vpn)    /* yifan 2026/5/6: 查找当前虚页对应页表项。 */
                .unwrap()    /* yifan 2026/5/6: 假定该虚页映射存在；不存在则 panic。 */
                .ppn()    /* yifan 2026/5/6: 从页表项提取物理页号。 */
                .get_bytes_array()[..src.len()];    /* yifan 2026/5/6: 取目标物理页前 src.len() 字节，保证与源片段等长。 */
            dst.copy_from_slice(src);    /* yifan 2026/5/6: 执行字节拷贝：src -> 当前物理页帧。 */
            start += PAGE_SIZE;    /* yifan 2026/5/6: 源偏移推进一页。 */
            if start >= len {    /* yifan 2026/5/6: 若全部数据已复制完成则退出循环。 */
                break;    /* yifan 2026/5/6: 结束复制。 */
            }
            current_vpn.step();    /* yifan 2026/5/6: 切换到下一个虚拟页继续复制。 */
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

/// Return (bottom, top) of a kernel stack in kernel space.
pub fn kernel_stack_position(app_id: usize) -> (usize, usize) {
    let top = TRAMPOLINE - app_id * (KERNEL_STACK_SIZE + PAGE_SIZE);    // yifan 2026/5/10: 这里不是全系统唯一内核栈，而是每个应用/任务按 app_id 分配独立内核栈；从 TRAMPOLINE 向下排布，每个槽位间距为“栈大小+一页保护页”。
    let bottom = top - KERNEL_STACK_SIZE;    // yifan 2026/5/10: 栈区间为 [bottom, top)；额外预留的 PAGE_SIZE 作为 guard page（通常不映射），用于栈溢出保护，避免踩到相邻任务内核栈。
    (bottom, top)
}

/// remap test in kernel space
#[allow(unused)]
pub fn remap_test() {    // yifan 2026/5/9: 定义内核地址空间重映射检查函数，用于验证关键段权限是否正确。
    let mut kernel_space = KERNEL_SPACE.exclusive_access();    // yifan 2026/5/9: 获取 KERNEL_SPACE 的独占访问句柄，后续需要读取其页表项权限。
    let mid_text: VirtAddr = ((stext as usize + etext as usize) / 2).into();    // yifan 2026/5/9: 取 .text 段中点虚拟地址，作为代码段权限检查样本点。
    let mid_rodata: VirtAddr = ((srodata as usize + erodata as usize) / 2).into();    // yifan 2026/5/9: 取 .rodata 段中点虚拟地址，作为只读数据段权限检查样本点。
    let mid_data: VirtAddr = ((sdata as usize + edata as usize) / 2).into();    // yifan 2026/5/9: 取 .data 段中点虚拟地址，作为可写数据段权限检查样本点。
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
