//!Implementation of [`TaskManager`]
use super::TaskControlBlock;
use crate::sync::UPSafeCell;
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use lazy_static::*;
///A array of `TaskControlBlock` that is thread-safe
pub struct TaskManager {
    ready_queue: VecDeque<Arc<TaskControlBlock>>,
}

/// Large stride constant used to compute pass = BIG_STRIDE / priority.
pub const BIG_STRIDE: usize = 65536;   // yifan 2026/5/28: 大步长常量，用于 stride 调度算法中计算每个任务的 stride 值，确保足够大以区分不同优先级的任务。

/// A simple FIFO scheduler.
impl TaskManager {
    ///Creat an empty TaskManager
    pub fn new() -> Self {
        Self {
            ready_queue: VecDeque::new(),
        }
    }
    /// Add process back to ready queue
    pub fn add(&mut self, task: Arc<TaskControlBlock>) {
        self.ready_queue.push_back(task);
    }
    /// Take a process out of the ready queue
    pub fn fetch(&mut self) -> Option<Arc<TaskControlBlock>> {
        let mut min_id: usize = 0;
        let mut min_stride = usize::MAX;
        for (id, task) in self.ready_queue.iter().enumerate() { // yifan 2026/5/28: 遍历就绪队列，找到 stride 值最小的任务进行调度。
            let inner = task.inner_exclusive_access();
            if inner.stride < min_stride {
                min_stride = inner.stride;
                min_id = id as usize;
            }
        }
        let fetched_task = self.ready_queue.remove(min_id as usize); // yifan 2026/5/28: 从就绪队列中移除选中的任务，准备调度执行。
        if let Some(task) = fetched_task {
            let mut inner = task.inner_exclusive_access();
            inner.stride += inner.pass; // yifan 2026/5/28: 调度后更新该任务的 stride 值，为下一次调度做准备。
            drop(inner); // yifan 2026/5/28: 释放任务内部独占访问，避免后续调度前持有锁。
            Some(task)
        } else {
            None
        }
        // self.ready_queue.pop_front()
    }
}

lazy_static! {
    /// TASK_MANAGER instance through lazy_static!
    pub static ref TASK_MANAGER: UPSafeCell<TaskManager> =
        unsafe { UPSafeCell::new(TaskManager::new()) };
}

/// Add process to ready queue
pub fn add_task(task: Arc<TaskControlBlock>) {
    //trace!("kernel: TaskManager::add_task");
    TASK_MANAGER.exclusive_access().add(task);
}

/// Take a process out of the ready queue
pub fn fetch_task() -> Option<Arc<TaskControlBlock>> {
    //trace!("kernel: TaskManager::fetch_task");
    TASK_MANAGER.exclusive_access().fetch()
}
