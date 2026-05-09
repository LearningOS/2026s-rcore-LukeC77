//! The global allocator
use crate::config::KERNEL_HEAP_SIZE;    // 从 config 模块引入堆大小常量，后面初始化堆时会用到。 yifan 2026/5/4
use buddy_system_allocator::LockedHeap;    // 引入 buddy_system_allocator 提供的 LockedHeap 类型，可作为全局分配器实现。 yifan 2026/5/4

#[global_allocator]    // 告诉 Rust：下面这个 static 是全局分配器实例，Vec/Box/String 等分配会走它。 yifan 2026/5/4
/// heap allocator instance
static HEAP_ALLOCATOR: LockedHeap = LockedHeap::empty();    // 定义全局分配器对象，初始为空堆，后续在 init_heap() 中绑定实际内存。 yifan 2026/5/4

#[alloc_error_handler]
/// panic when heap allocation error occurs
pub fn handle_alloc_error(layout: core::alloc::Layout) -> ! {
    panic!("Heap allocation error, layout = {:?}", layout);
}

// yifan 2026/5/4
// 为什么要把全局变量作为堆后端：在 no_std 内核启动早期，堆分配器尚未初始化，不能直接用 Box/Vec 做堆分配。
// 先预留一块静态内存（如 HEAP_SPACE）并交给分配器管理，之后才能正常“直接使用堆”。
// HEAP_SPACE 位于 .bss 会占用内存，但这是有意预留给堆的 RAM，不是额外侵占。
// 只要链接脚本和 init 传入的地址/大小正确，分配器只会在 HEAP_SPACE 范围内分配，不会覆盖其他 .bss 变量。
// 真正会混乱的情况通常是内存布局重叠、初始化参数错误或重复/错误初始化。
/// heap space ([u8; KERNEL_HEAP_SIZE])
static mut HEAP_SPACE: [u8; KERNEL_HEAP_SIZE] = [0; KERNEL_HEAP_SIZE];    // 定义全局可变字节数组，作为内核堆的实际内存空间。 yifan 2026/5/4
/// initiate heap allocator
pub fn init_heap() {    // 定义堆初始化函数，系统启动早期会调用它。 yifan 2026/5/4
    unsafe {    // 进入 unsafe 块是因为要操作 static mut，编译器无法自动保证安全。 yifan 2026/5/4
        HEAP_ALLOCATOR
            .lock()
            .init(HEAP_SPACE.as_ptr() as usize, KERNEL_HEAP_SIZE);    // 将 HEAP_SPACE 的起始地址和大小注册给分配器，使其成为可分配的堆。 yifan 2026/5/4
    }
}

#[allow(unused)]    // yifan 2026/5/4: 允许该函数未被调用时不触发 unused 警告。
pub fn heap_test() {    // yifan 2026/5/4: 定义公开的堆分配测试函数。
    use alloc::boxed::Box;    // yifan 2026/5/4: 引入 Box，用于测试单对象堆分配。
    use alloc::vec::Vec;    // yifan 2026/5/4: 引入 Vec，用于测试动态数组堆分配。
    extern "C" {    // yifan 2026/5/4: 声明 C ABI 外部符号，读取链接脚本提供的地址边界。
        fn sbss();    // yifan 2026/5/4: .bss 段起始符号。
        fn ebss();    // yifan 2026/5/4: .bss 段结束符号。
    }
    let bss_range = sbss as usize..ebss as usize;    // yifan 2026/5/4: 构造 .bss 地址区间，用于校验分配地址来源。
    let a = Box::new(5);    // yifan 2026/5/4: 在堆上分配一个值为 5 的对象。
    assert_eq!(*a, 5);    // yifan 2026/5/4: 验证堆对象读写正确。
    assert!(bss_range.contains(&(a.as_ref() as *const _ as usize)));    // yifan 2026/5/4: 断言 Box 对象地址位于 .bss 范围内。
    drop(a);    // yifan 2026/5/4: 主动释放 Box，测试释放路径。
    let mut v: Vec<usize> = Vec::new();    // yifan 2026/5/4: 创建空 Vec，后续触发堆扩容分配。
    for i in 0..500 {    // yifan 2026/5/4: 循环压入元素，施加分配压力。
        v.push(i);    // yifan 2026/5/4: 向 Vec 追加元素，可能触发扩容。
    }
    for (i, val) in v.iter().take(500).enumerate() {    // yifan 2026/5/4: 遍历前 500 个元素并携带索引做一致性校验。
        assert_eq!(*val, i);    // yifan 2026/5/4: 断言元素值与其索引一致。
    }
    assert!(bss_range.contains(&(v.as_ptr() as usize)));    // yifan 2026/5/4: 断言 Vec 底层缓冲区地址也来自 .bss 堆区。
    drop(v);    // yifan 2026/5/4: 释放 Vec，测试释放逻辑。
    println!("heap_test passed!");    // yifan 2026/5/4: 输出测试通过信息。
}
