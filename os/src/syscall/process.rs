//! Process management syscalls
use crate::config::PAGE_SIZE;
// use crate::mm::{VirtAddr, VirtPageNum};
use crate::task::{self, change_program_brk, exit_current_and_run_next, suspend_current_and_run_next};
use crate::timer::get_time_us; // yifan 2026/5/13: 引入 get_time_us 函数，用于获取当前时间的微秒数，以便在 sys_get_time 中填充 TimeVal 结构体。


#[repr(C)]
#[derive(Debug)]
pub struct TimeVal {
    pub sec: usize,
    pub usec: usize,
}

/// task exits and submit an exit code
pub fn sys_exit(_exit_code: i32) -> ! {
    trace!("kernel: sys_exit");
    exit_current_and_run_next();
    panic!("Unreachable in sys_exit!");
}

/// current task gives up resources for other tasks
pub fn sys_yield() -> isize {
    trace!("kernel: sys_yield");
    suspend_current_and_run_next();
    0
}

/// YOUR JOB: get time with second and microsecond
/// HINT: You might reimplement it with virtual memory management.
/// HINT: What if [`TimeVal`] is splitted by two pages ?
pub fn sys_get_time(_ts: *mut TimeVal, _tz: usize) -> isize {
    trace!("kernel: sys_get_time");
    
    // yifan 2026/5/13: 首先检查用户缓冲区指针是否为 null；如果是 null，直接返回错误码 -1。
    if _ts.is_null() {
        return -1;
    }

    let token = task::current_user_token(); // yifan 2026/5/13: 获取当前任务的用户地址空间标识 token，用于后续的地址翻译。
    let ptr = _ts as usize as *const u8; // yifan 2026/5/13: 将用户缓冲区指针 _ts 转换为 usize 再转换为 *const u8，准备进行地址翻译和访问。这里的转换是为了适配 translated_byte_buffer_checked 的参数类型。
    let len = core::mem::size_of::<TimeVal>(); // yifan 2026/5/13: 计算 TimeVal 结构体的字节长度，作为翻译和访问的范围。
    // yifan 2026/5/13: 调用 translated_byte_buffer_checked 检查并翻译用户缓冲区地址；如果翻译失败（如地址无效或不可访问），返回错误码 -1。
    let need_write = true;
    let mut buffers = match crate::mm::translated_byte_buffer_checked(token, ptr, len, need_write) {
        Some(bufs) => bufs,
        None => return -1, // yifan 2026/5/13: 如果用户缓冲区无效或不可访问，返回错误码 -1。
    };

    // yifan 2026/5/13: 获得时间，创建 TimeVal 结构体。
    let us = get_time_us();
    let time = TimeVal {sec: us / 1_000_000, usec: us % 1_000_000};

    // yifan 2026/5/13: 将 TimeVal 结构体变成按字节切片的形式，准备写入用户缓冲区。
    let time_ptr: *const u8 = &time as *const TimeVal as *const u8;
    let src: &[u8] = unsafe{ core::slice::from_raw_parts(time_ptr, len) };

    // yifan 2026/5/13: 逐段写入用户缓冲区；buffers 中的每个 buffer 都是用户缓冲区的一段，可能跨页；src 是 TimeVal 的字节表示。
    let mut used = 0usize;
    for dst in buffers.iter_mut() {
        let n = dst.len();
        dst.copy_from_slice(&src[used..used + n]);
        used += n;
    }
    0
}

/// TODO: Finish sys_trace to pass testcases
/// HINT: You might reimplement it with virtual memory management.
pub fn sys_trace(_trace_request: usize, _id: usize, _data: usize) -> isize {
    trace!("kernel: sys_trace");
    match _trace_request {
        0 => {
            // yifan 2026/5/13: 如果 trace_request 为 0，则 id 应被视作 *const u8 ，
            // 表示读取当前任务 id 地址处一个字节的无符号整数值。此时应忽略 data 参数。
            // 返回值为 id 地址处的值。
            let ptr = _id as *const u8;
            let token = task::current_user_token();
            let buffers = match crate::mm::translated_byte_buffer_checked(token, ptr, 1, false) {
                Some(bufs) => bufs,
                None => return -1, // yifan 2026/5/13: 如果用户地址无效或不可访问，返回错误码 -1。
            };
            buffers[0][0] as isize // yifan 2026/5/13: 由于只需要读取一个字节，直接访问 buffers 中第一个 buffer 的第一个字节即可。
        },
        1 => {
            // yifan 2026/5/13: 如果 trace_request 为 1，则 id 应被视作 *mut u8 ，
            // 表示写入 data （作为 u8，即只考虑最低位的一个字节）到该用户程序 id 地址
            // 处。返回值应为0。
            let ptr = _id as *mut u8;
            let token = task::current_user_token();
            let mut buffers = match crate::mm::translated_byte_buffer_checked(token, ptr, 1, true) {
                Some(bufs) => bufs,
                None => return -1, // yifan 2026/5/13: 如果用户地址无效或不可访问，返回错误码 -1。
            };
            buffers[0][0] = (_data & 0xff) as u8; // yifan 2026/5/13: 将 data 的最低位字节写入用户缓冲区。
            0
        },
        2 => task::get_syscall_count(_id),
        _ => -1,
    }
}

// YOUR JOB: Implement mmap.
/// start 需要映射的虚存起始地址，要求按页对齐
/// len 映射字节长度，可以为 0
/// prot：第 0 位表示是否可读，第 1 位表示是否可写，第 2 位表示是否可执行。其他位无效且必须为 0
pub fn sys_mmap(_start: usize, _len: usize, _port: usize) -> isize {
    trace!("kernel: sys_mmap"); 
    if _start % PAGE_SIZE != 0 {
        return -1; // yifan 2026/5/14: 如果 start 地址不是页对齐的，返回错误码 -1。
    }
    if _port & 0x7 == 0 {
        return -1; // yifan 2026/5/14: 如果 prot 参数的最低三位都为0（即没有任何权限），没有意义。
    }
    if _port & !0x7 != 0 {
        return -1; // yifan 2026/5/14: 其他位无效且必须为 0
    }
    task::mmap(_start, _len, _port) as isize
}

// YOUR JOB: Implement munmap.
pub fn sys_munmap(_start: usize, _len: usize) -> isize {
    trace!("kernel: sys_munmap");
    if _start % PAGE_SIZE != 0 {
        return -1; // yifan 2026/5/16 如果start 地址不是也对齐，返回错误码 -1。
    }
    task::munmap(_start, _len) as isize
}
/// change data segment size
pub fn sys_sbrk(size: i32) -> isize {
    trace!("kernel: sys_sbrk");
    if let Some(old_brk) = change_program_brk(size) {
        old_brk as isize
    } else {
        -1
    }
}
