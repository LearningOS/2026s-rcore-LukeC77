//! Types related to task management & Functions for completely changing TCB

use super::{kstack_alloc, pid_alloc, KernelStack, PidHandle, SignalActions, SignalFlags, TaskContext};
use crate::{
    config::TRAP_CONTEXT_BASE,
    fs::{File, Stdin, Stdout},
    mm::{translated_refmut, MemorySet, PhysPageNum, VirtAddr, KERNEL_SPACE},
    sync::UPSafeCell,
    trap::{trap_handler, TrapContext},
};
use alloc::{
    string::String,
    sync::{Arc, Weak},
    vec,
    vec::Vec,
};
use core::cell::RefMut;

/// Task control block structure
///
/// Directly save the contents that will not change during running
pub struct TaskControlBlock {
    // Immutable
    /// Process identifier
    pub pid: PidHandle,

    /// Kernel stack corresponding to PID
    pub kernel_stack: KernelStack,

    /// Mutable
    inner: UPSafeCell<TaskControlBlockInner>,
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

pub struct TaskControlBlockInner {
    /// The physical page number of the frame where the trap context is placed
    pub trap_cx_ppn: PhysPageNum,

    /// Application data can only appear in areas
    /// where the application address space is lower than base_size
    pub base_size: usize,

    /// Save task context
    pub task_cx: TaskContext,

    /// Maintain the execution status of the current process
    pub task_status: TaskStatus,

    /// Application address space
    pub memory_set: MemorySet,

    /// Parent process of the current process.
    /// Weak will not affect the reference count of the parent
    pub parent: Option<Weak<TaskControlBlock>>,

    /// A vector containing TCBs of all child processes of the current process
    pub children: Vec<Arc<TaskControlBlock>>,

    /// It is set when active exit or execution error occurs
    pub exit_code: i32,
    pub fd_table: Vec<Option<Arc<dyn File + Send + Sync>>>,
    pub signals: SignalFlags,
    pub signal_mask: SignalFlags,// yifan 2026/6/27: 表示进程的全局信号掩码，其类型 SignalFlags 与用户库 user_lib 中的相同，表示一个信号集合。在 signal_mask 这个信号集合内的信号将被该进程全局屏蔽。
    // the signal which is being handling
    pub handling_sig: isize, // yifan 2026/6/30: 表示进程正在执行哪个信号的处理例程
    // Signal actions
    pub signal_actions: SignalActions, // yifan 2026/6/27: 是一个 SignalAction （同样与 user_lib 中的定义相同）的定长数组，其中每一项都记录进程如何响应对应的信号
    // if the task is killed
    pub killed: bool,    // yifan 2026/6/30: 表示这个任务是否已经被标记为应当终止；它的含义不是此刻任务控制块已经被释放，而是进程收到某些致命信号后会先被标记为 killed=true，随后再由内核在合适时机真正结束它。
    // if the task is frozen by a signal
    pub frozen: bool,    // yifan 2026/6/30: 表示这个任务是否因为某个信号而被冻结/暂停执行；某些信号不会直接杀死进程，而是让它先停住，这时内核就会把 frozen 设为 true，使后续调度或信号处理逻辑不要把它当作普通可继续运行的任务。
    pub trap_ctx_backup: Option<TrapContext>, // yifan 2026/6/30: 表示进程执行信号处理例程之前的 Trap 上下文.因为我们要 Trap 回到用户态执行信号处理例程，原来的 Trap 上下文会被覆盖，所以我们将其保存在进程控制块中。

    /// Heap bottom
    pub heap_bottom: usize,

    /// Program break
    pub program_brk: usize,
}

impl TaskControlBlockInner {
    pub fn get_trap_cx(&self) -> &'static mut TrapContext {
        self.trap_cx_ppn.get_mut()
    }
    pub fn get_user_token(&self) -> usize {
        self.memory_set.token()
    }
    fn get_status(&self) -> TaskStatus {
        self.task_status
    }
    pub fn is_zombie(&self) -> bool {
        self.get_status() == TaskStatus::Zombie
    }
    pub fn alloc_fd(&mut self) -> usize {
        if let Some(fd) = (0..self.fd_table.len()).find(|fd| self.fd_table[*fd].is_none()) {    // yifan 2026/6/21: 这里先在 0..fd_table.len() 这个下标范围上调用迭代器的 find，从前往后寻找第一个满足条件的文件描述符槽位；闭包里的 fd 实际类型是 &usize，所以要写 *fd 取出真实下标，再检查 self.fd_table[*fd] 是否为 None。
            fd    // yifan 2026/6/21: 如果 find 返回 Some(fd)，说明找到了一个空闲槽位，就直接复用这个下标作为新分配的文件描述符。
        } else {
            self.fd_table.push(None);
            self.fd_table.len() - 1    // yifan 2026/6/21: 如果 find 没找到空位，就给 fd_table 追加一个新的空槽位，并把新槽位的下标作为分配结果返回。
        }
    }
}

impl TaskControlBlock {
    /// Create a new process
    ///
    /// At present, it is only used for the creation of initproc
    pub fn new(elf_data: &[u8]) -> Self {
        // memory_set with elf program headers/trampoline/trap context/user stack
        let (memory_set, user_sp, entry_point) = MemorySet::from_elf(elf_data);
        let trap_cx_ppn = memory_set
            .translate(VirtAddr::from(TRAP_CONTEXT_BASE).into())
            .unwrap()
            .ppn();
        // alloc a pid and a kernel stack in kernel space
        let pid_handle = pid_alloc();
        let kernel_stack = kstack_alloc();
        let kernel_stack_top = kernel_stack.get_top();
        // push a task context which goes to trap_return to the top of kernel stack
        let task_control_block = Self {
            pid: pid_handle,
            kernel_stack,
            inner: unsafe {
                UPSafeCell::new(TaskControlBlockInner {
                    trap_cx_ppn,
                    base_size: user_sp,
                    task_cx: TaskContext::goto_trap_return(kernel_stack_top),
                    task_status: TaskStatus::Ready,
                    memory_set,
                    parent: None,
                    children: Vec::new(),
                    exit_code: 0,
                    fd_table: vec![
                        // 0 -> stdin
                        Some(Arc::new(Stdin)),
                        // 1 -> stdout
                        Some(Arc::new(Stdout)),
                        // 2 -> stderr
                        Some(Arc::new(Stdout)),
                    ],
                    signals: SignalFlags::empty(),
                    signal_mask: SignalFlags::empty(),
                    handling_sig: -1,
                    signal_actions: SignalActions::default(),
                    killed: false,
                    frozen: false,
                    trap_ctx_backup: None,
                    heap_bottom: user_sp,
                    program_brk: user_sp,
                })
            },
        };
        // prepare TrapContext in user space
        let trap_cx = task_control_block.inner_exclusive_access().get_trap_cx();
        *trap_cx = TrapContext::app_init_context(
            entry_point,
            user_sp,
            KERNEL_SPACE.exclusive_access().token(),
            kernel_stack_top,
            trap_handler as usize,
        );
        task_control_block
    }

    /// Load a new elf to replace the original application address space and start execution
    pub fn exec(&self, elf_data: &[u8], args: Vec<String>) {
        // memory_set with elf program headers/trampoline/trap context/user stack
        // yifan 2026/6/22: exec 需要同时拿到 memory_set 和 trap_cx_ppn，因为它不是在旧程序上继续修改少量状态，而是要把当前进程的整个用户态执行环境替换成一个新程序。
        // memory_set 表示新程序的完整用户地址空间，包含代码段、数据段、用户栈以及 TrapContext 所在页；后面会用它替换 inner.memory_set，使进程真正运行在新 ELF 对应的内存布局上。
        // trap_cx_ppn 表示新地址空间中 TrapContext 所在物理页的页号；由于 exec 后地址空间已经更换，TRAP_CONTEXT_BASE 对应的物理页也可能变化，所以必须重新查询并更新 inner.trap_cx_ppn。
        // 否则内核后续通过 inner.get_trap_cx() 访问的仍可能是旧地址空间里的 TrapContext，进程返回用户态时就无法从新程序入口和新用户栈开始执行。
        let (memory_set, mut user_sp, entry_point) = MemorySet::from_elf(elf_data);    // yifan 2026/6/22: 根据新的 ELF 重新构造用户地址空间，返回新的页表/地址空间 memory_set、新用户栈顶 user_sp，以及新程序入口地址 entry_point。
        let trap_cx_ppn = memory_set    // yifan 2026/6/22: 下面要从这个新地址空间里找到 TrapContext 对应的物理页，并把它的物理页号保存下来，后续 exec 会用它更新当前进程的 TCB。
            .translate(VirtAddr::from(TRAP_CONTEXT_BASE).into())    // yifan 2026/6/22: 用新页表查询虚拟地址 TRAP_CONTEXT_BASE 对应的页表项；这个地址是用户地址空间中专门留给 TrapContext 的位置。
            .unwrap()    // yifan 2026/6/22: 这里默认这条映射一定存在，因为 from_elf 刚刚已经为 TrapContext 建好了映射；如果不存在就说明地址空间构造出错，内核应直接 panic。
            .ppn();    // yifan 2026/6/22: 从页表项中取出物理页号 PhysPageNum，得到 TrapContext 实际所在的物理页，后续 inner.get_trap_cx() 就会通过这个页号访问并重写新的陷入上下文。
        // push arguments on user stack
        user_sp -= (args.len() + 1) * core::mem::size_of::<usize>();    // yifan 2026/6/22: 先把用户栈顶向下移动，为 argv 指针数组预留出 args.len() + 1 个 usize 槽位；前 args.len() 个槽位存各参数字符串地址，最后一个槽位留给空指针结尾。
        let argv_base = user_sp;    // yifan 2026/6/22: 记录这块 argv 数组在用户栈中的起始地址，后面既要按这个基址写入各个参数指针，也会把它放进寄存器作为用户程序看到的 argv。
        let mut argv: Vec<_> = (0..=args.len())    // yifan 2026/6/22: 这里构造的不是普通内核数组，而是一个保存“用户栈上各 argv 槽位可写引用”的 Vec，便于后续直接把参数字符串地址写到用户地址空间中。
            .map(|arg| {    // yifan 2026/6/22: 遍历 0..=args.len()，为 argv[0] 到 argv[args.len()] 每一个槽位都建立对应的可写引用；注意这里包含最后一个结尾空指针槽位。
                translated_refmut(    // yifan 2026/6/22: translated_refmut 会结合给定页表，把用户虚拟地址翻译成内核可访问的可写引用，从而安全地修改新地址空间里的用户栈内容。
                    memory_set.token(),    // yifan 2026/6/22: 这里使用新地址空间 memory_set 的页表 token 做地址翻译，确保写入发生在 exec 后的新用户地址空间，而不是旧地址空间。
                    (argv_base + arg * core::mem::size_of::<usize>()) as *mut usize,    // yifan 2026/6/22: 按 argv_base + arg * sizeof(usize) 计算出用户栈中第 arg 个参数指针槽位的地址，并把它视为一个 *mut usize。
                )    // yifan 2026/6/22: 这一轮 map 的结果就是“指向用户栈中 argv[arg] 位置的可写引用”，后面通过 *argv[i] = ... 就能把参数字符串地址写进去。
            })    // yifan 2026/6/22: map 闭包对每个参数槽位都重复同样的地址翻译过程，把整块 argv 数组逐项包装成可写引用。
            .collect();    // yifan 2026/6/22: 把这些对用户态 argv 槽位的可写引用收集成一个 Vec，后面就可以像操作普通下标数组一样填写 argv 各项。
        *argv[args.len()] = 0;    // yifan 2026/6/22: 把 argv 最后一个槽位写成 0，形成 C 风格的空指针结尾，即 argv[argc] == NULL，用户程序可据此判断参数列表结束。
        for i in 0..args.len() {    // yifan 2026/6/22: 依次处理每一个参数字符串，为每个 args[i] 在新用户栈中分配空间、写入内容，并把它的起始地址回填到 argv[i]。
            user_sp -= args[i].len() + 1;    // yifan 2026/6/22: 把用户栈顶继续向下移动，为当前参数字符串预留 len + 1 个字节；额外的 1 个字节用于存放结尾的 \0。
            *argv[i] = user_sp;    // yifan 2026/6/22: 把当前参数字符串在用户栈中的起始地址写入 argv[i]，这样用户程序后续访问 argv[i] 时就能找到这个字符串。
            let mut p = user_sp;    // yifan 2026/6/22: 用 p 作为当前写指针，从这个参数字符串在用户栈中的起始地址开始，逐字节写入字符串内容。
            for c in args[i].as_bytes() {    // yifan 2026/6/22: 遍历当前参数字符串的每一个字节；这里按字节复制，是因为最终要在用户栈里构造原始的 C 风格字符串。
                *translated_refmut(memory_set.token(), p as *mut u8) = *c;    // yifan 2026/6/22: 借助新地址空间的页表 token，把用户虚拟地址 p 翻译成内核可写引用，并将当前字节 c 写入用户栈对应位置。
                p += 1;    // yifan 2026/6/22: 写完一个字节后把写指针向后移动 1 字节，准备写入当前参数字符串的下一个字符。
            }    // yifan 2026/6/22: 当前参数字符串的所有实际字符已经写入完成，接下来还需要在末尾补上字符串结束标记。
            *translated_refmut(memory_set.token(), p as *mut u8) = 0;    // yifan 2026/6/22: 在字符串末尾写入 0 字节，也就是 \0，形成标准的 C 风格字符串，用户程序即可按 argv[i] 指向的以 0 结尾的字符串读取参数。
        }
        // make the user_sp aligned to 8B for k210 platform
        user_sp -= user_sp % core::mem::size_of::<usize>();    // yifan 2026/6/22: 这一行把 user_sp 向下对齐到 usize 边界；先用 user_sp % size_of::<usize>() 算出当前栈顶偏离字长对齐边界的余数，再把这部分减掉，使 user_sp 变成一个 usize 大小的整数倍，从而满足平台对用户栈指针对齐的要求。

        // **** access current TCB exclusively
        let mut inner = self.inner_exclusive_access();    // yifan 2026/6/22: 独占访问当前进程的 TaskControlBlockInner；因为下面要修改进程的地址空间和陷入上下文，这些都属于当前进程的核心可变状态，必须先拿到可变访问权。
        // substitute memory_set
        inner.memory_set = memory_set;    // yifan 2026/6/22: 用前面根据新 ELF 构造出的 memory_set 替换当前进程原有的用户地址空间；执行这一步后，当前进程的代码段、数据段、用户栈等都切换为新程序对应的内容。
        // update trap_cx ppn
        inner.trap_cx_ppn = trap_cx_ppn;    // yifan 2026/6/22: 更新 TCB 中保存的 TrapContext 所在物理页号；因为 exec 后地址空间已经更换，所以 TRAP_CONTEXT_BASE 对应的物理页也必须同步更新，否则后续访问到的仍会是旧程序那份 TrapContext。
        // initialize trap_cx
        let mut trap_cx = TrapContext::app_init_context(    // yifan 2026/6/22: 重新构造一个“新程序刚开始运行时”应有的 TrapContext，也就是用户态恢复后要加载的那份初始寄存器现场。
            entry_point,    // yifan 2026/6/22: 把新程序入口地址写入 TrapContext，使进程返回用户态后从这个 entry point 开始执行新程序。
            user_sp,    // yifan 2026/6/22: 把前面整理好的新用户栈顶写入 TrapContext，作为新程序启动时使用的用户栈指针。
            KERNEL_SPACE.exclusive_access().token(),    // yifan 2026/6/22: 传入内核地址空间的页表 token，供之后发生 trap 时从用户态安全切回内核态使用。
            self.kernel_stack.get_top(),    // yifan 2026/6/22: 传入当前进程对应的内核栈栈顶，这样该进程后续陷入内核时能使用自己专属的内核栈。
            trap_handler as usize,    // yifan 2026/6/22: 指定之后用户态发生异常、中断或系统调用时进入内核的 trap_handler 入口地址。
        );    // yifan 2026/6/22: 到这里得到的 trap_cx 已经包含了新程序的入口、用户栈以及陷入内核所需的关键上下文信息。
        trap_cx.x[10] = args.len();    // yifan 2026/6/22: 把参数个数 argc 写入寄存器 x[10]，也就是 RISC-V 调用约定中的 a0，用户程序启动后会把它当作参数个数使用。
        trap_cx.x[11] = argv_base;    // yifan 2026/6/22: 把 argv 指针数组在用户栈中的起始地址写入寄存器 x[11]，也就是 a1，用户程序启动后会把它当作 argv 使用。
        *inner.get_trap_cx() = trap_cx;    // yifan 2026/6/22: 把刚构造好的 TrapContext 写入新地址空间中的 TrapContext 区域；这样内核下次恢复该进程时，实际恢复的就是“从新程序入口、带着新的 argc/argv、使用新的用户栈”这套执行现场。
        // **** release current PCB
    }

    /// Fork from parent to child
    pub fn fork(self: &Arc<TaskControlBlock>) -> Arc<TaskControlBlock> {
        // ---- hold parent PCB lock
        let mut parent_inner = self.inner_exclusive_access();
        // copy user space(include trap context)
        let memory_set = MemorySet::from_existed_user(&parent_inner.memory_set);
        let trap_cx_ppn = memory_set
            .translate(VirtAddr::from(TRAP_CONTEXT_BASE).into())
            .unwrap()
            .ppn();
        // alloc a pid and a kernel stack in kernel space
        let pid_handle = pid_alloc();
        let kernel_stack = kstack_alloc();
        let kernel_stack_top = kernel_stack.get_top();
        // copy fd table
        let mut new_fd_table: Vec<Option<Arc<dyn File + Send + Sync>>> = Vec::new();
        for fd in parent_inner.fd_table.iter() {    // yifan 2026/6/21: 这里在 fork 过程中遍历父进程的 fd_table，把父进程当前每一个文件描述符槽位依次复制到子进程的新表中。
            if let Some(file) = fd {    // yifan 2026/6/21: 如果这个槽位是 Some(file)，说明父进程这个 fd 当前已经绑定到了某个已打开文件、管道或标准输入输出对象。
                new_fd_table.push(Some(file.clone()));    // yifan 2026/6/21: 这里不是深拷贝底层文件内容，而是克隆 Arc；这样父子进程共享同一个 File 对象，只是引用计数加一，这正符合 fork 继承已打开文件描述符的语义。
            } else {
                new_fd_table.push(None);    // yifan 2026/6/21: 如果父进程这个 fd 位置本来就是空的，那么子进程对应位置也保持为空，从而保持父子进程的 fd 编号布局一致。
            }
        }
        let task_control_block = Arc::new(TaskControlBlock {
            pid: pid_handle,
            kernel_stack,
            inner: unsafe {
                UPSafeCell::new(TaskControlBlockInner {
                    trap_cx_ppn,
                    base_size: parent_inner.base_size,
                    task_cx: TaskContext::goto_trap_return(kernel_stack_top),
                    task_status: TaskStatus::Ready,
                    memory_set,
                    parent: Some(Arc::downgrade(self)),
                    children: Vec::new(),
                    exit_code: 0,
                    fd_table: new_fd_table,
                    signals: SignalFlags::empty(),
                    // inherit the signal_mask and signal_action
                    signal_mask: parent_inner.signal_mask,
                    handling_sig: -1,
                    signal_actions: parent_inner.signal_actions.clone(),
                    killed: false,
                    frozen: false,
                    trap_ctx_backup: None,
                    heap_bottom: parent_inner.heap_bottom,
                    program_brk: parent_inner.program_brk,
                })
            },
        });
        // add child
        parent_inner.children.push(task_control_block.clone());
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
