//! File and filesystem-related syscalls
use crate::fs::{open_file, OpenFlags, Stat};
use crate::mm::{translated_byte_buffer, translated_str, UserBuffer};
use crate::task::{current_task, current_user_token};

pub fn sys_write(fd: usize, buf: *const u8, len: usize) -> isize {    // yifan 2026/6/15: 这是 write 系统调用在内核中的入口，负责把用户缓冲区中的数据写入 fd 对应的文件对象。
    trace!("kernel:pid[{}] sys_write", current_task().unwrap().pid.0);    // yifan 2026/6/15: 这里打印调试日志，记录当前是哪个进程调用了 sys_write。
    let token = current_user_token();    // yifan 2026/6/15: 取得当前用户地址空间的页表 token，后面需要用它把用户缓冲区指针翻译成内核可访问的数据。
    let task = current_task().unwrap();    // yifan 2026/6/15: 取得当前进程控制块，因为要从它的 fd_table 中查找这个 fd 对应的文件对象。
    let inner = task.inner_exclusive_access();    // yifan 2026/6/15: 独占访问当前进程内部状态，准备读取文件描述符表中的对应槽位。
    if fd >= inner.fd_table.len() {    // yifan 2026/6/15: 先检查文件描述符是否越界，超过 fd_table 长度就说明它不是合法的已分配 fd。
        return -1;    // yifan 2026/6/15: fd 非法时直接返回 -1，表示写失败。
    }
    if let Some(file) = &inner.fd_table[fd] {    // yifan 2026/6/15: 如果这个槽位里确实有打开文件，就继续对该文件执行写操作；如果是 None，后面会返回失败。
        if !file.writable() {    // yifan 2026/6/15: 检查这个文件对象是否允许写，如果它是只读打开的，就不能执行 write。
            return -1;    // yifan 2026/6/15: 对不可写文件执行写操作时返回 -1。
        }
        let file = file.clone();    // yifan 2026/6/15: 先克隆一份文件对象的 Arc，这样即使后面释放 inner，也仍然可以继续安全使用这个文件对象。
        // release current task TCB manually to avoid multi-borrow
        drop(inner);    // yifan 2026/6/15: 手动释放对任务控制块内部数据的独占借用，避免后续 file.write 过程中产生多重借用冲突。
        file.write(UserBuffer::new(translated_byte_buffer(token, buf, len))) as isize    // yifan 2026/6/15: 先把用户缓冲区 [buf, buf + len) 翻译成内核可访问的 UserBuffer，再调用具体文件对象的 write 完成真正写入，并返回写入字节数。
    } else {
        -1    // yifan 2026/6/15: 如果 fd_table 这个槽位是空的，说明该 fd 没有对应打开文件，返回 -1 表示写失败。
    }
}

pub fn sys_read(fd: usize, buf: *const u8, len: usize) -> isize {    // yifan 2026/6/15: 这是 read 系统调用在内核中的入口，负责把 fd 对应文件中的数据读到用户缓冲区中。
    trace!("kernel:pid[{}] sys_read", current_task().unwrap().pid.0);    // yifan 2026/6/15: 这里打印调试日志，记录当前是哪个进程调用了 sys_read。
    let token = current_user_token();    // yifan 2026/6/15: 取得当前用户地址空间的页表 token，后面需要用它把用户缓冲区指针翻译成内核可访问的数据区域。
    let task = current_task().unwrap();    // yifan 2026/6/15: 取得当前进程控制块，因为文件描述符表 fd_table 属于这个进程。
    let inner = task.inner_exclusive_access();    // yifan 2026/6/15: 独占访问当前进程内部状态，准备查找 fd 对应的文件对象。
    if fd >= inner.fd_table.len() {    // yifan 2026/6/15: 先检查文件描述符是否越界，超过 fd_table 长度就说明它不是合法的已分配 fd。
        return -1;    // yifan 2026/6/15: fd 非法时直接返回 -1，表示读取失败。
    }
    if let Some(file) = &inner.fd_table[fd] {    // yifan 2026/6/15: 如果该槽位里确实有打开文件，就继续执行读取；如果是 None，后面会返回失败。
        let file = file.clone();    // yifan 2026/6/15: 先克隆一份文件对象的 Arc，这样即使后面释放 inner，也仍然可以安全地继续使用这个文件对象。
        if !file.readable() {    // yifan 2026/6/15: 检查这个文件对象是否允许读，如果它是不可读打开的，就不能执行 read。
            return -1;    // yifan 2026/6/15: 对不可读文件执行读操作时返回 -1。
        }
        // release current task TCB manually to avoid multi-borrow
        drop(inner);    // yifan 2026/6/15: 手动释放对任务控制块内部数据的独占借用，避免后续 file.read 过程中产生多重借用冲突。
        trace!("kernel: sys_read .. file.read");    // yifan 2026/6/15: 再打印一条调试日志，表示接下来将真正调用底层文件对象的 read 方法。
        file.read(UserBuffer::new(translated_byte_buffer(token, buf, len))) as isize    // yifan 2026/6/15: 先把用户缓冲区 [buf, buf + len) 翻译成内核可访问的 UserBuffer，再调用具体文件对象的 read 把数据写入用户缓冲区，并返回实际读取字节数。
    } else {
        -1    // yifan 2026/6/15: 如果 fd_table 这个槽位是空的，说明该 fd 没有对应打开文件，返回 -1 表示读取失败。
    }
}

pub fn sys_open(path: *const u8, flags: u32) -> isize {    // yifan 2026/6/15: 这是 open 系统调用在内核中的入口，负责按用户给定的路径和标志打开文件并返回文件描述符。
    trace!("kernel:pid[{}] sys_open", current_task().unwrap().pid.0);    // yifan 2026/6/15: 这里打印调试日志，记录当前是哪个进程发起了 sys_open 调用。
    let task = current_task().unwrap();    // yifan 2026/6/15: 先取得当前正在运行的进程控制块，后面要把新打开的文件登记到它的 fd_table 中。
    let token = current_user_token();    // yifan 2026/6/15: 取得当前用户地址空间对应的页表 token，后面需要用它把用户指针翻译成内核可访问的数据。
    let path = translated_str(token, path);    // yifan 2026/6/15: 把用户态传入的路径指针安全地翻译并复制成内核中的字符串，避免直接解引用用户虚拟地址。
    if let Some(inode) = open_file(path.as_str(), OpenFlags::from_bits(flags).unwrap()) {    // yifan 2026/6/15: 先把整数 flags 转成 OpenFlags，再真正执行打开文件逻辑；成功时得到的是打开后的 Arc<OSInode>。
        let mut inner = task.inner_exclusive_access();    // yifan 2026/6/15: 由于接下来要修改当前进程内部状态，所以这里独占访问任务控制块内部数据。
        let fd = inner.alloc_fd();    // yifan 2026/6/15: 为这个新打开的文件分配一个文件描述符编号，优先复用空槽，没有空槽就扩展 fd_table。
        inner.fd_table[fd] = Some(inode);    // yifan 2026/6/15: 把打开好的文件对象放进当前进程的文件描述符表中，使这个 fd 能映射到真正的文件对象。
        fd as isize    // yifan 2026/6/15: 打开成功后把文件描述符返回给用户程序。
    } else {
        -1    // yifan 2026/6/15: 如果 open_file 失败，例如文件不存在且未指定 CREATE，就返回 -1 表示打开失败。
    }
}

pub fn sys_close(fd: usize) -> isize {    // yifan 2026/6/15: 这是 close 系统调用在内核中的入口，负责关闭当前进程中指定编号的文件描述符。
    trace!("kernel:pid[{}] sys_close", current_task().unwrap().pid.0);    // yifan 2026/6/15: 这里打印调试日志，记录当前是哪个进程调用了 sys_close。
    let task = current_task().unwrap();    // yifan 2026/6/15: 先取得当前正在运行的进程控制块，因为文件描述符表 fd_table 属于具体进程。
    let mut inner = task.inner_exclusive_access();    // yifan 2026/6/15: 由于接下来要修改进程内部的 fd_table，所以这里需要独占访问任务控制块内部数据。
    if fd >= inner.fd_table.len() {    // yifan 2026/6/15: 先检查用户传入的 fd 是否越界，如果超过文件描述符表长度就说明它不是合法的已分配描述符。
        return -1;    // yifan 2026/6/15: fd 越界时直接返回 -1，表示关闭失败。
    }
    if inner.fd_table[fd].is_none() {    // yifan 2026/6/15: 即使 fd 没越界，也可能这个槽位当前是空的，说明该描述符未打开或已经被关闭过。
        return -1;    // yifan 2026/6/15: 如果对应槽位没有文件对象，同样返回 -1 表示关闭失败。
    }
    inner.fd_table[fd].take();    // yifan 2026/6/15: take() 会取走 Option 里的文件对象并把该槽位改成 None，从而释放这个 fd 供以后复用。
    0    // yifan 2026/6/15: 返回 0 表示本次 close 成功完成。
}

/// YOUR JOB: Implement fstat.
pub fn sys_fstat(_fd: usize, _st: *mut Stat) -> isize {
    trace!(
        "kernel:pid[{}] sys_fstat NOT IMPLEMENTED",
        current_task().unwrap().pid.0
    );
    -1
}

/// YOUR JOB: Implement linkat.
pub fn sys_linkat(_old_name: *const u8, _new_name: *const u8) -> isize {
    trace!(
        "kernel:pid[{}] sys_linkat NOT IMPLEMENTED",
        current_task().unwrap().pid.0
    );
    -1
}

/// YOUR JOB: Implement unlinkat.
pub fn sys_unlinkat(_name: *const u8) -> isize {
    trace!(
        "kernel:pid[{}] sys_unlinkat NOT IMPLEMENTED",
        current_task().unwrap().pid.0
    );
    -1
}
