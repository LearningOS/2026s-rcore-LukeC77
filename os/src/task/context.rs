//! Implementation of [`TaskContext`]

#[derive(Copy, Clone)]
#[repr(C)]
/// task context structure containing some registers
pub struct TaskContext {
    /// Ret position after task switching
    ra: usize,
    /// Stack pointer
    sp: usize,
    /// s0-11 register, callee saved
    s: [usize; 12],
}

impl TaskContext {
    /// Create a new empty task context
    pub fn zero_init() -> Self {
        Self {
            ra: 0,
            sp: 0,
            s: [0; 12],
        }
    }
    /// Create a new task context with a trap return addr and a kernel stack pointer
    pub fn goto_restore(kstack_ptr: usize) -> Self {
        extern "C" {    // yifan 2026/5/2: 声明外部汇编符号，后续把它作为新任务首次恢复后的跳转入口。
            fn __restore();    // yifan 2026/5/2: __restore 负责在任务上下文恢复后继续执行恢复流程。
        }
        Self {    // yifan 2026/5/2: 构造用于首次被调度的任务上下文。
            ra: __restore as usize,    // yifan 2026/5/2: 将返回地址设为 __restore，使 ret 后进入恢复入口。
            sp: kstack_ptr,    // yifan 2026/5/2: 将栈指针设为该任务的内核栈顶。
            s: [0; 12],    // yifan 2026/5/2: 将 s0-s11 清零初始化，作为初始被调用者保存寄存器状态。
        }
    }
}
