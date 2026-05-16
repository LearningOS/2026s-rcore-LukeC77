//! Trap handling functionality
//!
//! For rCore, we have a single trap entry point, namely `__alltraps`. At
//! initialization in [`init()`], we set the `stvec` CSR to point to it.
//!
//! All traps go through `__alltraps`, which is defined in `trap.S`. The
//! assembly language code does just enough work restore the kernel space
//! context, ensuring that Rust code safely runs, and transfers control to
//! [`trap_handler()`].
//!
//! It then calls different functionality based on what exactly the exception
//! was. For example, timer interrupts trigger task preemption, and syscalls go
//! to [`syscall()`].

mod context;

use crate::config::{TRAMPOLINE, TRAP_CONTEXT_BASE};
use crate::syscall::syscall;
use crate::task::{
    current_trap_cx, current_user_token, exit_current_and_run_next, suspend_current_and_run_next,
};
use crate::timer::set_next_trigger;
use core::arch::{asm, global_asm};
use riscv::register::{
    mtvec::TrapMode,
    scause::{self, Exception, Interrupt, Trap},
    sie, stval, stvec,
};

global_asm!(include_str!("trap.S"));

/// Initialize trap handling
pub fn init() {
    set_kernel_trap_entry();
}

fn set_kernel_trap_entry() {
    extern "C" {
        fn __trap_from_kernel();
    }
    unsafe {
        // yifan 2026/5/12: stvec 是 S 态 trap 向量寄存器，S 态发生中断/异常时 CPU 会读取它决定跳转入口。
        stvec::write(__trap_from_kernel as usize, TrapMode::Direct);    // yifan 2026/5/12: 将 stvec 设置为内核 trap 入口 __trap_from_kernel，用于处理内核态发生的中断/异常。    // yifan 2026/5/12: 与第 49 行不冲突，stvec 由内核在不同阶段切换；此处对应内核态 trap 入口。
    }
}

fn set_user_trap_entry() {
    unsafe {
        stvec::write(TRAMPOLINE as usize, TrapMode::Direct);    // yifan 2026/5/12: 将 stvec 设置为用户态 trap 入口 TRAMPOLINE，使用户态中断/异常/系统调用先进入 trampoline。    // yifan 2026/5/12: 与第 43 行分工不同；43 行对应内核态 trap，49 行对应用户态 trap。
    }
}

/// enable timer interrupt in supervisor mode
pub fn enable_timer_interrupt() {
    unsafe {
        sie::set_stimer();
    }
}

/// trap handler
#[no_mangle]
pub fn trap_handler() -> ! {
    set_kernel_trap_entry();
    let cx = current_trap_cx();
    let scause = scause::read(); // get trap cause
    let stval = stval::read(); // get extra value

    // trace!("into {:?}", scause.cause());
    match scause.cause() {
        Trap::Exception(Exception::UserEnvCall) => {
            // jump to next instruction anyway
            cx.sepc += 4;
            // get system call return value
            cx.x[10] = syscall(cx.x[17], [cx.x[10], cx.x[11], cx.x[12]]) as usize;
        }
        Trap::Exception(Exception::StoreFault)
        | Trap::Exception(Exception::StorePageFault)
        | Trap::Exception(Exception::LoadFault)
        | Trap::Exception(Exception::LoadPageFault) => {
            println!("[kernel] PageFault in application, bad addr = {:#x}, bad instruction = {:#x}, kernel killed it.", stval, cx.sepc);
            exit_current_and_run_next();
        }
        Trap::Exception(Exception::IllegalInstruction) => {
            println!("[kernel] IllegalInstruction in application, kernel killed it.");
            exit_current_and_run_next();
        }
        Trap::Interrupt(Interrupt::SupervisorTimer) => {
            set_next_trigger();
            suspend_current_and_run_next();
        }
        _ => {
            panic!(
                "Unsupported trap {:?}, stval = {:#x}!",
                scause.cause(),
                stval
            );
        }
    }
    //println!("before trap_return");
    trap_return();
}

#[no_mangle]
/// return to user space
/// set the new addr of __restore asm function in TRAMPOLINE page,
/// set the reg a0 = trap_cx_ptr, reg a1 = phy addr of usr page table,
/// finally, jump to new addr of __restore asm function
pub fn trap_return() -> ! {
    set_user_trap_entry();
    let trap_cx_ptr = TRAP_CONTEXT_BASE;
    let user_satp = current_user_token();
    extern "C" {
        fn __alltraps();
        fn __restore();
    }

    // yifan 2026/5/12: 这里是在做地址平移：先取 __restore 相对 __alltraps 的段内偏移，再加上 TRAMPOLINE 基址，得到 __restore 在 trampoline 映射下的虚拟地址 restore_va。
    // yifan 2026/5/12: __alltraps 是 trampoline 段起点符号；TRAMPOLINE 是该段在统一映射中的基址常量。两者不必数值相等，但在各自视角下都表示“这段代码的起点”。
    // yifan 2026/5/12: 关键性质是同一段代码内相对偏移不变：设 A=addr(__alltraps), R=addr(__restore), T=TRAMPOLINE，则 R-A=R'-T，故 R'=(R-A)+T。
    let restore_va = __restore as usize - __alltraps as usize + TRAMPOLINE;
    // trace!("[kernel] trap_return: ..before return");
    unsafe {
        asm!(
            // yifan 2026/5/12: 使用 fence.i 指令清空指令缓存 i-cache 。
            // 这是因为，在内核中进行的一些操作可能导致一些原先存放某个应用代码的物理页帧如今用来存放数据或者是其他应用的代码，
            // i-cache 中可能还保存着该物理页帧的错误快照。因此我们直接将整个 i-cache 清空避免错误。
            "fence.i",                 
            "jr {restore_va}",         // jump to new addr of __restore asm function
            restore_va = in(reg) restore_va,
            in("a0") trap_cx_ptr,      // a0 = virt addr of Trap Context
            in("a1") user_satp,        // a1 = phy addr of usr page table
            options(noreturn)
        );
    }
}

#[no_mangle]
/// handle trap from kernel
/// Unimplement: traps/interrupts/exceptions from kernel mode
/// Todo: Chapter 9: I/O device
pub fn trap_from_kernel() -> ! {
    use riscv::register::sepc;
    trace!("stval = {:#x}, sepc = {:#x}", stval::read(), sepc::read());
    panic!("a trap {:?} from kernel!", scause::read().cause());
}

pub use context::TrapContext;
