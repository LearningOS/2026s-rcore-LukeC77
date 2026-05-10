//! The main module and entrypoint
//!
//! Various facilities of the kernels are implemented as submodules. The most
//! important ones are:
//!
//! - [`trap`]: Handles all cases of switching from userspace to the kernel
//! - [`task`]: Task management
//! - [`syscall`]: System call handling and implementation
//!
//! The operating system also starts in this module. Kernel code starts
//! executing from `entry.asm`, after which [`rust_main()`] is called to
//! initialize various pieces of functionality. (See its source code for
//! details.)
//!
//! We then call [`task::run_first_task()`] and for the first time go to
//! userspace.

#![deny(missing_docs)]
#![deny(warnings)]
#![no_std]
#![no_main]
#![feature(panic_info_message)]
#![feature(alloc_error_handler)]    // yifan 2026/5/4 启用 nightly 的 alloc_error_handler 特性，使 no_std 内核可定义分配失败处理函数；否则 #[alloc_error_handler] 无法编译。

#[macro_use]
extern crate bitflags;
#[macro_use]
extern crate log;

extern crate alloc;

#[macro_use]
mod console;
pub mod config;
pub mod lang_items;
mod loader;
pub mod logging;
pub mod mm;
pub mod sbi;
pub mod sync;
pub mod syscall;
pub mod task;
pub mod timer;
pub mod trap;

core::arch::global_asm!(include_str!("entry.asm"));
core::arch::global_asm!(include_str!("link_app.S"));

/// clear BSS segment
fn clear_bss() {
    extern "C" {
        fn sbss();
        fn ebss();
    }
    unsafe {
        core::slice::from_raw_parts_mut(sbss as usize as *mut u8, ebss as usize - sbss as usize)
            .fill(0);
    }
}

/// kernel log info
fn kernel_log_info() {
    extern "C" {
        fn stext(); // begin addr of text segment
        fn etext(); // end addr of text segment
        fn srodata(); // start addr of Read-Only data segment
        fn erodata(); // end addr of Read-Only data ssegment
        fn sdata(); // start addr of data segment
        fn edata(); // end addr of data segment
        fn sbss(); // start addr of BSS segment
        fn ebss(); // end addr of BSS segment
        fn boot_stack_lower_bound(); // stack lower bound
        fn boot_stack_top(); // stack top
    }
    logging::init();
    println!("[kernel] Hello, world!");
    trace!(
        "[kernel] .text [{:#x}, {:#x})",
        stext as usize,
        etext as usize
    );
    debug!(
        "[kernel] .rodata [{:#x}, {:#x})",
        srodata as usize, erodata as usize
    );
    info!(
        "[kernel] .data [{:#x}, {:#x})",
        sdata as usize, edata as usize
    );
    warn!(
        "[kernel] boot_stack top=bottom={:#x}, lower_bound={:#x}",
        boot_stack_top as usize, boot_stack_lower_bound as usize
    );
    error!("[kernel] .bss [{:#x}, {:#x})", sbss as usize, ebss as usize);
}

#[no_mangle]
/// the rust entry-point of os
pub fn rust_main() -> ! {
    // yifan 2026/5/9: 在内核正式初始化前把 .bss 段清零，确保未初始化的全局/静态变量初值为 0，避免后续 mm/log/task 等模块读取到脏数据。
    // .bss 是 RAM 区域，启动时其内容不保证为 0（可能是随机值或残留），而语言/ABI 要求未初始化全局静态变量初值为 0，所以必须主动清零。
    clear_bss();
    // yifan 2026/5/9: 打印内核内存布局相关日志（.text/.rodata/.data/.bss、boot stack 边界等），用于启动早期调试和核对链接地址是否正确。
    kernel_log_info();
    // yifan 2026/5/9: 初始化内存管理子系统（内核堆分配器、物理页帧分配器并激活内核页表），为后续 trap/任务等模块提供可用的动态分配与分页映射能力。
    mm::init();    
    println!("[kernel] back to world!");
    // yifan 2026/5/9: 执行内核页表映射自检，验证关键段权限（如 .text 不可写、.rodata 不可写、.data 不可执行）；若映射错误会触发 assert/panic 及时停止。
    mm::remap_test();    
    trap::init();
    trap::enable_timer_interrupt();
    timer::set_next_trigger();
    task::run_first_task();
    panic!("Unreachable in rust_main!");
}
