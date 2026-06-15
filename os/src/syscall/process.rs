//! Process management syscalls
//!
use alloc::sync::Arc;

use crate::{
    fs::{open_file, OpenFlags}, // yifan 2026/6/16: 引入文件系统模块中的 open_file 函数和 OpenFlags 枚举，用于 sys_exec 和 sys_spawn 中打开可执行文件。
    mm::{translated_refmut, translated_str},
    task::{
        add_task, current_task, current_user_token, exit_current_and_run_next,
        suspend_current_and_run_next,
    },
};

use crate::timer::get_time_us;// yifan 2026/5/27: 引入获取当前时间的函数，用于 sys_get_time 系统调用实现。
use crate::task;// yifan 2026/5/27: 引入任务模块，主要是为了获取当前用户地址空间的 token，以及后续可能需要访问或修改任务控制块（PCB）相关数据。

use crate::task::BIG_STRIDE; // yifan 2026/5/28: 引入调度算法相关常量。
use crate::config::PAGE_SIZE; // yifan 2026/5/29: 引入页大小常量用于地址对齐检查。
use crate::mm::VirtAddr; // yifan 2026/5/29: 引入虚拟地址类型用于地址范围检查。
use crate::mm::MapPermission; // yifan 2026/5/29: 引入内存映射权限类型用于权限设置。

#[repr(C)]
#[derive(Debug)]
pub struct TimeVal {
    pub sec: usize,
    pub usec: usize,
}

pub fn sys_exit(exit_code: i32) -> ! {
    trace!("kernel:pid[{}] sys_exit", current_task().unwrap().pid.0);
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

pub fn sys_exec(path: *const u8) -> isize {    // yifan 2026/6/15: 这是 exec 系统调用在内核中的入口，用来用一个新的程序镜像替换当前进程原来的用户态执行内容。
    trace!("kernel:pid[{}] sys_exec", current_task().unwrap().pid.0);    // yifan 2026/6/15: 这里打印调试日志，记录当前是哪个进程调用了 sys_exec。
    let token = current_user_token();    // yifan 2026/6/15: 取得当前用户地址空间对应的页表 token，后面需要用它安全地读取用户传入的路径字符串。
    let path = translated_str(token, path);    // yifan 2026/6/15: 把用户态的路径指针翻译成内核中的字符串，避免直接解引用用户虚拟地址。
    if let Some(app_inode) = open_file(path.as_str(), OpenFlags::RDONLY) {    // yifan 2026/6/15: 以只读方式打开目标程序文件，因为 exec 只需要读取可执行文件内容而不需要写它。
        let all_data = app_inode.read_all();    // yifan 2026/6/15: 读取这个应用文件的全部内容，通常这里得到的是完整的 ELF 可执行文件字节数据。
        let task = current_task().unwrap();    // yifan 2026/6/15: 再次取得当前进程控制块，因为接下来要直接修改当前进程的地址空间和执行上下文。
        task.exec(all_data.as_slice());    // yifan 2026/6/15: 用刚读出的新程序镜像替换当前进程原来的用户空间、用户栈和入口点，这正是 exec 的核心语义。
        0    // yifan 2026/6/15: 如果替换成功完成，就返回 0 表示 exec 成功。
    } else {
        -1    // yifan 2026/6/15: 如果目标程序文件无法打开，就返回 -1 表示 exec 失败。
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

/// YOUR JOB: get time with second and microsecond
/// HINT: You might reimplement it with virtual memory management.
/// HINT: What if [`TimeVal`] is splitted by two pages ?
pub fn sys_get_time(_ts: *mut TimeVal, _tz: usize) -> isize {
    trace!(
        "kernel:pid[{}] sys_get_time IMPLEMENTED",
        current_task().unwrap().pid.0
    );
    // yifan 2026/5/27: 首先检查用户缓冲区指针是否为 null；如果是 null，直接返回错误码 -1。
    if _ts.is_null() {
        return -1;
    }

    let token = task::current_user_token(); // yifan 2026/5/27: 获取当前任务的用户地址空间标识 token，用于后续的地址翻译。
    let ptr = _ts as usize as *const u8; // yifan 2026/5/27: 将用户缓冲区指针 _ts 转换为 usize 再转换为 *const u8，准备进行地址翻译和访问。这里的转换是为了适配 translated_byte_buffer_checked 的参数类型。
    let len = core::mem::size_of::<TimeVal>(); // yifan 2026/5/27: 计算 TimeVal 结构体的字节长度，作为翻译和访问的范围。
    // yifan 2026/5/27: 调用 translated_byte_buffer_checked 检查并翻译用户缓冲区地址；如果翻译失败（如地址无效或不可访问），返回错误码 -1。
    let need_write = true;
    let mut buffers = match crate::mm::translated_byte_buffer_checked(token, ptr, len, need_write) {
        Some(bufs) => bufs,
        None => return -1, // yifan 2026/5/27: 如果用户缓冲区无效或不可访问，返回错误码 -1。
    };
    
    // yifan 2026/5/27: 获得时间，创建 TimeVal 结构体。
    let us = get_time_us();
    let time = TimeVal {sec: us / 1_000_000, usec: us % 1_000_000};

    // yifan 2026/5/27: 将 TimeVal 结构体变成按字节切片的形式，准备写入用户缓冲区。
    let time_ptr: *const u8 = &time as *const TimeVal as *const u8;
    let src: &[u8] = unsafe{ core::slice::from_raw_parts(time_ptr, len) };

    // yifan 2026/5/27: 逐段写入用户缓冲区；buffers 中的每个 buffer 都是用户缓冲区的一段，可能跨页；src 是 TimeVal 的字节表示。
    let mut used = 0usize;
    for dst in buffers.iter_mut() {
        let n = dst.len();
        dst.copy_from_slice(&src[used..used + n]);
        used += n;
    }
    0
}

/// YOUR JOB: Implement mmap.
pub fn sys_mmap(_start: usize, _len: usize, _port: usize) -> isize {
    trace!(
        "kernel:pid[{}] sys_mmap IMPLEMENTED",
        current_task().unwrap().pid.0
    );
    if _start % PAGE_SIZE != 0 {
        return -1; // yifan 2026/5/14: 如果 start 地址不是页对齐的，返回错误码 -1。
    }
    if _port & 0x7 == 0 {
        return -1; // yifan 2026/5/14: 如果 prot 参数的最低三位都为0（即没有任何权限），没有意义。
    }
    if _port & !0x7 != 0 {
        return -1; // yifan 2026/5/14: 其他位无效且必须为 0
    }
    
    let task = current_task().unwrap(); // yifan 2026/5/29: 获取当前任务，准备访问其内存空间信息以检查映射冲突。
    let mut inner = task.inner_exclusive_access(); // yifan 2026/5/29: 独占访问当前任务的内部状态，准备检查内存映射冲突。
    let memory_set = &mut inner.memory_set;
    let start_va = VirtAddr(_start);
    let end_va = VirtAddr(_start + _len);
    if memory_set.overlap(start_va, end_va) {
        return -1; // yifan 2026/5/15: 如果要映射的虚拟地址区间与当前内存空间已有的映射存在重叠，返回错误码 -1。
    }
    let mut perm = MapPermission::U;
    if _port & 0b1 != 0 { // 可读
        perm |= MapPermission::R;
    }
    if _port & 0b10 != 0 { // 可写
        perm |= MapPermission::W;
    }
    if _port & 0b100 != 0 { // 可执行
        perm |= MapPermission::X;
    }
    memory_set.insert_framed_area(start_va, end_va, perm);
    0
}

/// YOUR JOB: Implement munmap.
pub fn sys_munmap(_start: usize, _len: usize) -> isize {
    trace!(
        "kernel:pid[{}] sys_munmap IMPLEMENTED",
        current_task().unwrap().pid.0
    );
    if _start % PAGE_SIZE != 0 {
        return -1; // yifan 2026/5/16 如果start 地址不是也对齐，返回错误码 -1。
    }
    // task::munmap(_start, _len) as isize
    let task = current_task().unwrap(); // yifan 2026/5/29: 获取当前任务，准备访问其内存空间信息以检查映射区间。
    let mut inner = task.inner_exclusive_access(); // yifan 2026/5/29: 独占访问当前任务的内部状态，准备检查内存映射区间。
    let memory_set = &mut inner.memory_set;
    let start_va = VirtAddr(_start);
    let end_va = VirtAddr(_start + _len);

    if !memory_set.full_mapped(start_va, end_va) {
        return -1; // yifan 2026/5/15: 如果要解除映射的虚拟地址区间不是当前内存空间已有的映射的子区间，返回错误码 -1。
    }

    memory_set.unmap(start_va, end_va);
    0
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
        "kernel:pid[{}] sys_spawn IMPLEMENTED",
        current_task().unwrap().pid.0
    );
    let token = current_user_token();    // yifan 2026/5/27: 获取当前进程用户页表 token，用于翻译用户态传入的 path 指针。这是因为path 是用户空间的 C 字符串指针，内核需要通过当前进程的页表来正确访问它。
    let path = translated_str(token, _path);    // yifan 2026/5/27: 将用户态 C 字符串路径按页表翻译并拷贝为内核 String。
    if let Some(app_inode) = open_file(path.as_str(), OpenFlags::RDONLY) { // yifan 2026/6/16: 以open_file打开目标程序文件，获取对应的 OSInode 对象，而不是复用 chapter 5 中使用loader的get_app_data_by_name。如果打开失败（如文件不存在），则返回 -1 表示 spawn 失败。
        let all_data = app_inode.read_all(); // yifan 2026/6/16: 读取目标程序文件的全部内容，得到一个 Vec<u8>，通常是完整的 ELF 可执行文件字节数据。
        let new_task = Arc::new(task::TaskControlBlock::new(all_data.as_slice())); // yifan 2026/6/16: 创建新任务，直接从文件系统读取出的 ELF 字节构造新进程，而不是复用 chapter 5 中链接进内核镜像的应用数据。
        let new_pid = new_task.pid.0;
        let parent = current_task().unwrap();
        let mut parent_inner = parent.inner_exclusive_access(); // yifan 2026/5/27: 获取当前任务的内部可变访问，准备读取父进程信息并创建子进程。
        parent_inner.children.push(new_task.clone()); // yifan 2026/5/27: 将新任务加入父进程的 children 列表，建立父子关系。
        new_task.inner_exclusive_access().parent = Some(Arc::downgrade(&parent)); // yifan 2026/5/27: 在新任务内部记录父进程的弱引用，方便后续父进程 wait 时找到子进程。
        add_task(new_task); // yifan 2026/5/27: 将新任务加入调度器，等待调度执行。这里new_task被move了，所以不再使用它了。
        new_pid as isize // yifan 2026/5/27: 返回新创建子进程的 PID，表示 spawn 成功。
    } else {
        -1 // yifan 2026/5/27: 返回 -1 表示未找到目标应用（spawn 失败）。
    }
}

// YOUR JOB: Set task priority.
pub fn sys_set_priority(_prio: isize) -> isize {
    trace!(
        "kernel:pid[{}] sys_set_priority IMPLEMENTED",
        current_task().unwrap().pid.0
    );
    if _prio <= 1 {
        return -1; // yifan 2026/5/28: 优先级数值必须大于 1，返回 -1 表示无效优先级。
    }
    let task = current_task().unwrap();
    let mut inner = task.inner_exclusive_access();
    inner.priority = _prio as usize; // yifan 2026/5/28: 设置当前任务的优先级字段，供后续调度算法使用。
    inner.pass = BIG_STRIDE / inner.priority; // yifan 2026/5/28: 根据新的优先级计算 pass 值，供 stride 调度算法使用。
    _prio // yifan 2026/5/28: 返回设置的优先级数值，表示成功。
}
