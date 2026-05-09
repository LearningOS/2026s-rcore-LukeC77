//! Implementation of physical and virtual address and page number.
use super::PageTableEntry;
use crate::config::{PAGE_SIZE, PAGE_SIZE_BITS};
use core::fmt::{self, Debug, Formatter};
/// physical address
const PA_WIDTH_SV39: usize = 56;
const VA_WIDTH_SV39: usize = 39;
const PPN_WIDTH_SV39: usize = PA_WIDTH_SV39 - PAGE_SIZE_BITS;
const VPN_WIDTH_SV39: usize = VA_WIDTH_SV39 - PAGE_SIZE_BITS;

/// physical address
#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq)]
pub struct PhysAddr(pub usize);
/// virtual address
#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq)]
pub struct VirtAddr(pub usize);
/// physical page number
#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq)]
pub struct PhysPageNum(pub usize);
/// virtual page number
#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq)]
pub struct VirtPageNum(pub usize);

/// Debugging

impl Debug for VirtAddr {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_fmt(format_args!("VA:{:#x}", self.0))
    }
}
impl Debug for VirtPageNum {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_fmt(format_args!("VPN:{:#x}", self.0))
    }
}
impl Debug for PhysAddr {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_fmt(format_args!("PA:{:#x}", self.0))
    }
}
impl Debug for PhysPageNum {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_fmt(format_args!("PPN:{:#x}", self.0))
    }
}

/// T: {PhysAddr, VirtAddr, PhysPageNum, VirtPageNum}
/// T -> usize: T.0
/// usize -> T: usize.into()

impl From<usize> for PhysAddr {    // yifan 2026/5/4: 为 From trait 定义 usize -> PhysAddr 的转换规则，可用 PhysAddr::from(v) 或 v.into()。
    fn from(v: usize) -> Self {    // yifan 2026/5/4: 该转换总是可执行，v 不需要先被类型系统证明为完整合法物理地址。
        Self(v & ((1 << PA_WIDTH_SV39) - 1))    // yifan 2026/5/4: 仅保留低 PA_WIDTH_SV39 位；要语义正确，v 的这些低位本来就应是目标物理地址位模式。
    }
}
impl From<usize> for PhysPageNum {
    fn from(v: usize) -> Self {
        Self(v & ((1 << PPN_WIDTH_SV39) - 1))
    }
}
impl From<usize> for VirtAddr {
    fn from(v: usize) -> Self {
        Self(v & ((1 << VA_WIDTH_SV39) - 1))
    }
}
impl From<usize> for VirtPageNum {
    fn from(v: usize) -> Self {
        Self(v & ((1 << VPN_WIDTH_SV39) - 1))
    }
}
impl From<PhysAddr> for usize {    // yifan 2026/5/4: 为 From trait 定义 PhysAddr -> usize 的转换规则，可用 usize::from(pa) 或 pa.into()。
    fn from(v: PhysAddr) -> Self {
        v.0
    }
}
impl From<PhysPageNum> for usize {
    fn from(v: PhysPageNum) -> Self {
        v.0
    }
}
impl From<VirtAddr> for usize {    // yifan 2026/5/4: 将 VirtAddr 转为 usize 时需要恢复 Sv39 规范地址格式（高位做符号扩展）。
    fn from(v: VirtAddr) -> Self {    // yifan 2026/5/4: VirtAddr 仅保留低 VA_WIDTH_SV39 位，转换时要按最高有效位决定高位填充。
        if v.0 >= (1 << (VA_WIDTH_SV39 - 1)) {    // yifan 2026/5/4: 若第 38 位为 1，则地址属于高半区，需要把更高位全部补 1。
            v.0 | (!((1 << VA_WIDTH_SV39) - 1))    // yifan 2026/5/4: 对 39 位以上执行置 1，实现符号扩展，得到 canonical Sv39 虚拟地址。
        } else {
            v.0    // yifan 2026/5/4: 若第 38 位为 0，则高位保持 0，直接返回即可。
        }
    }
}
impl From<VirtPageNum> for usize {
    fn from(v: VirtPageNum) -> Self {
        v.0
    }
}
/// virtual address impl
impl VirtAddr {
    /// Get the (floor) virtual page number
    pub fn floor(&self) -> VirtPageNum {
        VirtPageNum(self.0 / PAGE_SIZE)
    }

    /// Get the (ceil) virtual page number
    pub fn ceil(&self) -> VirtPageNum {
        VirtPageNum((self.0 - 1 + PAGE_SIZE) / PAGE_SIZE)
    }

    /// Get the page offset of virtual address
    pub fn page_offset(&self) -> usize {
        self.0 & (PAGE_SIZE - 1)
    }

    /// Check if the virtual address is aligned by page size
    pub fn aligned(&self) -> bool {
        self.page_offset() == 0
    }
}
impl From<VirtAddr> for VirtPageNum {
    fn from(v: VirtAddr) -> Self {
        assert_eq!(v.page_offset(), 0);
        v.floor()
    }
}
impl From<VirtPageNum> for VirtAddr {
    fn from(v: VirtPageNum) -> Self {
        Self(v.0 << PAGE_SIZE_BITS)
    }
}
impl PhysAddr {
    // yifan 2026/5/4: PA=(PPN<<offset_bits)|offset 只是 CPU/OS 视角的物理地址编号计算；
    // PA 到 DRAM 的 channel/rank/bank/row/column 映射由内存控制器完成，架构规定地址与页表格式而不规定内存条内部结构。
    /// Get the (floor) physical page number
    pub fn floor(&self) -> PhysPageNum {    // yifan 2026/5/4: 将物理地址转换为其所在物理页号（向下取整）。
        PhysPageNum(self.0 / PAGE_SIZE)    // yifan 2026/5/4: 用地址按页大小做整数除法，去掉页内偏移后得到页号, 也就是右移 PAGE_SIZE_BITS 位。
    }
    // yifan 2026/5/4: 为什么需要 ceil：内核常把任意起止地址区间 [start, end) 转成页号区间处理，起点用 floor，终点用 ceil，避免漏掉最后一页。
    // yifan 2026/5/4: 例：PAGE_SIZE=0x1000, start=0x1003, end=0x2001；start.floor()=1，end.floor()=2 只得 [1,2) 会漏第2页；end.ceil()=3 得 [1,3) 才覆盖第1/2页。
    // yifan 2026/5/4: 常见于映射/解映射、释放页、用户缓冲区跨页检查、ELF 段装载等按页操作。
    // yifan 2026/5/4: floor/ceil 不是给同一个 pa 构造必非空区间；floor(pa) 是所在页，ceil(pa) 是把 pa 当区间上界地址时的上界页号。
    // yifan 2026/5/4: 当 pa=0x1000（页边界）时 floor=ceil=1，表示上界正好落边界无需多算一页；[1,1) 代表长度为0的空区间，是合法且常见语义。
    /// Get the (ceil) physical page number
    pub fn ceil(&self) -> PhysPageNum {
        PhysPageNum((self.0 - 1 + PAGE_SIZE) / PAGE_SIZE)    // yifan 2026/5/4: 对地址上取整到页边界后再取页号，等价于 ceil(self.0 / PAGE_SIZE)。
    }
    /// Get the page offset of physical address
    pub fn page_offset(&self) -> usize {
        self.0 & (PAGE_SIZE - 1)
    }
    // yifan 2026/5/5: aligned() 用来区分“普通字节地址”与“页起始地址”，本质是检查页内 offset 是否为 0。
    // yifan 2026/5/5: 内核中的页表映射/解映射、物理页分配回收、PhysAddr->PhysPageNum 转换等按整页操作都要求页对齐。
    // yifan 2026/5/5: 因此它常作为前置校验：true 才能当页基址使用，false 则需先 floor/ceil 或报错，避免把页内地址误当页起点。
    /// Check if the physical address is aligned by page size
    pub fn aligned(&self) -> bool {
        self.page_offset() == 0
    }
}
impl From<PhysAddr> for PhysPageNum {
    fn from(v: PhysAddr) -> Self {
        assert_eq!(v.page_offset(), 0);
        v.floor()
    }
}
impl From<PhysPageNum> for PhysAddr {
    fn from(v: PhysPageNum) -> Self {
        Self(v.0 << PAGE_SIZE_BITS)
    }
}

impl VirtPageNum {
    /// Get the indexes of the page table entry
    pub fn indexes(&self) -> [usize; 3] {    /* yifan 2026/5/6: 将 VirtPageNum 拆分为 SV39 三级页表索引并返回 [usize; 3]。 */
        let mut vpn = self.0;    /* yifan 2026/5/6: 取出页号整数；VirtPageNum 只含页号，不包含低 12 位页内 offset。 */
        let mut idx = [0usize; 3];    /* yifan 2026/5/6: 初始化三级索引数组，后续填充为 [vpn2, vpn1, vpn0]。 */
        for i in (0..3).rev() {    /* yifan 2026/5/6: 逆序遍历 2->1->0，每次提取当前最低 9 位索引。 */
            idx[i] = vpn & 511;    /* yifan 2026/5/6: 用掩码 0x1FF(511) 取出最低 9 位作为该级页表索引。 */
            vpn >>= 9;    /* yifan 2026/5/6: 右移 9 位，准备提取下一层索引。 */
        }
        idx    /* yifan 2026/5/6: 返回最终页表索引数组。 */
    }
}

impl PhysAddr {
    ///Get mutable reference to `PhysAddr` value
    /// Get the mutable reference of physical address
    pub fn get_mut<T>(&self) -> &'static mut T {
        unsafe { (self.0 as *mut T).as_mut().unwrap() }
    }
}
impl PhysPageNum {    /* yifan 2026/5/6: 为 PhysPageNum 实现关联方法，提供从物理页号到不同内存视图的访问接口。 */
    /// Get the reference of page table(array of ptes)    
    pub fn get_pte_array(&self) -> &'static mut [PageTableEntry] {    /* yifan 2026/5/6: 定义公开方法，输入 &self，返回该页对应的可变 PageTableEntry 切片。 */
        let pa: PhysAddr = (*self).into();    /* yifan 2026/5/6: 通过 Into 转换把物理页号变成物理地址（通常是页起始地址）。 */
        unsafe { core::slice::from_raw_parts_mut(pa.0 as *mut PageTableEntry, 512) }    /* yifan 2026/5/6: 在 unsafe 中用裸指针+长度构造可变切片；地址转为 *mut PageTableEntry，长度 512（4KiB/8B）。 */
    }
    /// Get the reference of page(array of bytes)    
    pub fn get_bytes_array(&self) -> &'static mut [u8] {    /* yifan 2026/5/6: 定义公开方法，返回该物理页对应的可变字节切片。 */
        let pa: PhysAddr = (*self).into();    /* yifan 2026/5/6: 同样先把 PhysPageNum 转成 PhysAddr。 */
        unsafe { core::slice::from_raw_parts_mut(pa.0 as *mut u8, 4096) }    /* yifan 2026/5/6: 在 unsafe 中把页起始地址解释为 *mut u8，并构造长度 4096 的整页字节切片。 */
    }
    /// Get the mutable reference of physical address    
    pub fn get_mut<T>(&self) -> &'static mut T {    /* yifan 2026/5/6: 定义泛型公开方法，将该页起始地址按类型 T 解释并返回可变引用。 */
        let pa: PhysAddr = (*self).into();    /* yifan 2026/5/6: 先将物理页号转换为物理地址。 */
        pa.get_mut()    /* yifan 2026/5/6: 复用 PhysAddr::get_mut<T>() 实现完成地址到 &mut T 的转换，避免重复实现。 */
    }
}

/// iterator for phy/virt page number
pub trait StepByOne {
    /// step by one element(page number)
    fn step(&mut self);
}
impl StepByOne for VirtPageNum {
    fn step(&mut self) {
        self.0 += 1;
    }
}

#[derive(Copy, Clone)]
/// a simple range structure for type T
pub struct SimpleRange<T>
where
    T: StepByOne + Copy + PartialEq + PartialOrd + Debug,
{
    l: T,
    r: T,
}
impl<T> SimpleRange<T>
where
    T: StepByOne + Copy + PartialEq + PartialOrd + Debug,
{
    pub fn new(start: T, end: T) -> Self {
        assert!(start <= end, "start {:?} > end {:?}!", start, end);
        Self { l: start, r: end }
    }
    pub fn get_start(&self) -> T {
        self.l
    }
    pub fn get_end(&self) -> T {
        self.r
    }
}
impl<T> IntoIterator for SimpleRange<T>
where
    T: StepByOne + Copy + PartialEq + PartialOrd + Debug,
{
    type Item = T;
    type IntoIter = SimpleRangeIterator<T>;
    fn into_iter(self) -> Self::IntoIter {
        SimpleRangeIterator::new(self.l, self.r)
    }
}
/// iterator for the simple range structure
pub struct SimpleRangeIterator<T>
where
    T: StepByOne + Copy + PartialEq + PartialOrd + Debug,
{
    current: T,
    end: T,
}
impl<T> SimpleRangeIterator<T>
where
    T: StepByOne + Copy + PartialEq + PartialOrd + Debug,
{
    pub fn new(l: T, r: T) -> Self {
        Self { current: l, end: r }
    }
}
impl<T> Iterator for SimpleRangeIterator<T>
where
    T: StepByOne + Copy + PartialEq + PartialOrd + Debug,
{
    type Item = T;
    fn next(&mut self) -> Option<Self::Item> {
        if self.current == self.end {
            None
        } else {
            let t = self.current;
            self.current.step();
            Some(t)
        }
    }
}
/// a simple range structure for virtual page number
pub type VPNRange = SimpleRange<VirtPageNum>;
