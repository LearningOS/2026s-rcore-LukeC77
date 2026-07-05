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
    check_signals_error_of_current, current_add_signal, current_trap_cx, current_user_token,
    exit_current_and_run_next, handle_signals, suspend_current_and_run_next, SignalFlags,
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
    set_kernel_trap_entry();    // yifan 2026/6/30: 刚从用户态陷入内核后，先把 stvec 切换成内核自己的 trap 入口；这样如果后续内核执行过程中再次发生 trap，就会继续进入内核 trap 入口，而不是跳回用户态 trampoline。
    let scause = scause::read();    // yifan 2026/6/30: 读取 RISC-V 的 scause 寄存器，用来判断这次 trap 的具体原因，例如用户态 ecall、页故障、非法指令或定时器中断；下面的 match 就是依据它来分发处理逻辑。
    let stval = stval::read();    // yifan 2026/6/30: 读取 stval 寄存器，拿到与本次 trap 相关的附加信息；最常见的是出错地址，例如页故障时这里通常就是触发 fault 的那个虚拟地址。
    // trace!("into {:?}", scause.cause());
    match scause.cause() {    // yifan 2026/6/30: 这里根据 scause 里记录的 trap 原因做分发处理；scause.cause() 会把本次陷入内核的原因解析成枚举值，然后由 match 决定后续应该按系统调用、访存异常、非法指令还是中断等不同路径来处理。
        Trap::Exception(Exception::UserEnvCall) => {    // yifan 2026/6/30: 这个分支表示本次 trap 的原因是用户态执行了 ecall，也就是用户程序发起了一次系统调用。
            // jump to next instruction anyway
            let mut cx = current_trap_cx();    // yifan 2026/6/30: 取出当前进程保存的 TrapContext，其中包含用户态陷入内核时的寄存器现场，例如 sepc 和各个通用寄存器的值。
            cx.sepc += 4;    // yifan 2026/6/30: 把用户态返回后要继续执行的指令地址前移一条指令长度；因为 ecall 已经处理完了，如果不跳过它，返回用户态后会再次执行 ecall，从而反复陷入内核。
            // get system call return value
            let result = syscall(cx.x[17], [cx.x[10], cx.x[11], cx.x[12], cx.x[13]]);    // yifan 2026/6/30: 按照 RISC-V 调用约定，从保存下来的寄存器里取出系统调用号和参数；x[17] 也就是 a7 存 syscall 编号，x[10] 到 x[13] 也就是 a0 到 a3 存前四个参数，然后交给内核的 syscall 分发函数执行并得到返回值。
            // cx is changed during sys_exec, so we have to call it again
            cx = current_trap_cx();    // yifan 2026/6/30: 重新获取一次当前 TrapContext，因为像 sys_exec 这样的系统调用可能会更换当前进程的地址空间或 trap 上下文，之前拿到的 cx 可能已经不是最新的了。
            cx.x[10] = result as usize;    // yifan 2026/6/30: 把系统调用返回值写回 x[10]，也就是 a0；这样内核返回用户态后，用户程序就能像普通函数返回一样从 a0 里拿到 syscall 的结果。
        }
        Trap::Exception(Exception::StoreFault)
        | Trap::Exception(Exception::StorePageFault)
        | Trap::Exception(Exception::InstructionFault)
        | Trap::Exception(Exception::InstructionPageFault)
        | Trap::Exception(Exception::LoadFault)
        | Trap::Exception(Exception::LoadPageFault) => {    // yifan 2026/6/30: 只要本次 trap 属于读内存、写内存或取指相关的 fault/page fault，就统一进入这个分支；这说明用户程序发生了严重的非法访存或非法取指异常。
            error!(    // yifan 2026/6/30: 先打印一条错误日志，把这次异常的关键信息记录下来，便于后续调试和定位应用为什么崩溃。
                "[kernel] trap_handler: {:?} in application, bad addr = {:#x}, bad instruction = {:#x}, kernel killed it.",
                scause.cause(),    // yifan 2026/6/30: 这里打印异常类型，也就是这次 trap 具体是读错、写错、取指错还是对应的页故障。
                stval,    // yifan 2026/6/30: 这里打印 stval，它通常保存触发异常的坏地址；例如用户程序访问了一个没有映射或不允许访问的虚拟地址时，这里往往就是那个地址。
                current_trap_cx().sepc,    // yifan 2026/6/30: 这里打印用户程序出错时的 PC，也就是发生异常时正在执行的那条用户指令地址，用来定位程序是跑到哪里时出问题的。
            );
            current_add_signal(SignalFlags::SIGSEGV);    // yifan 2026/6/30: 给当前进程挂上 SIGSEGV 信号，表示它发生了段错误/非法内存访问；这里不是立刻直接销毁进程，而是交给后续的信号处理机制决定是执行用户注册的 handler 还是按默认方式终止它。
        }
        Trap::Exception(Exception::IllegalInstruction) => {    // yifan 2026/6/30: 这个分支表示本次 trap 的原因是用户程序执行了非法指令，也就是 CPU 发现当前用户态这条指令根本不合法，无法继续执行。
            current_add_signal(SignalFlags::SIGILL);    // yifan 2026/6/30: 给当前进程挂上 SIGILL 信号，表示发生了非法指令异常；这里不是当场直接销毁进程，而是先把异常转换成信号，再交给后续信号处理机制决定是执行用户注册的 handler 还是按默认方式终止进程。
        }
        Trap::Interrupt(Interrupt::SupervisorTimer) => {    // yifan 2026/6/30: 这个分支表示本次 trap 不是异常而是一次监督态时钟中断，也就是内核的一个时钟 tick 到了。
            set_next_trigger();    // yifan 2026/6/30: 处理当前这次时钟中断后，立刻重新设置下一次时钟中断的触发时间；因为时钟中断需要周期性到来，而不是只触发一次。
            suspend_current_and_run_next();    // yifan 2026/6/30: 挂起当前正在运行的任务并切换到下一个任务运行；这正是时间片轮转调度发生的关键位置之一，当前任务时间片到期后，CPU 会借由这次时钟中断让给别的任务。
        }
        _ => {
            panic!(
                "Unsupported trap {:?}, stval = {:#x}!",
                scause.cause(),
                stval
            );
        }
    }
    // handle signals (handle the sent signal)
    // trace!("[kernel] trap_handler:: handle_signals");
    handle_signals();    // yifan 2026/6/30: 在前面的异常或中断分支处理完之后，先统一处理当前进程已经挂起的信号；这里会查看 pending signals，并决定是否执行用户注册的 signal handler 或更新相关信号状态。

    // check error signals (if error then exit)
    if let Some((errno, msg)) = check_signals_error_of_current() {    // yifan 2026/6/30: 处理完信号后，再检查当前进程是否还带有需要按错误处理的致命信号；如果有，就返回对应的退出码 errno 和错误说明 msg，否则返回 None。
        trace!("[kernel] trap_handler: .. check signals {}", msg);    // yifan 2026/6/30: 打一条调试日志，说明当前是因为某个错误信号而准备结束这个进程，并把对应的错误信息打印出来。
        exit_current_and_run_next(errno);    // yifan 2026/6/30: 真正结束当前进程，并把 errno 作为退出码，然后切换到下一个可运行任务继续执行。
    }
    trap_return();    // yifan 2026/6/30: 如果没有致命信号导致当前进程退出，就从这次 trap 处理流程返回用户态，让当前用户程序从保存好的现场继续执行。
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
    let restore_va = __restore as usize - __alltraps as usize + TRAMPOLINE;
    // trace!("[kernel] trap_return: ..before return");
    unsafe {
        asm!(
            "fence.i",
            "jr {restore_va}",
            restore_va = in(reg) restore_va,
            in("a0") trap_cx_ptr,
            in("a1") user_satp,
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
