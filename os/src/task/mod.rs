//! Implementation of process management mechanism
//!
//! Here is the entry for process scheduling required by other modules
//! (such as syscall or clock interrupt).
//! By suspending or exiting the current process, you can
//! modify the process state, manage the process queue through TASK_MANAGER,
//! and switch the control flow through PROCESSOR.
//!
//! Be careful when you see [`__switch`]. Control flow around this function
//! might not be what you expect.

mod action;
mod context;
mod manager;
mod id;
mod processor;
mod signal;
mod switch;
#[allow(clippy::module_inception)]
mod task;

use crate::fs::{open_file, OpenFlags};
use alloc::sync::Arc;
pub use context::TaskContext;
use lazy_static::*;
use manager::fetch_task;
use manager::remove_from_pid2task;
use switch::__switch;
pub use task::{TaskControlBlock, TaskStatus};

pub use action::{SignalAction, SignalActions};
pub use manager::{add_task, pid2task};
pub use id::{kstack_alloc, pid_alloc, KernelStack, PidHandle};
pub use processor::{
    current_task, current_trap_cx, current_user_token, run_tasks, schedule, take_current_task,
};
pub use signal::{SignalFlags, MAX_SIG};

/// Make current task suspended and switch to the next task
pub fn suspend_current_and_run_next() {
    // There must be an application running.
    let task = take_current_task().unwrap();

    // ---- access current TCB exclusively
    let mut task_inner = task.inner_exclusive_access();
    let task_cx_ptr = &mut task_inner.task_cx as *mut TaskContext;
    // Change status to Ready
    task_inner.task_status = TaskStatus::Ready;
    drop(task_inner);
    // ---- release current PCB

    // push back to ready queue.
    add_task(task);
    // jump to scheduling cycle
    schedule(task_cx_ptr);
}

/// pid of usertests app in make run TEST=1
pub const IDLE_PID: usize = 0;

/// Exit the current 'Running' task and run the next task in task list.
pub fn exit_current_and_run_next(exit_code: i32) {
    // take from Processor
    let task = take_current_task().unwrap();

    let pid = task.getpid();
    if pid == IDLE_PID {
        println!(
            "[kernel] Idle process exit with exit_code {} ...",
            exit_code
        );
        panic!("All applications completed!");
    }

    // remove from pid2task
    remove_from_pid2task(task.getpid());
    // **** access current TCB exclusively
    let mut inner = task.inner_exclusive_access();
    // Change status to Zombie
    inner.task_status = TaskStatus::Zombie;
    // Record exit code
    inner.exit_code = exit_code;
    // do not move to its parent but under initproc

    // ++++++ access initproc TCB exclusively
    {
        let mut initproc_inner = INITPROC.inner_exclusive_access();
        for child in inner.children.iter() {
            child.inner_exclusive_access().parent = Some(Arc::downgrade(&INITPROC));
            initproc_inner.children.push(child.clone());
        }
    }
    // ++++++ release parent PCB

    inner.children.clear();
    // deallocate user space
    inner.memory_set.recycle_data_pages();
    // drop file descriptors
    inner.fd_table.clear();
    drop(inner);
    // **** release current PCB
    // drop task manually to maintain rc correctly
    drop(task);
    // we do not have to save task context
    let mut _unused = TaskContext::zero_init();
    schedule(&mut _unused as *mut _);
}

lazy_static! {
    /// Creation of initial process
    ///
    /// the name "initproc" may be changed to any other app name like "usertests",
    /// but we have user_shell, so we don't need to change it.
    pub static ref INITPROC: Arc<TaskControlBlock> = Arc::new({
        let inode = open_file("ch7b_initproc", OpenFlags::RDONLY).unwrap();
        let v = inode.read_all();
        TaskControlBlock::new(v.as_slice())
    });
}

///Add init process to the manager
pub fn add_initproc() {
    add_task(INITPROC.clone());
}

/// Check if the current task has any signal to handle
pub fn check_signals_error_of_current() -> Option<(i32, &'static str)> {    // yifan 2026/6/30: 这个函数负责检查当前进程挂起的信号里，是否存在需要按错误处理并导致进程退出的致命信号；返回 None 表示没有错误信号，返回 Some((errno, msg)) 则表示发现了对应的退出码和错误说明。
    let task = current_task().unwrap();    // yifan 2026/6/30: 先取出当前 CPU 上正在运行的任务；这里使用 unwrap，说明按设计此时一定应该存在当前任务，否则就属于内核状态异常。
    let task_inner = task.inner_exclusive_access();    // yifan 2026/6/30: 因为接下来要读取任务控制块内部保存的 signals 字段，所以需要先对任务内部状态做一次独占访问。
    // println!(
    //     "[K] check_signals_error_of_current {:?}",
    //     task_inner.signals
    // );
    task_inner.signals.check_error()    // yifan 2026/6/30: 真正的检查逻辑由 signals 上的 check_error() 完成；它会查看当前挂起信号集合里是否包含像 SIGINT、SIGILL、SIGABRT、SIGFPE、SIGKILL、SIGSEGV 这类致命信号，如果有就返回对应的错误码和错误信息，否则返回 None。
}

/// Add signal to the current task
pub fn current_add_signal(signal: SignalFlags) {    // yifan 2026/6/30: 这个辅助函数的作用是给当前正在运行的任务追加一个信号；参数 signal 就是要挂到当前任务上的那个信号标志，例如 SIGSEGV 或 SIGILL。
    let task = current_task().unwrap();    // yifan 2026/6/30: 先取出当前 CPU 上正在运行的任务；这里使用 unwrap，说明按设计此时一定应该存在当前任务，否则就属于内核状态异常。
    let mut task_inner = task.inner_exclusive_access();    // yifan 2026/6/30: 因为接下来要修改任务控制块内部保存的信号集合 signals，所以需要先对任务内部状态做一次独占访问。
    task_inner.signals |= signal;    // yifan 2026/6/30: 把传入的信号按位或进当前任务的 signals 位图里，也就是把对应那一位置 1，表示这个信号已经挂起并会在后续被处理；这里是追加，不会覆盖原来已经存在的其他信号位。
    // println!(
    //     "[K] current_add_signal:: current task sigflag {:?}",
    //     task_inner.signals
    // );
}

/// call kernel signal handler
fn call_kernel_signal_handler(signal: SignalFlags) {    // yifan 2026/6/30: 这个函数负责处理那些由内核直接解释语义的特殊信号；它们不走用户态 handler，而是由内核立即更新当前进程的状态位。
    let task = current_task().unwrap();    // yifan 2026/6/30: 先取出当前 CPU 上正在运行的任务；这里按设计必须存在当前任务，否则就属于内核状态异常。
    let mut task_inner = task.inner_exclusive_access();    // yifan 2026/6/30: 因为后面要直接修改当前任务控制块中的信号和调度相关状态，所以需要先对任务内部数据做一次独占访问。
    match signal {    // yifan 2026/6/30: 根据收到的内核信号种类，选择对应的默认处理动作；其中 SIGSTOP 和 SIGCONT 有特殊语义，其他情况统一按致命信号处理。
        SignalFlags::SIGSTOP => {    // yifan 2026/6/30: 如果收到 SIGSTOP，表示当前进程应该被暂停执行，这是一个“冻结进程”而不是“杀死进程”的动作。
            task_inner.frozen = true;    // yifan 2026/6/30: 把当前进程标记为 frozen=true，表示它后续不应继续像普通可运行任务那样执行；handle_signals 会据此把它挂起并切走。
            task_inner.signals ^= SignalFlags::SIGSTOP;    // yifan 2026/6/30: 把已经接收到的 SIGSTOP 从 pending signals 集合中清掉，避免同一个停止信号在后续被重复处理。
        }
        SignalFlags::SIGCONT => {    // yifan 2026/6/30: 如果收到 SIGCONT，表示让之前被暂停的进程恢复执行，这是一个“解除冻结”的动作。
            if task_inner.signals.contains(SignalFlags::SIGCONT) {    // yifan 2026/6/30: 先确认 SIGCONT 当前确实还在 pending signals 集合里，再去消费并处理它。
                task_inner.signals ^= SignalFlags::SIGCONT;    // yifan 2026/6/30: 把已经接收到的 SIGCONT 从 pending signals 集合中清掉，避免这个继续信号在后续再次被重复处理。
                task_inner.frozen = false;    // yifan 2026/6/30: 清除冻结状态，表示该进程之后可以重新被调度继续运行。
            }
        }
        _ => {    // yifan 2026/6/30: 除了 SIGSTOP 和 SIGCONT 之外，这里归到内核信号处理路径的其他信号都按默认致命处理方式走。
            // println!(
            //     "[K] call_kernel_signal_handler:: current task sigflag {:?}",
            //     task_inner.signals
            // );
            task_inner.killed = true;    // yifan 2026/6/30: 把当前进程标记为 killed=true，表示它应当被终止；这样的进程不会在这里立刻释放，而是会在 Trap 返回用户态之前被后续信号检查和调度逻辑切换出去并结束运行。
        }
    }
}

/// call user signal handler
fn call_user_signal_handler(sig: usize, signal: SignalFlags) {    // yifan 2026/6/30: 这个函数负责处理普通用户信号；如果当前进程为该信号提供了用户态处理例程，就把 Trap 返回后的用户执行现场改造成“先去执行这个 handler”。
    let task = current_task().unwrap();    // yifan 2026/6/30: 先取出当前 CPU 上正在运行的任务；这里按设计必须存在当前任务，否则就属于内核状态异常。
    let mut task_inner = task.inner_exclusive_access();    // yifan 2026/6/30: 因为后面要修改当前任务的信号状态和 Trap 上下文，所以需要先对任务内部数据做一次独占访问。

    let handler = task_inner.signal_actions.table[sig].handler;    // yifan 2026/6/30: 根据信号编号 sig 取出该信号配置的处理动作，并读取其中的 handler 字段；它就是用户通过 sigaction 等接口设置的处理例程入口地址。
    if handler != 0 {    // yifan 2026/6/30: 只有当 handler 非 0 时，才说明进程真的为这个信号提供了用户态处理例程；如果是 0，就表示没有提供专门的 handler。
        // user handler

        // handle flag
        task_inner.handling_sig = sig as isize;    // yifan 2026/6/30: 记录当前正在处理哪个信号；后续如果新的信号到来，就可以据此判断是否允许嵌套处理，或者是否会被当前 handler 的局部 mask 屏蔽。
        task_inner.signals ^= signal;    // yifan 2026/6/30: 把这个已经准备开始处理的信号从 pending signals 集合中清掉，避免后续再次重复处理同一个信号。

        // backup trapframe
        let trap_ctx = task_inner.get_trap_cx();    // yifan 2026/6/30: 取出当前进程的 Trap 上下文，也就是用户态陷入内核时保存下来的寄存器现场，后面会直接在这份现场上做修改。
        task_inner.trap_ctx_backup = Some(*trap_ctx);    // yifan 2026/6/30: 先把当前 Trap 上下文完整备份到 trap_ctx_backup 中；因为后面要改写现场去执行 handler，而等 handler 结束并调用 sigreturn 后，还需要恢复原来的用户执行现场。

        // modify trapframe
        trap_ctx.sepc = handler;    // yifan 2026/6/30: 把 Trap 上下文中的 sepc 改成用户设定的处理例程地址；这样 Trap 返回用户态后，CPU 不会回到原先被打断的位置，而是直接跳到这个 handler 入口开始执行。这里没有修改 sp，因此 handler 仍然会在原来的用户栈上运行，这是这个教学内核里的简化实现。

        // put args (a0)
        trap_ctx.x[10] = sig;    // yifan 2026/6/30: 把信号编号写入 x[10] 也就是 a0，这样用户态 handler 启动后就能把当前信号类型作为第一个参数接收。
    } else {
        // default action
        println!("[K] task/call_user_signal_handler: default action: ignore it or kill process");    // yifan 2026/6/30: 如果当前进程没有为这个信号注册用户态处理例程，这里就不去改写 Trap 上下文启动 handler，而是走默认动作分支；当前这份代码里这里只打印提示信息，表示默认动作可能是忽略或杀死进程。
    }
}

// yifan 2026/6/30: 它的核心作用是从当前进程所有挂起的信号里找出“现在可以处理的那个信号”，并决定它应该走内核处理路径还是用户 handler 处理路径。
/// Check if the current task has any signal to handle
fn check_pending_signals() {    // yifan 2026/6/30: 这个函数负责扫描当前进程所有 pending signals，从中挑出当前真正可以处理的那个信号，再把它交给内核信号处理逻辑或用户信号处理逻辑。    
    for sig in 0..(MAX_SIG + 1) {    // yifan 2026/6/30: 按信号编号从小到大遍历所有可能的信号，逐个检查它们当前是否处于可处理状态。
        let task = current_task().unwrap();    // yifan 2026/6/30: 先取出当前 CPU 上正在运行的任务；这里按设计必须存在当前任务，否则就属于内核状态异常。
        let task_inner = task.inner_exclusive_access();    // yifan 2026/6/30: 因为接下来要读取当前任务内部保存的信号相关字段，所以需要先对任务内部状态做一次独占访问。
        let signal = SignalFlags::from_bits(1 << sig).unwrap();    // yifan 2026/6/30: 把当前遍历到的信号编号 sig 转成对应的位图标志；例如 sig=10 时，这里得到的就是 SIGUSR1 对应的那一位。
        if task_inner.signals.contains(signal) && (!task_inner.signal_mask.contains(signal)) {    // yifan 2026/6/30: 第一层筛选条件是：这个信号必须已经在 pending signals 集合里，同时它又不能被进程的全局 signal_mask 屏蔽；只有满足这两点才有资格继续考虑处理。
            let mut masked = true;    // yifan 2026/6/30: 这里的 masked 不是指全局 signal_mask，而是用来判断这个信号是否会被“当前正在执行的 signal handler 的局部 mask”临时屏蔽住。
            let handling_sig = task_inner.handling_sig;    // yifan 2026/6/30: 取出当前是否已经有某个信号正在处理；-1 表示当前没有 signal handler 在运行，其他值表示正在处理对应编号的信号。
            if handling_sig == -1 {    // yifan 2026/6/30: 如果当前没有任何 signal handler 正在执行，那么这个候选信号就不受 handler 局部 mask 的限制，可以直接视为未被屏蔽。
                masked = false;    // yifan 2026/6/30: 把 masked 设为 false，表示这个候选信号在当前上下文里允许被处理。
            } else {
                let handling_sig = handling_sig as usize;    // yifan 2026/6/30: 如果当前已经在执行某个 handler，就把 handling_sig 转成数组下标，后面要去查这个 handler 配置的局部信号掩码。
                if !task_inner.signal_actions.table[handling_sig]
                    .mask
                    .contains(signal)    // yifan 2026/6/30: 检查当前正在运行的 handler 的 mask 是否包含这个候选信号；如果包含，说明这个信号在 handler 执行期间应被临时屏蔽，如果不包含，则允许嵌套处理。
                {
                    masked = false;    // yifan 2026/6/30: 当前 handler 的局部 mask 没有挡住这个信号，因此它在这一轮检查中可以继续向下处理。
                }
            }
            if !masked {    // yifan 2026/6/30: 只有当这个信号既满足全局未屏蔽，又没有被当前 handler 的局部 mask 挡住时，才真正进入后续处理流程。
                drop(task_inner);    // yifan 2026/6/30: 在调用后续处理函数前，先手动释放当前任务内部状态的独占借用；因为后面的处理函数还会再次访问当前任务内部数据。
                drop(task);    // yifan 2026/6/30: 同时释放当前这里持有的任务引用，避免和后续处理逻辑中的再次借用产生冲突。
                if signal == SignalFlags::SIGKILL
                    || signal == SignalFlags::SIGSTOP
                    || signal == SignalFlags::SIGCONT
                    || signal == SignalFlags::SIGDEF    // yifan 2026/6/30: 这几个信号被当作内核直接处理的特殊信号，不走用户态 handler，而是交给内核内置的信号语义去处理。
                {
                    // signal is a kernel signal
                    call_kernel_signal_handler(signal);    // yifan 2026/6/30: 对内核信号直接调用内核信号处理逻辑，例如 SIGSTOP 冻结进程、SIGCONT 恢复进程、致命类信号把进程标记为 killed。
                } else {
                    // signal is a user signal
                    call_user_signal_handler(sig, signal);    // yifan 2026/6/30: 对普通用户信号，按信号编号和信号标志去布置用户态 handler 的执行环境，例如保存 trap 上下文并把用户返回地址改到 handler 入口。
                    return;    // yifan 2026/6/30: 一旦决定处理一个用户信号，就立刻返回而不继续扫描后续信号；因为此时 trap 上下文已经被改写，需要先让这个用户 handler 真正运行起来，再谈后续信号。
                }
            }
        }
    }
}

// yifan 2026/6/30: 它的核心作用是反复处理当前进程的 pending signals，并在进程因为信号被冻结时把它挂起让出 CPU，直到它可以继续运行或者已经被标记为终止。
/// Handle signals for the current process
pub fn handle_signals() {    // yifan 2026/6/30: 这是给当前进程统一处理信号的入口函数；它不只是简单检查一次信号，还负责把“因信号冻结进程”与后续调度行为衔接起来。    
    loop {    // yifan 2026/6/30: 这里用循环，说明信号处理可能不是一轮就结束；如果当前进程因为信号被冻结了，就需要后续在再次被调度回来时继续检查。
        check_pending_signals();    // yifan 2026/6/30: 检查当前进程所有 pending signals，并按规则处理它们；这里可能会调用内核信号处理逻辑、调用用户注册的 signal handler，或者修改 frozen 和 killed 等状态。
        let (frozen, killed) = {    // yifan 2026/6/30: 处理完一轮信号后，读取当前任务的两个关键状态位：frozen 表示是否因信号被冻结，killed 表示是否已经被标记为应当终止。
            let task = current_task().unwrap();    // yifan 2026/6/30: 先取出当前 CPU 上正在运行的任务；按设计这里必须存在当前任务，否则就属于内核状态异常。
            let task_inner = task.inner_exclusive_access();    // yifan 2026/6/30: 因为要读取任务控制块内部的信号相关状态，所以需要先对任务内部数据做一次独占访问。
            (task_inner.frozen, task_inner.killed)    // yifan 2026/6/30: 把当前任务是否被冻结、是否已被标记为 killed 这两个状态打包取出，供后面的退出条件判断使用。
        };
        if !frozen || killed {    // yifan 2026/6/30: 如果当前进程已经不处于冻结状态，说明它可以继续往下执行；或者如果它已经被标记为 killed，也没必要继续在这里等待，因此这两种情况都退出循环。
            break;    // yifan 2026/6/30: 跳出 handle_signals 的循环，把后续控制流交还给 trap 处理收尾逻辑。
        }
        suspend_current_and_run_next();    // yifan 2026/6/30: 只有当 frozen=true 且 killed=false 时才会走到这里；这说明当前进程因信号被冻结但还没被终止，于是它需要主动让出 CPU，挂起自己并切换到下一个任务运行，等将来再次被调度回来后再继续检查。
    }
}
