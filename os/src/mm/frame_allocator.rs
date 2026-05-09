//! Implementation of [`FrameAllocator`] which
//! controls all the frames in the operating system.

use super::{PhysAddr, PhysPageNum};
use crate::config::MEMORY_END;
use crate::sync::UPSafeCell;
use alloc::vec::Vec;
use core::fmt::{self, Debug, Formatter};
use lazy_static::*;

/// tracker for physical page frame allocation and deallocation
pub struct FrameTracker {
    /// physical page number
    pub ppn: PhysPageNum,
}

impl FrameTracker {
    /// Create a new FrameTracker
    pub fn new(ppn: PhysPageNum) -> Self {
        // page cleaning
        let bytes_array = ppn.get_bytes_array();    /* yifan 2026/5/6: 根据 PhysPageNum 获取该物理页的可写字节视图（整页切片），便于直接读写整页内容。 */
        for i in bytes_array {    /* yifan 2026/5/6: 遍历该页每个字节执行页清零，确保分配出的页是干净的。 */
            *i = 0;    /* yifan 2026/5/6: 将当前字节置 0，避免旧数据泄露并保证后续使用正确性。 */
        }
        Self { ppn }
    }
}

impl Debug for FrameTracker {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_fmt(format_args!("FrameTracker:PPN={:#x}", self.ppn.0))
    }
}

impl Drop for FrameTracker {
    fn drop(&mut self) {
        frame_dealloc(self.ppn);
    }
}

trait FrameAllocator {
    fn new() -> Self;
    fn alloc(&mut self) -> Option<PhysPageNum>;
    fn dealloc(&mut self, ppn: PhysPageNum);
}
/// an implementation for frame allocator
pub struct StackFrameAllocator {
    current: usize, //空闲内存的起始物理页号
    end: usize, //空闲内存的结束物理页号
    recycled: Vec<usize>, // 保存了被回收的物理页号
}

impl StackFrameAllocator {
    pub fn init(&mut self, l: PhysPageNum, r: PhysPageNum) {
        self.current = l.0;
        self.end = r.0;
        // trace!("last {} Physical Frames.", self.end - self.current);
    }
}
impl FrameAllocator for StackFrameAllocator {
    fn new() -> Self {
        Self {
            current: 0,
            end: 0,
            recycled: Vec::new(),    /* yifan 2026/5/6: no_std 仍可用 alloc；工程在 Cargo.toml 引入 buddy_system_allocator 并通过 #[global_allocator] 提供全局堆分配器，因此这里可创建 Vec（且 Vec::new 本身不立即分配堆内存）。 */
        }
    }
    fn alloc(&mut self) -> Option<PhysPageNum> {
        if let Some(ppn) = self.recycled.pop() {
            Some(ppn.into()) // yifan 2026/5/6: 首先会检查栈 recycled 内有没有之前回收的物理页号，如果有的话直接弹出栈顶并返回。使用 into 方法将 usize 转换成了物理页号 PhysPageNum
        } else if self.current == self.end {
            None
        } else {
            self.current += 1;
            Some((self.current - 1).into()) // yifan 2026/5/6: 使用 into 方法将 usize 转换成了物理页号 PhysPageNum
        }
    }
    fn dealloc(&mut self, ppn: PhysPageNum) {
        let ppn = ppn.0;
        // validity check
        /* yifan 2026/5/6:
        回收页面合法有两个条件：
            该页面之前一定被分配出去过，因此它的物理页号一定 < self.current ；
            该页面没有正处在回收状态，即它的物理页号不能在栈 recycled 中找到。
         */
        if ppn >= self.current || self.recycled.iter().any(|&v| v == ppn) {
            panic!("Frame ppn={:#x} has not been allocated!", ppn);
        }
        // recycle
        self.recycled.push(ppn);
    }
}

type FrameAllocatorImpl = StackFrameAllocator;    /* yifan 2026/5/6: 用类型别名集中“当前帧分配器实现”的开关点；若后续有 BitmapFrameAllocator 且接口一致，只需改这一行，其他使用 FrameAllocatorImpl 的代码基本不用改。 */

lazy_static! {    /* yifan 2026/5/6: 使用 lazy_static 定义延迟初始化的全局静态对象，首次访问时再构造。 */
    /// frame allocator instance through lazy_static!    /* yifan 2026/5/6: 说明下面定义的是全局帧分配器实例。 */
    pub static ref FRAME_ALLOCATOR: UPSafeCell<FrameAllocatorImpl> =    /* yifan 2026/5/6: 定义全局 FRAME_ALLOCATOR，并用 UPSafeCell 包装以支持单核下安全可变访问。 */
        unsafe { UPSafeCell::new(FrameAllocatorImpl::new()) };    /* yifan 2026/5/6: 创建默认分配器并放入 UPSafeCell；unsafe 由调用者保证其使用满足单核互斥约束。 */
}
/// initiate the frame allocator using `ekernel` and `MEMORY_END`
/// 这里我们调用物理地址 PhysAddr 的 floor/ceil 方法分别下/上取整获得可用的物理页号区间。
pub fn init_frame_allocator() {
    extern "C" {
        fn ekernel();
    }
    FRAME_ALLOCATOR.exclusive_access().init(    /* yifan 2026/5/6: 获取全局帧分配器 FrameAllocatorImpl 的独占访问权限，并调用 init 初始化可用物理页号区间。 */
        PhysAddr::from(ekernel as usize).ceil(),    /* yifan 2026/5/6: current 起点上取整到页号；ekernel 若落在页中间，该页已含内核内容，必须从下一整页开始分配。 */
        PhysAddr::from(MEMORY_END).floor(),    /* yifan 2026/5/6: end 终点下取整到页号；分配区间是 [current, end)，只纳入完整可用页，避免越过 MEMORY_END 的残页。 */
    );
}

/// Allocate a physical page frame in FrameTracker style
pub fn frame_alloc() -> Option<FrameTracker> {
    FRAME_ALLOCATOR
        .exclusive_access()
        .alloc()
        .map(FrameTracker::new)    /* yifan 2026/5/6: Option::map 在 Some(x) 时返回 Some(f(x))，在 None 时原样返回 None（不调用 f）；这里将 Option<PhysPageNum> 转为 Option<FrameTracker>，分配失败保持 None 而不是 panic。 */
}

/// Deallocate a physical page frame with a given ppn
pub fn frame_dealloc(ppn: PhysPageNum) {
    FRAME_ALLOCATOR.exclusive_access().dealloc(ppn);
}

#[allow(unused)]
/// a simple test for frame allocator
pub fn frame_allocator_test() {
    let mut v: Vec<FrameTracker> = Vec::new();
    for i in 0..5 {
        let frame = frame_alloc().unwrap();
        println!("{:?}", frame);
        v.push(frame);
    }
    v.clear();
    for i in 0..5 {
        let frame = frame_alloc().unwrap();
        println!("{:?}", frame);
        v.push(frame);
    }
    drop(v);
    println!("frame_allocator_test passed!");
}
