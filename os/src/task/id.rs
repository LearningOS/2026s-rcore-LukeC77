//! Task pid implementation.
//!
//! Assign PID to the process here. At the same time, the position of the application KernelStack
//! is determined according to the PID.

use crate::config::{KERNEL_STACK_SIZE, PAGE_SIZE, TRAMPOLINE};
use crate::mm::{MapPermission, VirtAddr, KERNEL_SPACE};
use crate::sync::UPSafeCell;
use alloc::vec::Vec;
use lazy_static::*;

pub struct RecycleAllocator {
    current: usize,
    recycled: Vec<usize>,
}

impl RecycleAllocator {
    pub fn new() -> Self {
        RecycleAllocator {
            current: 0,
            recycled: Vec::new(),
        }
    }
    pub fn alloc(&mut self) -> usize {
        if let Some(id) = self.recycled.pop() {
            id
        } else {
            self.current += 1;
            self.current - 1
        }
    }
    pub fn dealloc(&mut self, id: usize) {
        assert!(id < self.current);
        assert!(
            !self.recycled.iter().any(|i| *i == id),
            "id {} has been deallocated!",
            id
        );
        self.recycled.push(id);
    }
}

lazy_static! {
    static ref PID_ALLOCATOR: UPSafeCell<RecycleAllocator> =    // yifan 2026/5/23: 定义全局惰性初始化的 PID 分配器，内部使用 RecycleAllocator 负责 PID 分配/回收，外层 UPSafeCell 提供单核下的独占可变访问。
        unsafe { UPSafeCell::new(RecycleAllocator::new()) };    // yifan 2026/5/23: `UPSafeCell::new` 为 unsafe，调用方需保证其安全前提（如单核/无并发冲突）成立。
    static ref KSTACK_ALLOCATOR: UPSafeCell<RecycleAllocator> =
        unsafe { UPSafeCell::new(RecycleAllocator::new()) };
}

/// Abstract structure of PID
pub struct PidHandle(pub usize);

impl Drop for PidHandle {
    fn drop(&mut self) {
        //println!("drop pid {}", self.0);
        PID_ALLOCATOR.exclusive_access().dealloc(self.0);
    }
}

/// Allocate a new PID
pub fn pid_alloc() -> PidHandle {
    PidHandle(PID_ALLOCATOR.exclusive_access().alloc())
}

/// Return (bottom, top) of a kernel stack in kernel space.
pub fn kernel_stack_position(app_id: usize) -> (usize, usize) {
    let top = TRAMPOLINE - app_id * (KERNEL_STACK_SIZE + PAGE_SIZE);    // yifan 2026/5/23: 第 app_id 个栈顶按槽位 `(KS+PZ)` 递减：top0=T, top1=T-(KS+PZ), top2=T-2(KS+PZ)；这样相邻内核栈之间可保留 1 页 guard。
    let bottom = top - KERNEL_STACK_SIZE;    // yifan 2026/5/23: 栈区间为 `[bottom, top)`，对应 stack0=[T-KS,T), stack1=[T-2KS-PZ, T-KS-PZ), stack2=[T-3KS-2PZ, T-2KS-2PZ)。
    (bottom, top)
}

/// Kernel stack for a process(task)
pub struct KernelStack(pub usize);    // yifan 2026/5/23: 这是“内核栈句柄/标识”而非栈内容本身；内部 usize 是 kstack_id，用于定位该栈并绑定 Drop 回收与栈顶计算等能力。

/// allocate a new kernel stack
pub fn kstack_alloc() -> KernelStack {
    let kstack_id = KSTACK_ALLOCATOR.exclusive_access().alloc();    // yifan 2026/5/23: 内核栈槽位由独立分配器管理，不直接复用 PID，便于将“进程标识”和“内核栈资源”解耦（生命周期/复用策略可独立演进）。
    let (kstack_bottom, kstack_top) = kernel_stack_position(kstack_id);    // yifan 2026/5/23: 根据 kstack_id 计算该内核栈在内核虚拟地址空间中的区间 `[bottom, top)`。
    KERNEL_SPACE.exclusive_access().insert_framed_area(
        kstack_bottom.into(),    // yifan 2026/5/23: 映射起始虚拟地址（栈底）。
        kstack_top.into(),    // yifan 2026/5/23: 映射结束虚拟地址（栈顶，区间右开）。
        MapPermission::R | MapPermission::W,    // yifan 2026/5/23: 栈区域需要读写权限。
    );
    KernelStack(kstack_id)    // yifan 2026/5/23: 返回内核栈句柄（记录 kstack_id），后续可据此计算栈顶并在 Drop 时回收映射与编号。
}

impl Drop for KernelStack {
    fn drop(&mut self) {
        let (kernel_stack_bottom, _) = kernel_stack_position(self.0);    // yifan 2026/5/23: Drop 时先根据 kstack_id 计算该内核栈区间，回收时以栈底作为区域起点定位键。
        let kernel_stack_bottom_va: VirtAddr = kernel_stack_bottom.into();    // yifan 2026/5/23: `remove_area_with_start_vpn` 按“起始虚拟页号”删除 MapArea，因此需把栈底地址转为虚拟地址再转 VPN。
        KERNEL_SPACE
            .exclusive_access()
            .remove_area_with_start_vpn(kernel_stack_bottom_va.into());    // yifan 2026/5/23: 从内核地址空间移除该内核栈映射区域。
        KSTACK_ALLOCATOR.exclusive_access().dealloc(self.0);    // yifan 2026/5/23: 回收 kstack_id，供后续任务复用。
    }
}

impl KernelStack {
    /// Push a variable of type T into the top of the KernelStack and return its raw pointer
    #[allow(unused)]
    pub fn push_on_top<T>(&self, value: T) -> *mut T
    where
        T: Sized,
    {    // yifan 2026/5/23: 将一个值直接放到该内核栈顶附近并返回其裸指针，常用于任务初始化时放置 TrapContext/初始上下文。
        let kernel_stack_top = self.get_top();    // yifan 2026/5/23: 取该内核栈固定的顶端地址（同一栈内通常不变）。
        let ptr_mut = (kernel_stack_top - core::mem::size_of::<T>()) as *mut T;    // yifan 2026/5/23: 栈向低地址增长，在栈顶下方预留一个 T 的空间；若重复调用通常会写到同一位置并覆盖，不是通用连续 push 接口。
        unsafe {
            *ptr_mut = value;    // yifan 2026/5/23: 将 value 写入计算出的栈内地址。
        }
        ptr_mut    // yifan 2026/5/23: 返回该对象在内核栈中的地址，供后续上下文切换/恢复逻辑使用。
    }
    /// Get the top of the KernelStack
    pub fn get_top(&self) -> usize {
        let (_, kernel_stack_top) = kernel_stack_position(self.0);    // yifan 2026/5/23: 用当前 kstack_id 重新计算内核栈区间，只取 top（忽略 bottom）。
        kernel_stack_top    // yifan 2026/5/23: 返回该内核栈的栈顶虚拟地址；这里只做地址计算，不读取栈内存内容。
    }
}
