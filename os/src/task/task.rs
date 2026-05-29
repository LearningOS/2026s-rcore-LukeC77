//! Types related to task management & Functions for completely changing TCB
use super::TaskContext;
use super::{kstack_alloc, pid_alloc, KernelStack, PidHandle};
use crate::config::TRAP_CONTEXT_BASE;
use crate::mm::{MemorySet, PhysPageNum, VirtAddr, KERNEL_SPACE};
use crate::sync::UPSafeCell;
use crate::trap::{trap_handler, TrapContext};
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::cell::RefMut;
use super::manager::BIG_STRIDE; // yifan 2026/5/28: 引入调度算法相关常量，供 TCB 内字段初始化使用。

/// Task control block structure
///
/// Directly save the contents that will not change during running
pub struct TaskControlBlock {    // yifan 2026/5/23: 进程控制块（PCB）外层：承载进程身份与核心资源句柄。
    // Immutable
    /// Process identifier
    pub pid: PidHandle,    // yifan 2026/5/23: 进程标识 PID（创建后通常不变）。

    /// Kernel stack corresponding to PID
    pub kernel_stack: KernelStack,    // yifan 2026/5/23: 该进程对应的内核栈资源句柄（创建后通常不变）。

    /// Mutable
    inner: UPSafeCell<TaskControlBlockInner>,    // yifan 2026/5/23: 运行时可变状态放在 inner 中，并通过 UPSafeCell 提供内部可变性。
}

impl TaskControlBlock {
    /// Get the mutable reference of the inner TCB
    pub fn inner_exclusive_access(&self) -> RefMut<'_, TaskControlBlockInner> {
        self.inner.exclusive_access()
    }
    /// Get the address of app's page table
    pub fn get_user_token(&self) -> usize {
        let inner = self.inner_exclusive_access();
        inner.memory_set.token()
    }
}

pub struct TaskControlBlockInner {    // yifan 2026/5/23: PCB 可变核心字段：描述进程运行态、地址空间和进程关系。
    /// The physical page number of the frame where the trap context is placed
    pub trap_cx_ppn: PhysPageNum,    // yifan 2026/5/23: TrapContext 所在物理页号，用于陷入/返回时定位寄存器现场。

    /// Application data can only appear in areas
    /// where the application address space is lower than base_size
    pub base_size: usize,    // yifan 2026/5/23: 用户地址空间基础边界（与应用初始空间大小相关）。

    /// Save task context
    pub task_cx: TaskContext,    // yifan 2026/5/23: 调度切换使用的任务上下文保存区。

    /// Maintain the execution status of the current process
    pub task_status: TaskStatus,    // yifan 2026/5/23: 进程当前状态（如 Ready/Running/Zombie）。

    /// Application address space
    pub memory_set: MemorySet,    // yifan 2026/5/23: 进程私有地址空间（页表与各内存映射）。

    /// Parent process of the current process.
    /// Weak will not affect the reference count of the parent
    pub parent: Option<Weak<TaskControlBlock>>,    // yifan 2026/5/23: 父进程弱引用，不增加强引用计数，避免父子循环引用。

    /// A vector containing TCBs of all child processes of the current process
    pub children: Vec<Arc<TaskControlBlock>>,    // yifan 2026/5/23: 子进程强引用列表，父进程可在 wait 时读取并回收子进程信息。

    /// It is set when active exit or execution error occurs
    pub exit_code: i32,    // yifan 2026/5/23: 进程退出码，供父进程回收时获取。

    /// Heap bottom
    pub heap_bottom: usize,    // yifan 2026/5/23: 用户堆起始位置（初始 brk 基准）。

    /// Program break
    pub program_brk: usize,    // yifan 2026/5/23: 当前 program break，随 sbrk/brk 调整而变化。

    pub stride: usize,    // yifan 2026/5/28: 进程 stride 值，表示该进程当前已经运行的“长度”。

    pub priority: usize,   // yifan 2026/5/28: 进程优先级数值。

    pub pass: usize,   // yifan 2026/5/28: 进程 pass 值，pass = BIG_STRIDE / priority，表示对应进程在调度后，stride 需要进行的累加值。

}

impl TaskControlBlockInner {
    /// get the trap context
    pub fn get_trap_cx(&self) -> &'static mut TrapContext {
        self.trap_cx_ppn.get_mut()    // yifan 2026/5/23: 通过保存的 TrapContext 物理页号定位并返回可变引用，供内核读写用户陷入现场（寄存器上下文）。
    }
    /// get the user token
    pub fn get_user_token(&self) -> usize {
        self.memory_set.token()    // yifan 2026/5/23: 返回该进程地址空间的页表 token（通常用于 satp 切换），调度到该进程时据此启用其用户地址空间。
    }
    fn get_status(&self) -> TaskStatus {
        self.task_status
    }
    pub fn is_zombie(&self) -> bool {
        self.get_status() == TaskStatus::Zombie
    }
}

impl TaskControlBlock {
    /// Create a new process
    ///
    /// At present, it is only used for the creation of initproc
    /// TaskControlBlock::new 的核心流程是：
    // 解析 ELF
    // -> 创建用户地址空间
    // -> 找到 TrapContext 物理页
    // -> 分配 PID
    // -> 创建内核栈
    // -> 初始化 TaskContext
    // -> 创建 PCB
    // -> 初始化 TrapContext
    // -> 返回 TaskControlBlock
    pub fn new(elf_data: &[u8]) -> Self {    // yifan 2026/5/25: 定义构造函数，根据 ELF 字节数据创建并返回一个新的任务控制块
        // memory_set with elf program headers/trampoline/trap context/user stack
        let (memory_set, user_sp, entry_point) = MemorySet::from_elf(elf_data);    // yifan 2026/5/25: 解析 ELF 并建立用户地址空间，得到 memory_set、用户栈顶 user_sp 和程序入口 entry_point
        let trap_cx_ppn = memory_set    // yifan 2026/5/25: 通过页表查询 TrapContext 虚拟地址，定位其所在物理页号以便后续内核写入
            .translate(VirtAddr::from(TRAP_CONTEXT_BASE).into())    // yifan 2026/5/25: 将 TRAP_CONTEXT_BASE 对应虚拟地址转换为页表查询所需类型并执行地址翻译
            .unwrap()    // yifan 2026/5/25: 假设该映射必须存在；若不存在则直接 panic
            .ppn();    // yifan 2026/5/25: 取翻译结果中的物理页号 PPN
        // alloc a pid and a kernel stack in kernel space
        let pid_handle = pid_alloc();    // yifan 2026/5/25: 为新进程分配唯一 PID 句柄
        let kernel_stack = kstack_alloc();    // yifan 2026/5/25: 为新进程分配内核栈（进入内核态时使用）
        let kernel_stack_top = kernel_stack.get_top();    // yifan 2026/5/25: 获取该内核栈的栈顶地址，供初始化任务上下文与 TrapContext
        // push a task context which goes to trap_return to the top of kernel stack
        let task_control_block = Self {    // yifan 2026/5/25: 开始构造 TaskControlBlock，本体包含调度与地址空间所需全部状态
            pid: pid_handle,    // yifan 2026/5/25: 记录该任务的 PID
            kernel_stack,    // yifan 2026/5/25: 记录该任务绑定的内核栈
            inner: unsafe {
                UPSafeCell::new(TaskControlBlockInner {    // yifan 2026/5/25: 使用 UPSafeCell 封装内部可变状态，支持受控独占访问
                    trap_cx_ppn,    // yifan 2026/5/25: 保存 TrapContext 所在物理页号
                    base_size: user_sp,    // yifan 2026/5/25: 记录用户地址空间初始大小边界（以用户栈顶为基准）
                    task_cx: TaskContext::goto_trap_return(kernel_stack_top),    // yifan 2026/5/25: 初始化任务上下文，使首次调度先跳到 trap_return 再进入用户态
                    task_status: TaskStatus::Ready,    // yifan 2026/5/25: 新建任务初始状态设为 Ready，表示可被调度
                    memory_set,    // yifan 2026/5/25: 持有该任务独立用户地址空间
                    parent: None,    // yifan 2026/5/25: 当前新建进程初始无父进程引用
                    children: Vec::new(),    // yifan 2026/5/25: 子进程列表初始为空
                    exit_code: 0,    // yifan 2026/5/25: 退出码初始为 0
                    heap_bottom: user_sp,    // yifan 2026/5/25: 堆底初始设为用户栈顶位置对应边界
                    program_brk: user_sp,    // yifan 2026/5/25: 程序 break 初始值与 heap_bottom 一致，供后续堆扩展
                    stride: 0,    // yifan 2026/5/28: 新建任务初始 stride 为 0，表示尚未运行过
                    priority: 16,    // yifan 2026/5/28: 新建任务默认优先级设为 16（范围 1-256），供后续调度算法使用；实际值可根据需要调整。
                    pass: BIG_STRIDE / 16,    // yifan 2026/5/28: 根据默认优先级计算初始 pass 值，供 stride 调度算法使用；实际计算可根据调度算法设计调整。
                })
            },
        };
        // prepare TrapContext in user space
        let trap_cx = task_control_block.inner_exclusive_access().get_trap_cx();    // yifan 2026/5/25: 获取该任务 TrapContext 的可变引用，准备写入首次进入用户态所需寄存器现场
        *trap_cx = TrapContext::app_init_context(    // yifan 2026/5/25: 构造并写入初始 TrapContext，定义用户态入口与陷入内核返回路径
            entry_point,    // yifan 2026/5/25: 用户程序入口地址
            user_sp,    // yifan 2026/5/25: 用户栈栈顶
            KERNEL_SPACE.exclusive_access().token(),    // yifan 2026/5/25: 内核地址空间 token，用于陷入内核后的地址空间切换语义
            kernel_stack_top,    // yifan 2026/5/25: 当前任务内核栈顶，供 trap 进入内核时使用
            trap_handler as usize,    // yifan 2026/5/25: trap 处理函数入口地址，系统调用/异常/中断时跳入
        );
        task_control_block
    }

    /// Load a new elf to replace the original application address space and start execution
    pub fn exec(&self, elf_data: &[u8]) {    // yifan 2026/5/26: exec 位于 syscall 处理链中：用户态 ecall -> trap_handler(UserEnvCall) -> syscall(...) -> sys_exec -> TaskControlBlock::exec(...)。
        // memory_set with elf program headers/trampoline/trap context/user stack
        let (memory_set, user_sp, entry_point) = MemorySet::from_elf(elf_data);    // yifan 2026/5/26: 这里是根据传入 ELF 二进制数据重建用户地址空间，并得到新用户栈顶与入口地址，不是直接执行程序。
        let trap_cx_ppn = memory_set
            .translate(VirtAddr::from(TRAP_CONTEXT_BASE).into())
            .unwrap()
            .ppn();    // yifan 2026/5/26: 重新定位新地址空间中的 TrapContext 物理页号；exec 替换 memory_set 后旧 trap_cx_ppn 失效，必须更新。

        // **** access current TCB exclusively
        let mut inner = self.inner_exclusive_access();
        // substitute memory_set
        inner.memory_set = memory_set;    // yifan 2026/5/26: 用新程序地址空间替换旧地址空间，后续用户态将不再执行旧程序映像。
        // update trap_cx ppn
        inner.trap_cx_ppn = trap_cx_ppn;    // yifan 2026/5/26: 更新当前进程保存 trap 上下文位置的缓存，保证内核后续读写寄存器现场指向新地址空间。
        // initialize base_size
        inner.base_size = user_sp;
        // initialize trap_cx
        let trap_cx = inner.get_trap_cx();
        *trap_cx = TrapContext::app_init_context(    // yifan 2026/5/26: exec 返回后控制流回到 trap_handler，随后由 trap_handler 末尾统一调用 trap_return->__restore->sret；这里先把 sepc/sp 等现场改为新程序初始状态。
            entry_point,    // yifan 2026/5/26: 将用户 PC(sepc) 设为新程序入口，返回用户态后从这里开始执行。
            user_sp,    // yifan 2026/5/26: 将用户 SP 设为新栈顶，匹配新程序运行时栈布局。
            KERNEL_SPACE.exclusive_access().token(),
            self.kernel_stack.get_top(),
            trap_handler as usize,
        );
        // **** release inner automatically
    }

    /// parent process fork the child process
    /// yifan 2026/5/26: fork和new基本一致，但 fork 是在已有父进程基础上创建子进程，涉及复制父进程地址空间、共享代码段、独立数据段等细节。
    ///yifan 2026/5/26: 调用链为“用户态 ecall -> trap_handler(UserEnvCall) -> syscall(...) -> sys_fork -> TaskControlBlock::fork()”，因此 ecall 不会出现在本函数内部。
    pub fn fork(self: &Arc<Self>) -> Arc<Self> {    // yifan 2026/5/26: 接收者用 &Arc<Self> 而非 &Self，因为后续需要 Arc::downgrade(self) 生成指向父进程的 Weak 引用写入子进程 parent 字段。
        // ---- access parent PCB exclusively
        let mut parent_inner = self.inner_exclusive_access();
        // copy user space(include trap context)
        let memory_set = MemorySet::from_existed_user(&parent_inner.memory_set); // yifan 2026/5/26: 子进程的地址空间不是通过解析 ELF 文件，而是通过在第 8 行调用 MemorySet::from_existed_user 复制父进程地址空间得到的；
        let trap_cx_ppn = memory_set
            .translate(VirtAddr::from(TRAP_CONTEXT_BASE).into())
            .unwrap()
            .ppn();
        // alloc a pid and a kernel stack in kernel space
        let pid_handle = pid_alloc();
        let kernel_stack = kstack_alloc();
        let kernel_stack_top = kernel_stack.get_top();
        let task_control_block = Arc::new(TaskControlBlock {    // yifan 2026/5/26: 使用 Arc::new 是因为 TCB 需要被就绪队列、处理器 current、父进程 children 等多处共享持有；Arc 提供共享所有权与正确生命周期管理。
            pid: pid_handle,
            kernel_stack,
            inner: unsafe {
                UPSafeCell::new(TaskControlBlockInner {
                    trap_cx_ppn,
                    base_size: parent_inner.base_size,
                    task_cx: TaskContext::goto_trap_return(kernel_stack_top),
                    task_status: TaskStatus::Ready,
                    memory_set,
                    parent: Some(Arc::downgrade(self)),    // yifan 2026/5/26: Arc::downgrade 的参数类型是 &Arc<T>，这也是 fork 签名使用 self: &Arc<Self> 的直接原因。
                    children: Vec::new(),
                    exit_code: 0,
                    heap_bottom: parent_inner.heap_bottom,    // yifan 2026/5/26: 继承的是堆边界虚拟地址数值而非父进程地址空间引用；子进程已有独立 memory_set，故不会指向父进程物理页。
                    program_brk: parent_inner.program_brk,    // yifan 2026/5/26: 继承当前 brk 仅保持 fork 后堆区间语义一致；相同 VA 在父子中可映射到不同物理页（本实现为深拷贝而非 COW）。
                    stride: 0,    // yifan 2026/5/28: 新建子进程初始 stride 为 0，表示尚未运行过。
                    priority: 16,    // yifan 2026/5/28: 新建子进程默认优先级设为 16（范围 1-256），供后续调度算法使用；实际值可根据需要调整。
                    pass: BIG_STRIDE / 16,    // yifan 2026/5/28: 根据默认优先级计算初始 pass 值。
                })
            },
        });
        // add child
        parent_inner.children.push(task_control_block.clone());    // yifan 2026/5/26: 这里 clone Arc 是为了同时满足两处所有权：一份放入父进程 children，另一份保留给函数后续返回；该 clone 仅增加引用计数，不会深拷贝 TCB。
        // modify kernel_sp in trap_cx
        // **** access child PCB exclusively
        let trap_cx = task_control_block.inner_exclusive_access().get_trap_cx();
        trap_cx.kernel_sp = kernel_stack_top;
        // return
        task_control_block
        // **** release child PCB
        // ---- release parent PCB
    }

    /// get pid of process
    pub fn getpid(&self) -> usize {
        self.pid.0
    }

    /// change the location of the program break. return None if failed.
    pub fn change_program_brk(&self, size: i32) -> Option<usize> {
        let mut inner = self.inner_exclusive_access();
        let heap_bottom = inner.heap_bottom;
        let old_break = inner.program_brk;
        let new_brk = inner.program_brk as isize + size as isize;
        if new_brk < heap_bottom as isize {
            return None;
        }
        let result = if size < 0 {
            inner
                .memory_set
                .shrink_to(VirtAddr(heap_bottom), VirtAddr(new_brk as usize))
        } else {
            inner
                .memory_set
                .append_to(VirtAddr(heap_bottom), VirtAddr(new_brk as usize))
        };
        if result {
            inner.program_brk = new_brk as usize;
            Some(old_break)
        } else {
            None
        }
    }
}

#[derive(Copy, Clone, PartialEq)]
/// task status: UnInit, Ready, Running, Exited
pub enum TaskStatus {
    /// uninitialized
    UnInit,
    /// ready to run
    Ready,
    /// running
    Running,
    /// exited
    Zombie,
}
