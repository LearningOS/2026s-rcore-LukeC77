//! Constants in the kernel

#[allow(unused)]

/// user app's stack size
pub const USER_STACK_SIZE: usize = 4096 * 2;
/// kernel stack size
pub const KERNEL_STACK_SIZE: usize = 4096 * 2;
/// kernel heap size
pub const KERNEL_HEAP_SIZE: usize = 0x200_0000;

/// page size : 4KB
pub const PAGE_SIZE: usize = 0x1000;
/// page size bits: 12
pub const PAGE_SIZE_BITS: usize = 0xc;
/// the max number of syscall
pub const MAX_SYSCALL_NUM: usize = 500;
/// the virtual addr of trapoline
// yifan 2026/5/7: Trampoline 放在 Sv39 顶端虚拟页，作为各地址空间统一保留位置，避免与用户程序常规代码/堆/栈区域冲突。
// yifan 2026/5/7: trap 发生时 CPU 先在当前页表下按 stvec 取指，因此该虚拟地址必须在用户页表与内核页表中都可访问。
// yifan 2026/5/7: 这里的“所有地址空间”指内核地址空间和每个用户进程地址空间都把 trampoline 映射到同一个 TRAMPOLINE 虚拟地址，保证切换页表前后都能稳定执行 trap 入口/返回代码。
pub const TRAMPOLINE: usize = usize::MAX - PAGE_SIZE + 1;
/// the virtual addr of trap context
pub const TRAP_CONTEXT_BASE: usize = TRAMPOLINE - PAGE_SIZE;
/// clock frequency
pub const CLOCK_FREQ: usize = 12500000;
/// the physical memory end
pub const MEMORY_END: usize = 0x88000000;
