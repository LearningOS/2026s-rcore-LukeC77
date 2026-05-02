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

use crate::batch::run_next_app;
use crate::syscall::syscall;
use core::arch::global_asm;
use riscv::register::{
    mtvec::TrapMode,
    scause::{self, Exception, Trap},    // yifan 2026/5/2: `scause` here is imported from riscv crate's register::scause module, which provides access to the hardware CSR.
    stval, stvec,
};

global_asm!(include_str!("trap.S"));

/// initialize CSR `stvec` as the entry of `__alltraps`
pub fn init() {
    // yifan 2026/5/1: `init` is called during kernel startup to set trap entry.
    extern "C" {
        // yifan 2026/5/1: declare external symbol from trap.S with C ABI so Rust can link to it.
        fn __alltraps();
    }
    unsafe {
        // yifan 2026/5/1: write CSR `stvec` with __alltraps address in Direct mode,
        // so every trap first jumps to this single assembly entry.
        stvec::write(__alltraps as usize, TrapMode::Direct);
    }
}

#[no_mangle]
/// handle an interrupt, exception, or system call from user space
pub fn trap_handler(cx: &mut TrapContext) -> &mut TrapContext {
    let scause = scause::read(); // get trap cause    // yifan 2026/5/2: CPU writes trap reason into CSR scause on trap entry, and trap_handler reads it here for dispatch.
    let stval = stval::read(); // get extra value    // yifan 2026/5/2: read CSR stval (Supervisor Trap Value), which carries trap-specific extra data such as faulting addresses and is useful for panic diagnostics.
    match scause.cause() {
        Trap::Exception(Exception::UserEnvCall) => {    // yifan 2026/5/2: this branch handles a normal user-space ecall trap (system call path).
            cx.sepc += 4;    // yifan 2026/5/2: advance saved sepc by 4 bytes so sret resumes at the instruction after ecall instead of trapping again.
            cx.x[10] = syscall(cx.x[17], [cx.x[10], cx.x[11], cx.x[12]]) as usize;    // yifan 2026/5/2: pass a7 as syscall ID and a0-a2 as args, then write return value to TrapContext slot x[10] (future user a0), not the current S-mode physical a0 used to pass cx pointer, so context pointer flow is not overwritten.
        }    
        Trap::Exception(Exception::StoreFault) | Trap::Exception(Exception::StorePageFault) => {
            println!("[kernel] PageFault in application, kernel killed it.");
            run_next_app();
        }
        Trap::Exception(Exception::IllegalInstruction) => {
            println!("[kernel] IllegalInstruction in application, kernel killed it.");
            run_next_app();
        }
        _ => {
            panic!(
                "Unsupported trap {:?}, stval = {:#x}!",
                scause.cause(),
                stval
            );
        }
    }
    cx
}

pub use context::TrapContext;
