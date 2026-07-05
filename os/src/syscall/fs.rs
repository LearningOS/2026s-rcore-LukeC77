//! File and filesystem-related syscalls
use crate::fs::{make_pipe, open_file, OpenFlags, Stat};
use crate::mm::{translated_byte_buffer, translated_refmut, translated_str, UserBuffer};
use crate::task::{current_task, current_user_token};
use alloc::sync::Arc;

pub fn sys_write(fd: usize, buf: *const u8, len: usize) -> isize {
    trace!("kernel:pid[{}] sys_write", current_task().unwrap().pid.0);
    let token = current_user_token();
    let task = current_task().unwrap();
    let inner = task.inner_exclusive_access();
    if fd >= inner.fd_table.len() {
        return -1;
    }
    if let Some(file) = &inner.fd_table[fd] {
        if !file.writable() {
            return -1;
        }
        let file = file.clone();
        // release current task TCB manually to avoid multi-borrow
        drop(inner);
        file.write(UserBuffer::new(translated_byte_buffer(token, buf, len))) as isize
    } else {
        -1
    }
}

pub fn sys_read(fd: usize, buf: *const u8, len: usize) -> isize {
    trace!("kernel:pid[{}] sys_read", current_task().unwrap().pid.0);
    let token = current_user_token();
    let task = current_task().unwrap();
    let inner = task.inner_exclusive_access();
    if fd >= inner.fd_table.len() {
        return -1;
    }
    if let Some(file) = &inner.fd_table[fd] {
        let file = file.clone();
        if !file.readable() {
            return -1;
        }
        // release current task TCB manually to avoid multi-borrow
        drop(inner);
        trace!("kernel: sys_read .. file.read");
        file.read(UserBuffer::new(translated_byte_buffer(token, buf, len))) as isize
    } else {
        -1
    }
}

pub fn sys_open(path: *const u8, flags: u32) -> isize {
	trace!("kernel:pid[{}] sys_open", current_task().unwrap().pid.0);
    let task = current_task().unwrap();
    let token = current_user_token();
    let path = translated_str(token, path);
    if let Some(inode) = open_file(path.as_str(), OpenFlags::from_bits(flags).unwrap()) {
        let mut inner = task.inner_exclusive_access();
        let fd = inner.alloc_fd();
        inner.fd_table[fd] = Some(inode);
        fd as isize
    } else {
        -1
    }
}

pub fn sys_close(fd: usize) -> isize {
	trace!("kernel:pid[{}] sys_close", current_task().unwrap().pid.0);
    let task = current_task().unwrap();
    let mut inner = task.inner_exclusive_access();
    if fd >= inner.fd_table.len() {
        return -1;
    }
    if inner.fd_table[fd].is_none() {
        return -1;
    }
    inner.fd_table[fd].take();
    0
}

pub fn sys_pipe(pipe: *mut usize) -> isize {
	trace!("kernel:pid[{}] sys_pipe", current_task().unwrap().pid.0);
    let task = current_task().unwrap();
    let token = current_user_token();
    let mut inner = task.inner_exclusive_access();
    let (pipe_read, pipe_write) = make_pipe();    // yifan 2026/6/21: 这里先创建同一个底层管道缓冲区上的两个端点：一个读端、一个写端；注意此时只是“造出一对端点”，还没有把它们分别交给两个不同进程。
    let read_fd = inner.alloc_fd();    // yifan 2026/6/21: 先在当前进程自己的 fd_table 里分配一个空闲文件描述符编号，准备用来登记管道读端。
    inner.fd_table[read_fd] = Some(pipe_read);    // yifan 2026/6/21: 把读端放进当前进程的 fd_table；pipe() 的语义本来就是让调用它的那个进程先同时拿到这对端点，而不是在创建瞬间就自动分给两个不同进程。
    let write_fd = inner.alloc_fd();    // yifan 2026/6/21: 再次从同一个进程的 fd_table 中分配一个空闲编号，用来登记同一条管道的写端。
    inner.fd_table[write_fd] = Some(pipe_write);    // yifan 2026/6/21: 把写端也放进当前进程的 fd_table；后续如果这个进程 fork，父子进程会先一起继承这两个端点，再通过 close 把不需要的一端关掉，从而形成“一个进程读、另一个进程写”的最终形态。
    *translated_refmut(token, pipe) = read_fd;    // yifan 2026/6/21: 把读端对应的 fd 编号写回当前调用进程用户空间中的 pipe[0]；如果后续发生 fork，子进程通常也会在自己对应的用户地址里看到同样的整数值，因为用户地址空间会被复制。
    *translated_refmut(token, unsafe { pipe.add(1) }) = write_fd;    // yifan 2026/6/21: 把写端对应的 fd 编号写回当前调用进程用户空间中的 pipe[1]；但子进程之所以也能继续使用这两个 fd，关键不只是用户态数组内容被复制了，更重要的是 fork 时内核里的 fd_table 也一起被复制了，因此这两个编号在子进程中仍然对应有效的管道端点。
    0
}

/// yifan 2026/6/23: 功能：将进程中一个已经打开的文件复制一份并分配到一个新的文件描述符中。
/// 参数：fd 表示进程中一个已经打开的文件的文件描述符。
/// 返回值：如果出现了错误则返回 -1，否则能够访问已打开文件的新文件描述符。
/// 可能的错误原因是：传入的 fd 并不对应一个合法的已打开文件。
/// syscall ID：24
/// 当前sys_dup还不能复制到指定的文件描述符上，后续可以扩展为 sys_dup2。
pub fn sys_dup(fd: usize) -> isize {
	trace!("kernel:pid[{}] sys_dup", current_task().unwrap().pid.0);
    let task = current_task().unwrap();
    let mut inner = task.inner_exclusive_access();
    if fd >= inner.fd_table.len() {
        return -1;
    }
    if inner.fd_table[fd].is_none() {
        return -1;
    }
    let new_fd = inner.alloc_fd();
    inner.fd_table[new_fd] = Some(Arc::clone(inner.fd_table[fd].as_ref().unwrap()));
    new_fd as isize
}

/// YOUR JOB: Implement fstat.
pub fn sys_fstat(_fd: usize, _st: *mut Stat) -> isize {
    trace!("kernel:pid[{}] sys_fstat NOT IMPLEMENTED", current_task().unwrap().pid.0);
    -1
}

/// YOUR JOB: Implement linkat.
pub fn sys_linkat(_old_name: *const u8, _new_name: *const u8) -> isize {
    trace!("kernel:pid[{}] sys_linkat NOT IMPLEMENTED", current_task().unwrap().pid.0);
    -1
}

/// YOUR JOB: Implement unlinkat.
pub fn sys_unlinkat(_name: *const u8) -> isize {
    trace!("kernel:pid[{}] sys_unlinkat NOT IMPLEMENTED", current_task().unwrap().pid.0);
    -1
}
