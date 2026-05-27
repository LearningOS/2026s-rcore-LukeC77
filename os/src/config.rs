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
/// the virtual addr of trapoline
pub const TRAMPOLINE: usize = usize::MAX - PAGE_SIZE + 1;    // yifan 2026/5/23: 将 trampoline 固定在最高虚拟页页首：`+1` 是因为 `usize::MAX - PAGE_SIZE` 会落在页首前一字节，需加一回到页对齐起点；这里从 `usize::MAX` 回退是为选择顶端固定高地址（在本内核 VA 类型为 usize），便于统一映射并与普通用户区分离，实际可用地址仍受 RISC-V Sv39 合法虚拟地址规则约束。
/// the virtual addr of trap context
pub const TRAP_CONTEXT_BASE: usize = TRAMPOLINE - PAGE_SIZE;
/// clock frequency
pub const CLOCK_FREQ: usize = 12500000;
/// the physical memory end
pub const MEMORY_END: usize = 0x88000000;
