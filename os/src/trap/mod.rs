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
        stvec::write(__trap_from_kernel as usize, TrapMode::Direct);
    }
}

fn set_user_trap_entry() {
    unsafe {
        stvec::write(TRAMPOLINE as usize, TrapMode::Direct);
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
    set_kernel_trap_entry();    // yifan 2026/5/25: 切换到内核 trap 入口，确保 trap 处理期间若再次陷入仍进入内核处理路径。
    let scause = scause::read();    // yifan 2026/5/25: 读取 scause，获取本次 trap 的具体原因（异常或中断类型）。
    let stval = stval::read();    // yifan 2026/5/25: 读取 stval，通常用于提供故障地址或相关附加信息。
    // trace!("into {:?}", scause.cause());
    match scause.cause() {
        Trap::Exception(Exception::UserEnvCall) => {
            // jump to next instruction anyway
            let mut cx = current_trap_cx();    // yifan 2026/5/25: 取当前任务 trap 上下文，用于读写用户态寄存器现场。
            cx.sepc += 4;    // yifan 2026/5/25: 跳过 ecall 指令，避免返回用户态后重复执行同一条 ecall。
            // get system call return value
            let result = syscall(cx.x[17], [cx.x[10], cx.x[11], cx.x[12]]);    // yifan 2026/5/25: 用 a7(x17) 作为 syscall 号、a0-a2(x10-x12) 作为参数执行系统调用。
            // cx is changed during sys_exec, so we have to call it again
            cx = current_trap_cx();    // yifan 2026/5/25: sys_exec 可能替换地址空间和 trap 上下文，因此重新获取 cx。
            cx.x[10] = result as usize;    // yifan 2026/5/25: 按 ABI 将系统调用返回值写回 a0(x10)。
        }
        Trap::Exception(Exception::StoreFault)
        | Trap::Exception(Exception::StorePageFault)
        | Trap::Exception(Exception::InstructionFault)
        | Trap::Exception(Exception::InstructionPageFault)
        | Trap::Exception(Exception::LoadFault)
        | Trap::Exception(Exception::LoadPageFault) => {
            println!(
                "[kernel] trap_handler:  {:?} in application, bad addr = {:#x}, bad instruction = {:#x}, kernel killed it.",
                scause.cause(),
                stval,
                current_trap_cx().sepc,
            );
            // page fault exit code
            exit_current_and_run_next(-2);    // yifan 2026/5/25: 对应用发生的访存/取指故障直接终止当前任务，并调度下一个任务。
        }
        Trap::Exception(Exception::IllegalInstruction) => {
            println!("[kernel] IllegalInstruction in application, kernel killed it.");
            // illegal instruction exit code
            exit_current_and_run_next(-3);    // yifan 2026/5/25: 非法指令异常同样终止当前任务，并切换到下一个可运行任务。
        }
        Trap::Interrupt(Interrupt::SupervisorTimer) => {
            set_next_trigger();    // yifan 2026/5/25: 设置下一次时钟中断触发点，维持周期性时间片中断。
            suspend_current_and_run_next();    // yifan 2026/5/25: 将当前任务置回 ready_queue 并经 schedule 切回 idle/run_tasks 选择下一个任务。
        }
        _ => {
            panic!(
                "Unsupported trap {:?}, stval = {:#x}!",
                scause.cause(),
                stval
            );    // yifan 2026/5/25: 未支持的 trap 直接 panic，避免内核在未知状态下继续执行。
        }
    }
    //println!("before trap_return");
    trap_return();    // yifan 2026/5/25: 分支处理完成后统一走 trap_return，恢复用户态执行（若前面未因 panic/切换而离开）。
}

#[no_mangle]
/// return to user space
/// set the new addr of __restore asm function in TRAMPOLINE page,
/// set the reg a0 = trap_cx_ptr, reg a1 = phy addr of usr page table,
/// finally, jump to new addr of __restore asm function
pub fn trap_return() -> ! {
    set_user_trap_entry();    // yifan 2026/5/26: 将 stvec 设回用户态 trap 入口，保证下次从用户态陷入时进入正确入口。
    let trap_cx_ptr = TRAP_CONTEXT_BASE;    // yifan 2026/5/26: 准备 TrapContext 虚拟地址，后续通过 a0 传给 __restore。
    let user_satp = current_user_token();    // yifan 2026/5/26: 读取当前任务用户页表 token，后续通过 a1 传给 __restore 切换地址空间。
    extern "C" {
        fn __alltraps();
        fn __restore();
    }
    let restore_va = __restore as usize - __alltraps as usize + TRAMPOLINE;    // yifan 2026/5/26: 依据符号偏移计算 trampoline 映射后的 __restore 虚拟地址。
    // trace!("[kernel] trap_return: ..before return");
    unsafe {
        asm!(
            "fence.i",    // yifan 2026/5/26: 指令同步屏障，确保后续跳转执行看到一致的指令视图。
            "jr {restore_va}",         // jump to new addr of __restore asm function    // yifan 2026/5/26: 跳转到 trampoline 中的 __restore，不再返回当前 Rust 控制流。
            restore_va = in(reg) restore_va,
            in("a0") trap_cx_ptr,      // a0 = virt addr of Trap Context    // yifan 2026/5/26: 按调用约定把 TrapContext 地址放入 a0，供 __restore 恢复寄存器现场。
            in("a1") user_satp,        // a1 = phy addr of usr page table    // yifan 2026/5/26: 按调用约定把用户页表 token 放入 a1，供 __restore 写 satp。
            options(noreturn)    // yifan 2026/5/26: 标记该内联汇编不会返回（__restore 最终 sret 回用户态）。
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
