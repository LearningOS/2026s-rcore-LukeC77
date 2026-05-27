//! Process management syscalls
use alloc::sync::Arc;

use crate::{
    loader::get_app_data_by_name,
    mm::{translated_refmut, translated_str},
    task::{
        add_task, current_task, current_user_token, exit_current_and_run_next,
        suspend_current_and_run_next,
    },
};

#[repr(C)]
#[derive(Debug)]
pub struct TimeVal {
    pub sec: usize,
    pub usec: usize,
}

/// task exits and submit an exit code
pub fn sys_exit(exit_code: i32) -> ! { // yifan 2026/5/26:这里的 -> ! 表示这个函数不会正常返回。因为当前进程退出后，就会切换到其他进程，不会再回到原来的执行流。
    trace!("kernel:pid[{}] sys_exit", current_task().unwrap().pid.0);
    exit_current_and_run_next(exit_code);
    panic!("Unreachable in sys_exit!");
}

/// current task gives up resources for other tasks
pub fn sys_yield() -> isize {
    trace!("kernel:pid[{}] sys_yield", current_task().unwrap().pid.0);
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

pub fn sys_exec(path: *const u8) -> isize {
    trace!("kernel:pid[{}] sys_exec", current_task().unwrap().pid.0);
    let token = current_user_token();    // yifan 2026/5/26: 获取当前进程用户页表 token，用于翻译用户态传入的 path 指针。
    let path = translated_str(token, path);    // yifan 2026/5/26: 将用户态 C 字符串路径按页表翻译并拷贝为内核 String。
    if let Some(data) = get_app_data_by_name(path.as_str()) {    // yifan 2026/5/26: 按应用名查找对应 ELF 二进制数据。
        let task = current_task().unwrap();    // yifan 2026/5/26: 取当前任务（exec 语义是替换当前进程，而不是创建新进程）。
        task.exec(data);    // yifan 2026/5/26: 用新程序映像替换当前进程地址空间并重建返回用户态现场。
        0    // yifan 2026/5/26: 返回 0 表示 exec 成功。
    } else {
        -1    // yifan 2026/5/26: 返回 -1 表示未找到目标应用（exec 失败）。
    }
}

/// If there is not a child process whose pid is same as given, return -1.
/// Else if there is a child process but it is still running, return -2.
/// 
/// 获取当前父进程
/// → 查看 children 中有没有符合 pid 条件的子进程
/// → 如果没有，返回 -1
/// → 如果有，但都没退出，返回 -2
/// → 如果找到 Zombie 子进程
/// → 从 children 中移除它
/// → 确认它只剩当前这一份强引用
/// → 读取它的 PID 和 exit_code
/// → 把 exit_code 写回父进程用户空间
/// → 返回被回收子进程的 PID
/// → child 局部变量结束后，子进程 PCB 和剩余资源被彻底释放
pub fn sys_waitpid(pid: isize, exit_code_ptr: *mut i32) -> isize { // yifan 2026/5/26: pid：要等待的子进程 PID。exit_code_ptr：用户空间中保存退出码的位置
    trace!("kernel::pid[{}] sys_waitpid [{}]", current_task().unwrap().pid.0, pid);
    let task = current_task().unwrap();
    // find a child process

    // ---- access current PCB exclusively
    let mut inner = task.inner_exclusive_access();
    if !inner    // yifan 2026/5/26: 第一层先检查“是否存在至少一个符合 pid 条件的子进程”（此处不检查是否为 Zombie）。
        .children
        .iter()
        .any(|p| pid == -1 || pid as usize == p.getpid())    // yifan 2026/5/26: pid==-1 表示匹配任意子进程；否则仅匹配 pid 与子进程 getpid 相等者。
    {
        return -1;    // yifan 2026/5/26: 若一个匹配的子进程都没有，按 waitpid 语义返回 -1（不存在该子进程/不是其父进程）。
        // ---- release current PCB
    }
    let pair = inner.children.iter().enumerate().find(|(_, p)| {    // yifan 2026/5/26: 在 children 中查找“可立即回收”的目标，并保留下标与子进程引用（下标后续用于 remove）。
        // ++++ temporarily access child PCB exclusively
        p.inner_exclusive_access().is_zombie() && (pid == -1 || pid as usize == p.getpid())    // yifan 2026/5/26: 仅匹配已退出(Zombie)且 pid 条件满足的子进程：pid==-1 任意匹配，否则要求 pid 相等。
        // ++++ release child PCB
    });
    if let Some((idx, _)) = pair {    // yifan 2026/5/26: 若找到匹配且已退出的子进程，则进入回收路径。
        let child = inner.children.remove(idx);    // yifan 2026/5/26: 从父进程 children 列表移除目标子进程，开始实际回收。
        // confirm that child will be deallocated after being removed from children list
        assert_eq!(Arc::strong_count(&child), 1);    // yifan 2026/5/26: 断言仅剩当前这一个强引用，确保离开作用域后子进程可被释放。
        let found_pid = child.getpid();    // yifan 2026/5/26: 记录被回收子进程的 pid，作为 waitpid 成功返回值。
        // ++++ temporarily access child PCB exclusively
        let exit_code = child.inner_exclusive_access().exit_code;    // yifan 2026/5/26: 读取子进程退出码，后续写回父进程提供的用户地址。
        // ++++ release child PCB
        *translated_refmut(inner.memory_set.token(), exit_code_ptr) = exit_code;    // yifan 2026/5/26: 通过父进程页表翻译 exit_code_ptr 并写入退出码。
        found_pid as isize    // yifan 2026/5/26: 返回已回收子进程 pid（成功路径）。
    } else {
        -2    // yifan 2026/5/26: 存在匹配子进程但尚未退出（非 Zombie），暂不可回收，返回 -2 供用户库继续等待。
    }
    // ---- release current PCB automatically
}

/// YOUR JOB: get time with second and microsecond
/// HINT: You might reimplement it with virtual memory management.
/// HINT: What if [`TimeVal`] is splitted by two pages ?
pub fn sys_get_time(_ts: *mut TimeVal, _tz: usize) -> isize {
    trace!(
        "kernel:pid[{}] sys_get_time NOT IMPLEMENTED",
        current_task().unwrap().pid.0
    );
    -1
}

/// YOUR JOB: Implement mmap.
pub fn sys_mmap(_start: usize, _len: usize, _port: usize) -> isize {
    trace!(
        "kernel:pid[{}] sys_mmap NOT IMPLEMENTED",
        current_task().unwrap().pid.0
    );
    -1
}

/// YOUR JOB: Implement munmap.
pub fn sys_munmap(_start: usize, _len: usize) -> isize {
    trace!(
        "kernel:pid[{}] sys_munmap NOT IMPLEMENTED",
        current_task().unwrap().pid.0
    );
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
    trace!(
        "kernel:pid[{}] sys_spawn NOT IMPLEMENTED",
        current_task().unwrap().pid.0
    );
    -1
}

// YOUR JOB: Set task priority.
pub fn sys_set_priority(_prio: isize) -> isize {
    trace!(
        "kernel:pid[{}] sys_set_priority NOT IMPLEMENTED",
        current_task().unwrap().pid.0
    );
    -1
}
