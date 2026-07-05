//! Process management syscalls

use crate::{
    fs::{open_file, OpenFlags},
    mm::{translated_ref, translated_refmut, translated_str},
    task::{
        add_task, current_task, current_user_token, exit_current_and_run_next, pid2task,
        suspend_current_and_run_next, SignalAction, SignalFlags, MAX_SIG,
    },
};
use alloc::{string::String, sync::Arc, vec::Vec};

#[repr(C)]
#[derive(Debug)]
pub struct TimeVal {
    pub sec: usize,
    pub usec: usize,
}

pub fn sys_exit(exit_code: i32) -> ! {
    trace!("kernel:pid[{}] sys_exit",current_task().unwrap().pid.0);
    exit_current_and_run_next(exit_code);
    panic!("Unreachable in sys_exit!");
}

pub fn sys_yield() -> isize {
    //trace!("kernel: sys_yield");
    suspend_current_and_run_next();
    0
}

pub fn sys_getpid() -> isize {
	trace!("kernel: sys_getpid pid:{}", current_task().unwrap().pid.0);
    current_task().unwrap().pid.0 as isize
}

pub fn sys_fork() -> isize {
	trace!("kernel:pid[{}] sys_fork", current_task().unwrap().pid.0);
    let current_task = current_task().unwrap();
    let new_task = current_task.fork();
    let new_pid = new_task.pid.0;
    // modify trap context of new_task, because it returns immediately after switching
    let trap_cx = new_task.inner_exclusive_access().get_trap_cx();
    // we do not have to move to next instruction since we have done it before
    // for child process, fork returns 0
    trap_cx.x[10] = 0;
    // add new task to scheduler
    add_task(new_task);
    new_pid as isize
}

pub fn sys_exec(path: *const u8, mut args: *const usize) -> isize {
    trace!("kernel:pid[{}] sys_exec", current_task().unwrap().pid.0);
    let token = current_user_token();
    let path = translated_str(token, path);
    let mut args_vec: Vec<String> = Vec::new();
    loop { 
        // yifan 2026/6/22: 循环获取命令行参数，加到args_vec中。这里的 args 指向命令行参数字符串起始地址数组中的一个位置，
        // 每次我们都可以从一个起始地址通过 translated_str 拿到一个字符串，直到 args 为 0 就说明没有更多命令行参数了。
        let arg_str_ptr = *translated_ref(token, args);
        if arg_str_ptr == 0 {
            break;
        }
        args_vec.push(translated_str(token, arg_str_ptr as *const u8));
        unsafe {
            args = args.add(1);
        }
    }
    if let Some(app_inode) = open_file(path.as_str(), OpenFlags::RDONLY) {
        let all_data = app_inode.read_all();
        let task = current_task().unwrap();
        let argc = args_vec.len();
        task.exec(all_data.as_slice(), args_vec); // yifan 2026/6/22: 调用 TaskControlBlock::exec 的时候，我们需要将获取到的 args_vec 传入
        // return argc because cx.x[10] will be covered with it later
        argc as isize
    } else {
        -1
    }
}

/// If there is not a child process whose pid is same as given, return -1.
/// Else if there is a child process but it is still running, return -2.
pub fn sys_waitpid(pid: isize, exit_code_ptr: *mut i32) -> isize {
	//trace!("kernel: sys_waitpid");
    let task = current_task().unwrap();
    // find a child process

    // ---- access current PCB exclusively
    let mut inner = task.inner_exclusive_access();
    if !inner
        .children
        .iter()
        .any(|p| pid == -1 || pid as usize == p.getpid())
    {
        return -1;
        // ---- release current PCB
    }
    let pair = inner.children.iter().enumerate().find(|(_, p)| {
        // ++++ temporarily access child PCB exclusively
        p.inner_exclusive_access().is_zombie() && (pid == -1 || pid as usize == p.getpid())
        // ++++ release child PCB
    });
    if let Some((idx, _)) = pair {
        let child = inner.children.remove(idx);
        // confirm that child will be deallocated after being removed from children list
        assert_eq!(Arc::strong_count(&child), 1);
        let found_pid = child.getpid();
        // ++++ temporarily access child PCB exclusively
        let exit_code = child.inner_exclusive_access().exit_code;
        // ++++ release child PCB
        *translated_refmut(inner.memory_set.token(), exit_code_ptr) = exit_code;
        found_pid as isize
    } else {
        -2
    }
    // ---- release current PCB automatically
}

// yifan 2026/6/30: 它的核心作用是把指定信号挂到目标进程的 pending signals 集合中，表示该进程之后需要处理这个信号；它不会在这里立刻执行信号处理函数。
pub fn sys_kill(pid: usize, signum: i32) -> isize {    // yifan 2026/6/30: 这是内核里 kill 系统调用的实现；pid 是目标进程号，signum 是用户态传进来的信号编号，例如 SIGUSR1 对应 10。    
	trace!("kernel:pid[{}] sys_kill", current_task().unwrap().pid.0);    // yifan 2026/6/30: 这里只是记录调试日志，打印当前是哪个进程在调用 sys_kill，便于跟踪内核执行流程，不影响实际的信号发送逻辑。
    if let Some(task) = pid2task(pid) {    // yifan 2026/6/30: 先根据 pid 查找目标任务控制块；只有目标进程存在，后面才谈得上给它投递信号，否则直接走失败分支返回 -1。
        if let Some(flag) = SignalFlags::from_bits(1 << signum) {    // yifan 2026/6/30: 这里把“信号编号”转换成“信号位掩码”；例如 signum=10 时，1<<10 对应 SIGUSR1 在位图中的那一位，然后再检查这是不是一个合法的 SignalFlags。
            // insert the signal if legal
            let mut task_ref = task.inner_exclusive_access();    // yifan 2026/6/30: 接下来要修改目标进程内部保存的 pending signals，所以需要对它的内部状态做一次独占访问。
            if task_ref.signals.contains(flag) {    // yifan 2026/6/30: 如果这个信号已经在该进程的 pending signal 集合里，就不再重复插入；这个教学内核把 pending signal 当作一个集合，而不是允许同一信号排队多份。
                return -1;    // yifan 2026/6/30: 发现重复挂起同一个信号时返回 -1，表示这次 kill 失败。
            }
            task_ref.signals.insert(flag);    // yifan 2026/6/30: 把这个信号对应的位插入目标进程的 signals 位图中，表示该信号已经处于 pending 状态，后续会在合适的时机被真正处理。
            0    // yifan 2026/6/30: 能走到这里说明目标进程存在、信号合法且插入成功，因此返回 0 表示 kill 成功。
        } else {
            -1    // yifan 2026/6/30: from_bits 失败说明 signum 转不成合法的信号标志位，也就是用户传入了非法信号编号，所以返回 -1。
        }
    } else {
        -1    // yifan 2026/6/30: pid2task 没找到对应进程，说明目标 pid 不存在，因此这次发送信号失败并返回 -1。
    }
}

/// YOUR JOB: get time with second and microsecond
/// HINT: You might reimplement it with virtual memory management.
/// HINT: What if [`TimeVal`] is splitted by two pages ?
pub fn sys_get_time(_ts: *mut TimeVal, _tz: usize) -> isize {
    trace!("kernel:pid[{}] sys_get_time NOT IMPLEMENTED", current_task().unwrap().pid.0);
    -1
}

/// YOUR JOB: Implement mmap.
pub fn sys_mmap(_start: usize, _len: usize, _port: usize) -> isize {
    trace!("kernel:pid[{}] sys_mmap NOT IMPLEMENTED", current_task().unwrap().pid.0);
    -1
}

/// YOUR JOB: Implement munmap.
pub fn sys_munmap(_start: usize, _len: usize) -> isize {
    trace!("kernel:pid[{}] sys_munmap NOT IMPLEMENTED", current_task().unwrap().pid.0);
    -1
}

/// change data segment size
pub fn sys_sbrk(size: i32) -> isize {
    trace!("kernel:pid[{}] sys_sbrk", current_task().unwrap().pid.0);
    if let Some(old_brk) = current_task().unwrap().change_program_brk(size) {
        old_brk as isize
    } else {
        -1
    }
}

/// YOUR JOB: Implement spawn.
/// HINT: fork + exec =/= spawn
pub fn sys_spawn(_path: *const u8) -> isize {
    trace!("kernel:pid[{}] sys_spawn NOT IMPLEMENTED", current_task().unwrap().pid.0);
    -1
}

// YOUR JOB: Set task priority.
pub fn sys_set_priority(_prio: isize) -> isize {
    trace!("kernel:pid[{}] sys_set_priority NOT IMPLEMENTED", current_task().unwrap().pid.0);
    -1
}

pub fn sys_sigprocmask(mask: u32) -> isize {    // yifan 2026/6/27: 定义 sigprocmask 系统调用，参数 mask 是用户传入的信号屏蔽位图，返回值类型 isize 用于同时表示旧值或错误码。
    trace!("kernel:pid[{}] sys_sigprocmask", current_task().unwrap().pid.0);    // yifan 2026/6/27: 记录调试日志，说明当前进程正在调用 sys_sigprocmask。
    if let Some(task) = current_task() {    // yifan 2026/6/27: 先取出当前正在运行的任务；如果当前没有任务可操作，就不能设置屏蔽字。
        let mut inner = task.inner_exclusive_access();    // yifan 2026/6/27: 获取任务内部状态的独占访问权，因为后面要读取并修改 signal_mask。
        let old_mask = inner.signal_mask;    // yifan 2026/6/27: 先保存修改前的旧屏蔽字，后面成功时要把它返回给用户。
        if let Some(flag) = SignalFlags::from_bits(mask) {    // yifan 2026/6/27: 尝试把用户传入的 u32 位图转换成内核使用的 SignalFlags；若包含非法位则转换失败。
            inner.signal_mask = flag;    // yifan 2026/6/27: 将当前进程的信号屏蔽字更新为新的 flag，之后这些被屏蔽的信号不会立刻处理。
            old_mask.bits() as isize    // yifan 2026/6/27: 返回旧屏蔽字的原始位表示，符合“设置新值并返回旧值”的接口语义。
        } else {
            -1    // yifan 2026/6/27: 如果传入的 mask 不能构成合法的 SignalFlags，就返回 -1 表示参数错误。
        }
    } else {
        -1    // yifan 2026/6/27: 如果当前没有可用的任务上下文，也无法执行该系统调用，因此返回 -1。
    }
}

pub fn sys_sigreturn() -> isize {    // yifan 2026/6/30: 这是 sigreturn 系统调用的实现；它不是用来发送信号，而是供用户态 signal handler 执行结束后通知内核恢复被信号打断前的原始执行现场。
    trace!("kernel:pid[{}] sys_sigreturn", current_task().unwrap().pid.0);    // yifan 2026/6/30: 记录调试日志，说明当前进程正在调用 sys_sigreturn，便于跟踪信号处理函数返回后的内核执行流程。
    if let Some(task) = current_task() {    // yifan 2026/6/30: 正常情况下，调用 sigreturn 时一定存在当前正在运行的任务；如果连当前任务都拿不到，就只能走失败分支返回 -1。
        let mut inner = task.inner_exclusive_access();    // yifan 2026/6/30: 因为后面要修改当前任务内部保存的信号处理状态和 Trap 上下文，所以需要先对任务内部数据做一次独占访问。
        inner.handling_sig = -1;    // yifan 2026/6/30: 把“当前正在处理哪个信号”的标记清空，表示这个用户态 signal handler 已经执行结束，不再处于处理某个信号的状态。
        // restore the trap context
        let trap_ctx = inner.get_trap_cx();    // yifan 2026/6/30: 取出当前任务此刻使用的 Trap 上下文，后面要用之前备份的原始现场把它覆盖回去。
        *trap_ctx = inner.trap_ctx_backup.unwrap();    // yifan 2026/6/30: 用先前保存到 trap_ctx_backup 里的原始 Trap 上下文覆盖当前现场，恢复被信号打断前的寄存器值；这样 Trap 返回用户态后，程序就会像“先插队执行完 handler，再继续原来的执行”一样恢复运行。
        // Here we return the value of a0 in the trap_ctx,
        // otherwise it will be overwritten after we trap
        // back to the original execution of the application.
        trap_ctx.x[10] as isize    // yifan 2026/6/30: 返回恢复后 Trap 上下文里的 x[10] 也就是 a0 的值；如果这里不把它作为 sys_sigreturn 的返回值带出来，后续系统调用返回路径可能会改写 a0，从而破坏恢复后的用户寄存器现场一致性。
    } else {
        -1    // yifan 2026/6/30: 如果当前没有可用任务上下文，就无法恢复任何用户现场，因此返回 -1 表示 sigreturn 失败。
    }
}

fn check_sigaction_error(signal: SignalFlags, action: usize, old_action: usize) -> bool {    // yifan 2026/6/27: 定义一个辅助检查函数，用来判断本次 sigaction 的参数和目标信号是否合法。
    if action == 0    // yifan 2026/6/27: action 是新 SignalAction 的用户态地址，转成 usize 后若等于 0，就表示传入的是空指针。
        || old_action == 0    // yifan 2026/6/27: old_action 是保存旧 SignalAction 的用户态地址，若为 0，同样表示空指针，后续不能安全写回。
        || signal == SignalFlags::SIGKILL    // yifan 2026/6/27: SIGKILL 不允许用户安装自定义 handler，它必须保持由内核强制终止进程的语义。
        || signal == SignalFlags::SIGSTOP    // yifan 2026/6/27: SIGSTOP 也不允许用户安装自定义 handler，它必须保持由内核强制停止进程的语义。
    {    // yifan 2026/6/27: 只要上述任一条件成立，就说明这次 sigaction 调用参数非法。
        true    // yifan 2026/6/27: 返回 true 表示检测到错误，调用者应当拒绝这次 sigaction 请求。
    } else {
        false    // yifan 2026/6/27: 返回 false 表示参数检查通过，可以继续执行后续的 handler 安装流程。
    }
}

/// yifan 2026/6/25
/// 功能：为当前进程设置某种信号的处理函数，同时保存设置之前的处理函数。
/// 参数：signum 表示信号的编号，action 表示要设置成的处理函数的指针
/// old_action 表示用于保存设置之前的处理函数的指针（SignalAction 结构稍后介绍）。
/// 返回值：如果传入参数错误（比如传入的 action 或 old_action 为空指针或者）
/// 信号类型不存在返回 -1 ，否则返回 0 。
/// syscall ID: 134
/// 典型场景：
/// 原来 SIGUSR1 的 handler 是 A
/// 某段代码想暂时改成 B
/// 做完以后恢复成 A
pub fn sys_sigaction(
    signum: i32,
    action: *const SignalAction,
    old_action: *mut SignalAction,
) -> isize {
    trace!("kernel:pid[{}] sys_sigaction", current_task().unwrap().pid.0);
    let token = current_user_token();
    let task = current_task().unwrap();
    let mut inner = task.inner_exclusive_access();
    if signum as usize > MAX_SIG {    // yifan 2026/6/27: 先检查信号编号是否超过当前内核支持的最大范围；本实现只接受 0..=MAX_SIG，超出范围就属于非法参数。
        return -1;    // yifan 2026/6/27: 若 signum 过大，后续既可能导致 signal_actions.table[signum as usize] 下标越界，也无法构造出合法的 SignalFlags，因此直接返回 -1 报错。
    }
    if let Some(flag) = SignalFlags::from_bits(1 << signum) {    // yifan 2026/6/27: 把 signum 转成对应的信号位标志；若该位模式不是当前内核认可的合法信号，则转换失败并走后面的 else。
        if check_sigaction_error(flag, action as usize, old_action as usize) {    // yifan 2026/6/27: 继续检查参数是否合法，主要包括 action 和 old_action 是否为空指针，以及 signal 是否为不允许用户注册 handler 的 SIGKILL 或 SIGSTOP。
            return -1;    // yifan 2026/6/27: 如果参数检查失败，就立即返回 -1，拒绝这次 sigaction 调用。
        }
        let prev_action = inner.signal_actions.table[signum as usize];    // yifan 2026/6/27: 先从当前进程的 signal_actions 表中取出该信号原先对应的处理动作，便于稍后返回给用户。
        *translated_refmut(token, old_action) = prev_action;    // yifan 2026/6/27: 根据当前用户地址空间 token 把 old_action 用户态地址翻译成内核可写引用；这里操作的不是 TaskControlBlockInner 的某个字段，而是用户传入的一个输出缓冲区地址，然后把旧的 SignalAction 写回这块用户空间内存。
        inner.signal_actions.table[signum as usize] = *translated_ref(token, action);    // yifan 2026/6/27: 从用户传入的 action 地址读取新的 SignalAction，并把它安装到当前进程该信号对应的处理动作表项中。
        0    // yifan 2026/6/27: 走到这里说明旧动作已成功保存且新动作已成功安装，因此返回 0 表示调用成功。
    } else {
        -1    // yifan 2026/6/27: 如果 signum 虽未越界但无法构造出合法的 SignalFlags，就返回 -1，表示该信号类型对当前系统而言无效。
    }
}
