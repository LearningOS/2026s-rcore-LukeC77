//! Task management implementation
//!
//! Everything about task management, like starting and switching tasks is
//! implemented here.
//!
//! A single global instance of [`TaskManager`] called `TASK_MANAGER` controls
//! all the tasks in the operating system.
//!
//! Be careful when you see `__switch` ASM function in `switch.S`. Control flow around this function
//! might not be what you expect.

mod context;
mod switch;
#[allow(clippy::module_inception)]
mod task;

use crate::loader::{get_app_data, get_num_app};
use crate::sync::UPSafeCell;
use crate::trap::TrapContext;
use alloc::vec::Vec;
use lazy_static::*;
use switch::__switch;
pub use task::{TaskControlBlock, TaskStatus};
use crate::mm::{MapPermission, VirtAddr}; // yifan 2026/5/15: 添加 VirtAddr 引入以便 sys_mmap 实现中使用

pub use context::TaskContext;

/// The task manager, where all the tasks are managed.
///
/// Functions implemented on `TaskManager` deals with all task state transitions
/// and task context switching. For convenience, you can find wrappers around it
/// in the module level.
///
/// Most of `TaskManager` are hidden behind the field `inner`, to defer
/// borrowing checks to runtime. You can see examples on how to use `inner` in
/// existing functions on `TaskManager`.
pub struct TaskManager {
    /// total number of tasks
    num_app: usize,
    /// use inner value to get mutable access
    inner: UPSafeCell<TaskManagerInner>,
}

/// The task manager inner in 'UPSafeCell'
struct TaskManagerInner {
    /// task list
    tasks: Vec<TaskControlBlock>,
    /// id of current `Running` task
    current_task: usize,
}

lazy_static! {
    /// a `TaskManager` global instance through lazy_static!
    pub static ref TASK_MANAGER: TaskManager = {
        println!("init TASK_MANAGER");
        let num_app = get_num_app();
        println!("num_app = {}", num_app);
        let mut tasks: Vec<TaskControlBlock> = Vec::new();
        for i in 0..num_app {
            tasks.push(TaskControlBlock::new(get_app_data(i), i));
        }
        TaskManager {
            num_app,
            inner: unsafe {
                UPSafeCell::new(TaskManagerInner {
                    tasks,
                    current_task: 0,
                })
            },
        }
    };
}

impl TaskManager {
    /// Run the first task in task list.
    ///
    /// Generally, the first task in task list is an idle task (we call it zero process later).
    /// But in ch4, we load apps statically, so the first task is a real app.
    fn run_first_task(&self) -> ! {
        let mut inner = self.inner.exclusive_access();
        let next_task = &mut inner.tasks[0];
        next_task.task_status = TaskStatus::Running;
        let next_task_cx_ptr = &next_task.task_cx as *const TaskContext;
        drop(inner);
        let mut _unused = TaskContext::zero_init();
        // before this, we should drop local variables that must be dropped manually
        unsafe {
            __switch(&mut _unused as *mut _, next_task_cx_ptr);
        }
        panic!("unreachable in run_first_task!");
    }

    /// Change the status of current `Running` task into `Ready`.
    fn mark_current_suspended(&self) {
        let mut inner = self.inner.exclusive_access();
        let cur = inner.current_task;
        inner.tasks[cur].task_status = TaskStatus::Ready;
    }

    /// Change the status of current `Running` task into `Exited`.
    fn mark_current_exited(&self) {
        let mut inner = self.inner.exclusive_access();
        let cur = inner.current_task;
        inner.tasks[cur].task_status = TaskStatus::Exited;
    }

    /// Find next task to run and return task id.
    ///
    /// In this case, we only return the first `Ready` task in task list.
    fn find_next_task(&self) -> Option<usize> {
        let inner = self.inner.exclusive_access();
        let current = inner.current_task;
        (current + 1..current + self.num_app + 1)
            .map(|id| id % self.num_app)
            .find(|id| inner.tasks[*id].task_status == TaskStatus::Ready)
    }

    /// Get the current 'Running' task's token.
    // yifan 2026/5/12: get_current_token 返回的是可直接写入 satp 的 token（MODE=Sv39 + root_ppn）。
    // yifan 2026/5/12: satp 对应当前任务（应用）地址空间的根页表标识；调度进行 task 切换时，
    // yifan 2026/5/12: 必须切换到目标任务对应的地址空间，因此需要取得并装载该 satp 值。
    fn get_current_token(&self) -> usize {
        let inner = self.inner.exclusive_access();
        inner.tasks[inner.current_task].get_user_token()
    }

    /// Get the current 'Running' task's trap contexts.
    fn get_current_trap_cx(&self) -> &'static mut TrapContext {
        let inner = self.inner.exclusive_access();    // yifan 2026/5/12: 获取任务管理器内部可独占访问的数据，用于读取当前运行任务信息。
        inner.tasks[inner.current_task].get_trap_cx()    // yifan 2026/5/12: 返回当前任务的 TrapContext 可变引用，供 trap 处理/返回阶段读写寄存器现场。
    }

    /// Change the current 'Running' task's program break
    pub fn change_current_program_brk(&self, size: i32) -> Option<usize> {
        let mut inner = self.inner.exclusive_access();
        let cur = inner.current_task;
        inner.tasks[cur].change_program_brk(size)
    }

    /// Switch current `Running` task to the task we have found,
    /// or there is no `Ready` task and we can exit with all applications completed
    fn run_next_task(&self) {
        if let Some(next) = self.find_next_task() {
            let mut inner = self.inner.exclusive_access();
            let current = inner.current_task;
            inner.tasks[next].task_status = TaskStatus::Running;
            inner.current_task = next;
            let current_task_cx_ptr = &mut inner.tasks[current].task_cx as *mut TaskContext;
            let next_task_cx_ptr = &inner.tasks[next].task_cx as *const TaskContext;
            drop(inner);
            // before this, we should drop local variables that must be dropped manually
            unsafe {
                __switch(current_task_cx_ptr, next_task_cx_ptr);
            }
            // go back to user mode
        } else {
            panic!("All applications completed!");
        }
    }

    /// yifan 2026/5/14 Increment syscall count for the current task
    fn increment_syscall_count(&self, syscall_id: usize) {
        let mut inner = self.inner.exclusive_access();
        let cur_task = inner.current_task;
        let task = &mut inner.tasks[cur_task]; // yifan 2026/5/14: 这里必须是可变引用才能修改taskcontrolblock中的syscall_count数组。不能直接取值，这样会将vector中的taskcontrolblock move出来。
        task.syscall_count[syscall_id] += 1;
    }

    /// yifan 2026/5/14 Get syscall count for the current task
    fn get_syscall_count(&self, syscall_id: usize) -> isize {
        let inner = self.inner.exclusive_access();
        let cur_task = &inner.tasks[inner.current_task];
        cur_task.syscall_count[syscall_id] as isize
    }

    // yifan 2026/5/15 mmap
    fn mmap(&self, start: usize, len: usize, port: usize) -> isize {
        let mut inner = self.inner.exclusive_access();
        let cur = inner.current_task;
        let cur_task = &mut inner.tasks[cur];
        let memory_set = &mut cur_task.memory_set;
        let start_va = VirtAddr(start);
        let end_va = VirtAddr(start + len);
        if memory_set.overlap(start_va, end_va) {
            return -1; // yifan 2026/5/15: 如果要映射的虚拟地址区间与当前内存空间已有的映射存在重叠，返回错误码 -1。
        }
        let mut perm = MapPermission::U;
        if port & 0b1 != 0 { // 可读
            perm |= MapPermission::R;
        }
        if port & 0b10 != 0 { // 可写
            perm |= MapPermission::W;
        }
        if port & 0b100 != 0 { // 可执行
            perm |= MapPermission::X;
        }
        memory_set.insert_framed_area(start_va, end_va, perm);
        0
    }

    // yifan 2026/5/15 munmap
    fn munmap(&self, start: usize, len: usize) -> isize {
        let mut inner = self.inner.exclusive_access();
        let cur = inner.current_task;
        let cur_task = &mut inner.tasks[cur];
        let memory_set = &mut cur_task.memory_set;
        let start_va = VirtAddr(start);
        let end_va = VirtAddr(start + len);

        trace!("munmap start={:#x} len={:#x} start_vpn={:?} end_vpn={:?}",
            start, len, start_va.floor(), end_va.ceil());

        if !memory_set.full_mapped(start_va, end_va) {
            return -1; // yifan 2026/5/15: 如果要解除映射的虚拟地址区间不是当前内存空间已有的映射的子区间，返回错误码 -1。
        }

        memory_set.unmap(start_va, end_va);
        0
    }
}

/// Run the first task in task list.
pub fn run_first_task() {
    TASK_MANAGER.run_first_task();
}

/// Switch current `Running` task to the task we have found,
/// or there is no `Ready` task and we can exit with all applications completed
fn run_next_task() {
    TASK_MANAGER.run_next_task();
}

/// Change the status of current `Running` task into `Ready`.
fn mark_current_suspended() {
    TASK_MANAGER.mark_current_suspended();
}

/// Change the status of current `Running` task into `Exited`.
fn mark_current_exited() {
    TASK_MANAGER.mark_current_exited();
}

/// Suspend the current 'Running' task and run the next task in task list.
pub fn suspend_current_and_run_next() {
    mark_current_suspended();
    run_next_task();
}

/// Exit the current 'Running' task and run the next task in task list.
pub fn exit_current_and_run_next() {
    mark_current_exited();
    run_next_task();
}

/// Get the current 'Running' task's token.
pub fn current_user_token() -> usize {
    TASK_MANAGER.get_current_token()
}

/// Get the current 'Running' task's trap contexts.
pub fn current_trap_cx() -> &'static mut TrapContext {
    TASK_MANAGER.get_current_trap_cx()
}

/// Change the current 'Running' task's program break
pub fn change_program_brk(size: i32) -> Option<usize> {
    TASK_MANAGER.change_current_program_brk(size)
}

/// yifan 2026/5/14 Increment syscall count for the current task
pub fn increment_syscall_count(syscall_id: usize) {
    TASK_MANAGER.increment_syscall_count(syscall_id);
}

/// yifan 2026/5/14 Get syscall count for the current task
pub fn get_syscall_count(syscall_id: usize) -> isize {
    TASK_MANAGER.get_syscall_count(syscall_id)
}

/// yifan 2026/5/15 mmap
pub fn mmap(start: usize, len: usize, port: usize) -> isize {
    TASK_MANAGER.mmap(start, len, port)
}

/// yifan 2026/5/15 munmap
pub fn munmap(start: usize, len: usize) -> isize {
    TASK_MANAGER.munmap(start, len)
}