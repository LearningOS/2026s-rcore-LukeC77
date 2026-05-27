//! Task management implementation
//!
//! Everything about task management, like starting and switching tasks is
//! implemented here.
//!
//! A single global instance of [`TaskManager`] called `TASK_MANAGER` controls
//! all the tasks in the whole operating system.
//!
//! A single global instance of [`Processor`] called `PROCESSOR` monitors running
//! task(s) for each core.
//!
//! A single global instance of `PID_ALLOCATOR` allocates pid for user apps.
//!
//! Be careful when you see `__switch` ASM function in `switch.S`. Control flow around this function
//! might not be what you expect.
mod context;
mod id;
mod manager;
mod processor;
mod switch;
#[allow(clippy::module_inception)]
mod task;

use crate::loader::get_app_data_by_name;
use alloc::sync::Arc;
use lazy_static::*;
pub use manager::{fetch_task, TaskManager};
use switch::__switch;
pub use task::{TaskControlBlock, TaskStatus};

pub use context::TaskContext;
pub use id::{kstack_alloc, pid_alloc, KernelStack, PidHandle};
pub use manager::add_task;
pub use processor::{
    current_task, current_trap_cx, current_user_token, run_tasks, schedule, take_current_task,
    Processor,
};
/// Suspend the current 'Running' task and run the next task in task list.
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
    // yifan 2026/5/25: 这里调用 schedule(task_cx_ptr) 的作用是把当前任务切回 idle/scheduler 上下文。
    // yifan 2026/5/25: schedule 内部执行 __switch(switched_task_cx_ptr, idle_task_cx_ptr)，保存当前任务现场并恢复 idle 现场。
    // yifan 2026/5/25: 切回 idle 后并不会“再次显式调用 run_tasks”，而是回到 run_tasks 里之前 __switch(idle, next) 之后的位置继续 loop。
    // yifan 2026/5/25: run_tasks 在循环中 fetch_task() 从 ready_queue 取下一个任务，再 __switch(idle, next_task) 跳到新任务执行。
    // yifan 2026/5/25: 因此控制流是 task --schedule--> idle(run_tasks loop) --fetch/switch--> next task；run_tasks 仅在 rust_main 启动时显式调用一次。
    schedule(task_cx_ptr);
}

/// pid of usertests app in make run TEST=1
pub const IDLE_PID: usize = 0;    // yifan 2026/5/26: idle 不是业务进程，而是调度器锚点上下文；任务让出 CPU 时需切回它，再由 run_tasks 继续选下一个任务。

/// Exit the current 'Running' task and run the next task in task list.
/// 当前进程退出
/// → 从 Processor 中取出当前进程
/// → 状态改成 Zombie
/// → 保存 exit_code
/// → 把它的子进程交给 initproc
/// → 清空自己的 children
/// → 回收用户地址空间中的数据页
/// → 释放当前任务引用
/// → 不保存当前上下文
/// → 调用 schedule 切换到下一个进程
pub fn exit_current_and_run_next(exit_code: i32) {
    // take from Processor
    let task = take_current_task().unwrap();    // yifan 2026/5/26: 从 PROCESSOR.current 取走当前运行任务；unwrap 表示此路径下必须存在正在运行的任务。

    let pid = task.getpid();    // yifan 2026/5/26: 读取当前任务 PID，用于判断是否是 idle 进程。
    if pid == IDLE_PID {    // yifan 2026/5/26: 若退出者是 idle(PID=0)，语义上表示系统已无业务任务可继续运行，进入统一收尾路径。
        println!(
            "[kernel] Idle process exit with exit_code {} ...",
            exit_code
        );
        panic!("All applications completed!");    // yifan 2026/5/26: 通过 panic 明确终止内核执行，作为“所有应用完成”的结束信号。
    }

    // **** access current TCB exclusively
    let mut inner = task.inner_exclusive_access();
    // Change status to Zombie
    inner.task_status = TaskStatus::Zombie;
    // Record exit code
    inner.exit_code = exit_code;
    // do not move to its parent but under initproc

    // ++++++ access initproc TCB exclusively
    {    // yifan 2026/5/26: 进入过继流程，将当前退出进程的子进程统一托管给 initproc。
        let mut initproc_inner = INITPROC.inner_exclusive_access();    // yifan 2026/5/26: 独占访问 initproc 内部状态，准备更新其 children 列表。
        for child in inner.children.iter() {    // yifan 2026/5/26: 遍历当前退出进程的所有子进程。
            child.inner_exclusive_access().parent = Some(Arc::downgrade(&INITPROC));    // yifan 2026/5/26: 将子进程 parent 改为 INITPROC（弱引用，避免循环引用）。
            initproc_inner.children.push(child.clone());    // yifan 2026/5/26: 把子进程句柄加入 initproc.children，后续可由 initproc wait 回收。
        }
    }    // yifan 2026/5/26: 作用域结束后释放 initproc 的独占访问。
    // ++++++ release parent PCB

    inner.children.clear();
    // deallocate user space
    inner.memory_set.recycle_data_pages();
    drop(inner);
    // **** release current PCB
    // drop task manually to maintain rc correctly
    drop(task);
    // we do not have to save task context
    let mut _unused = TaskContext::zero_init();    // yifan 2026/5/26: 当前进程即将退出且不会再被恢复，构造一个占位 TaskContext 仅用于满足 schedule 的参数类型。
    schedule(&mut _unused as *mut _);    // yifan 2026/5/26: 将该占位上下文指针传入后切回 idle/scheduler；真正目的是触发调度，不是保留当前退出进程现场。
}

lazy_static! {
    /// Creation of initial process
    ///
    /// the name "initproc" may be changed to any other app name like "usertests",
    /// but we have user_shell, so we don't need to change it.
    pub static ref INITPROC: Arc<TaskControlBlock> = Arc::new(TaskControlBlock::new(    // yifan 2026/5/25: 定义全局懒初始化静态变量 INITPROC，类型为 Arc<TaskControlBlock>，表示初始进程共享句柄
        get_app_data_by_name("ch5b_initproc").unwrap()    // yifan 2026/5/25: 按名称获取 ch5b_initproc 的应用二进制数据；unwrap 表示若不存在则直接 panic，默认它必须存在
    ));
}

///Add init process to the manager
pub fn add_initproc() {    // yifan 2026/5/25: 提供函数将初始进程加入任务管理器（就绪队列）
    add_task(INITPROC.clone());    // yifan 2026/5/25: 克隆 INITPROC 的 Arc 后加入任务队列，仅增加引用计数，不复制任务控制块
}
