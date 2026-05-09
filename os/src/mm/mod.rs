//! Memory management implementation
//!
//! SV39 page-based virtual-memory architecture for RV64 systems, and
//! everything about memory management, like frame allocator, page table,
//! map area and memory set, is implemented here.
//!
//! Every task or process has a memory_set to control its virtual memory.

mod address;
mod frame_allocator;
mod heap_allocator;
mod memory_set;
mod page_table;

pub use address::{PhysAddr, PhysPageNum, VirtAddr, VirtPageNum};
use address::{StepByOne, VPNRange};
pub use frame_allocator::{frame_alloc, FrameTracker};
pub use memory_set::remap_test;
pub use memory_set::{kernel_stack_position, MapPermission, MemorySet, KERNEL_SPACE};
pub use page_table::{translated_byte_buffer, PageTableEntry};
pub use page_table::{PTEFlags, PageTable};

/// initiate heap allocator, frame allocator and kernel space
pub fn init() {
    heap_allocator::init_heap(); // yifan 2026/5/9 我们最先进行了全局动态内存分配器的初始化，因为接下来马上就要用到 Rust 的堆数据结构。
    frame_allocator::init_frame_allocator(); // yifan 2026/5/9 接下来我们初始化物理页帧管理器（内含堆数据结构 Vec<T> ）使能可用物理页帧的分配和回收能力。
    // yifan 2026/5/9 最后我们创建内核地址空间并让 CPU 开启分页模式， MMU 在地址转换的时候使用内核的多级页表，这一切均在一行之内做到
    // 首先，我们引用 KERNEL_SPACE ，这是它第一次被使用，就在此时它会被初始化，调用 MemorySet::new_kernel 创建一个内核地址空间并使用 Arc<UPSafeCell<T>> 包裹起来；
    // 接着使用 .exclusive_access() 获取一个可变引用 &mut MemorySet 。
    // 最后，我们调用 MemorySet::activate 在 activate 中，我们将这个值写入当前 CPU 的 satp CSR ，从这一刻开始 SV39 分页模式就被启用了，而且 MMU 会使用内核地址空间的多级页表进行地址转换。
    KERNEL_SPACE.exclusive_access().activate(); 
}
