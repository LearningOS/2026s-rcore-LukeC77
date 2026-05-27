//!Implementation of [`Processor`] and Intersection of control flow
//!
//! Here, the continuous operation of user apps in CPU is maintained,
//! the current running state of CPU is recorded,
//! and the replacement and transfer of control flow of different applications are executed.

use super::__switch;
use super::{fetch_task, TaskStatus};
use super::{TaskContext, TaskControlBlock};
use crate::sync::UPSafeCell;
use crate::trap::TrapContext;
use alloc::sync::Arc;
use lazy_static::*;

/// Processor management structure
pub struct Processor {    // yifan 2026/5/24: 定义 Processor 结构体，作为处理器管理器，用于保存当前 CPU 的调度相关状态
    ///The task currently executing on the current processor
    current: Option<Arc<TaskControlBlock>>,    // yifan 2026/5/24: 表示当前正在该处理器上运行的任务；Option 表示可能有任务(Some)也可能没有任务(None)，Arc<TaskControlBlock> 用原子引用计数共享任务控制块所有权

    ///The basic control flow of each core, helping to select and switch process
    idle_task_cx: TaskContext,    // yifan 2026/5/24: 保存处理器空闲控制流上下文（调度器自身上下文），任务切换时在它与任务 TaskContext 间切换，以便任务让出 CPU 后返回此处继续调度
}

impl Processor {
    ///Create an empty Processor
    pub fn new() -> Self {    // yifan 2026/5/24: 定义公开关联函数 new，返回一个 Processor 实例（Self）
        Self {    // yifan 2026/5/24: 开始使用结构体字面量初始化 Processor 的各个字段
            current: None,    // yifan 2026/5/24: 初始状态下没有正在运行的任务，因此 current 设置为 None
            idle_task_cx: TaskContext::zero_init(),    // yifan 2026/5/24: 将处理器空闲上下文初始化为全零 TaskContext，作为后续调度与上下文切换的基准
        }
    }

    ///Get mutable reference to `idle_task_cx`
    fn get_idle_task_cx_ptr(&mut self) -> *mut TaskContext {
        &mut self.idle_task_cx as *mut _    // yifan 2026/5/24: 先获取 idle_task_cx 的可变引用，再转换为裸可变指针（*mut TaskContext，由 _ 自动推断）以便传给底层 unsafe 上下文切换接口
    }

    ///Get current task in moving semanteme
    pub fn take_current(&mut self) -> Option<Arc<TaskControlBlock>> {
        self.current.take()    // yifan 2026/5/24: 取走并返回 self.current 中的值（Option<Arc<TaskControlBlock>>），同时将 self.current 原地置为 None，实现所有权转移而非复制
    }

    ///Get current task in cloning semanteme
    pub fn current(&self) -> Option<Arc<TaskControlBlock>> {
        self.current.as_ref().map(Arc::clone)    // yifan 2026/5/24: 先用 as_ref 借用为 Option<&Arc<_>> 避免取走所有权，再用 map(Arc::clone) 在 Some 时克隆 Arc（仅增加引用计数）；None 则保持 None，因此不会清空 self.current
    }
}

lazy_static! {
    pub static ref PROCESSOR: UPSafeCell<Processor> = unsafe { UPSafeCell::new(Processor::new()) };
}

///The main part of process execution and scheduling
///Loop `fetch_task` to get the process that needs to run, and switch the process through `__switch`
pub fn run_tasks() {    // yifan 2026/5/24: 定义调度主函数，负责循环选择并运行任务
    loop {    // yifan 2026/5/24: 进入无限循环，持续尝试调度可运行任务
        let mut processor = PROCESSOR.exclusive_access();    // yifan 2026/5/24: 独占访问全局 PROCESSOR，读取并更新处理器调度状态
        if let Some(task) = fetch_task() {    // yifan 2026/5/24: 从就绪队列获取一个可运行任务，只有取到任务才执行上下文切换
            let idle_task_cx_ptr = processor.get_idle_task_cx_ptr();    // yifan 2026/5/24: 获取调度器自身上下文指针，作为 __switch 的 from 上下文
            // access coming task TCB exclusively
            let mut task_inner = task.inner_exclusive_access();    // yifan 2026/5/24: 独占访问将要运行任务的内部数据结构
            let next_task_cx_ptr = &task_inner.task_cx as *const TaskContext;    // yifan 2026/5/24: 获取目标任务上下文指针，作为 __switch 的 to 上下文
            task_inner.task_status = TaskStatus::Running;    // yifan 2026/5/24: 将任务状态标记为 Running，表示即将占用 CPU 运行
            // release coming task_inner manually
            drop(task_inner);    // yifan 2026/5/24: 手动释放任务内部独占访问，避免切换前持有锁
            // release coming task TCB manually
            processor.current = Some(task);    // yifan 2026/5/24: 把该任务登记为当前处理器正在运行的任务
            // release processor manually
            drop(processor);    // yifan 2026/5/24: 手动释放 PROCESSOR 独占访问，避免带锁进入上下文切换
            unsafe {    // yifan 2026/5/24: 进入 unsafe 块调用底层上下文切换例程
                __switch(idle_task_cx_ptr, next_task_cx_ptr);    // yifan 2026/5/24: 从调度器上下文切换到目标任务上下文，正式开始执行任务
            }
        } else {    // yifan 2026/5/24: 若本轮未取到可运行任务，进入空闲分支
            warn!("no tasks available in run_tasks");    // yifan 2026/5/24: 打印告警提示当前无任务可调度，然后继续下一轮循环
        }
    }
}

/// Get current task through take, leaving a None in its place
pub fn take_current_task() -> Option<Arc<TaskControlBlock>> {    // yifan 2026/5/24: 定义全局辅助函数，从 PROCESSOR 中取走当前任务并返回；Option 表示可能无任务
    PROCESSOR.exclusive_access().take_current()    // yifan 2026/5/24: 独占访问全局 PROCESSOR 后调用 take_current，取出 current 并将其置为 None（所有权转移）
}

/// Get a copy of the current task
pub fn current_task() -> Option<Arc<TaskControlBlock>> {    // yifan 2026/5/24: 定义全局辅助函数，获取当前任务的共享句柄而不移走任务本体
    PROCESSOR.exclusive_access().current()    // yifan 2026/5/24: 独占访问 PROCESSOR 后调用 current，内部克隆 Arc，因此 PROCESSOR.current 本身不变
}

/// Get the current user token(addr of page table)
pub fn current_user_token() -> usize {    // yifan 2026/5/24: 定义函数获取当前任务的用户态页表 token（地址空间标识）
    let task = current_task().unwrap();    // yifan 2026/5/24: 获取当前任务并强制解包；若当前无任务会 panic，隐含前提是调用时一定有运行任务
    task.get_user_token()    // yifan 2026/5/24: 从当前任务控制块读取并返回用户页表 token
}

///Get the mutable reference to trap context of current task
pub fn current_trap_cx() -> &'static mut TrapContext {    // yifan 2026/5/24: 定义函数获取当前任务 TrapContext 的可变引用，供 trap 处理阶段读写现场寄存器
    current_task()    // yifan 2026/5/24: 先拿到当前任务的 Arc 句柄
        .unwrap()    // yifan 2026/5/24: 强制解包 Option，要求当前必须存在任务
        .inner_exclusive_access()    // yifan 2026/5/24: 进入任务内部数据的独占可变访问区，保证并发安全
        .get_trap_cx()    // yifan 2026/5/24: 取出并返回该任务 trap 上下文的可变引用（生命周期可视为静态）
}

///Return to idle control flow for new scheduling
pub fn schedule(switched_task_cx_ptr: *mut TaskContext) {    // yifan 2026/5/24: 定义 schedule，用于把当前任务切回调度器；参数是被切出任务的上下文指针
    let mut processor = PROCESSOR.exclusive_access();    // yifan 2026/5/24: 独占访问全局 PROCESSOR，准备读取调度器自身上下文
    let idle_task_cx_ptr = processor.get_idle_task_cx_ptr();    // yifan 2026/5/24: 获取 idle 控制流（调度器）上下文指针，作为切换目标
    drop(processor);    // yifan 2026/5/24: 手动释放 PROCESSOR 独占访问，避免持锁执行上下文切换
    unsafe {    // yifan 2026/5/24: 进入 unsafe 块调用底层切换例程
        __switch(switched_task_cx_ptr, idle_task_cx_ptr);    // yifan 2026/5/24: 保存当前任务现场到 switched_task_cx_ptr，并恢复 idle_task_cx_ptr，返回调度循环
    }
}
