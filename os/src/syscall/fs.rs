//! File and filesystem-related syscalls

use crate::mm::translated_byte_buffer;
use crate::task::current_user_token;

const FD_STDOUT: usize = 1;

/// write buf of length `len`  to a file with `fd`
pub fn sys_write(fd: usize, buf: *const u8, len: usize) -> isize {      // yifan 2026/5/12: sys_write 系统调用入口，参数分别是文件描述符、用户缓冲区指针和长度。
    trace!("kernel: sys_write");    // yifan 2026/5/12: 打印调试日志，标记进入 sys_write。
    match fd {    // yifan 2026/5/12: 按文件描述符分支处理；当前实现仅支持标准输出。
        FD_STDOUT => {    // yifan 2026/5/12: 当 fd=1（标准输出）时进入该分支。
            let buffers = translated_byte_buffer(current_user_token(), buf, len);    // yifan 2026/5/12: 将用户态 [buf, buf+len) 翻译为内核可访问的分段切片（可能跨页）。
            for buffer in buffers {    // yifan 2026/5/12: 逐段处理翻译后的切片。
                print!("{}", core::str::from_utf8(buffer).unwrap());    // yifan 2026/5/12: 把当前字节段按 UTF-8 解释为字符串并输出到控制台。
            }
            len as isize    // yifan 2026/5/12: 成功时返回写入字节数。
        }
        _ => {    // yifan 2026/5/12: 其他 fd 目前未实现。
            panic!("Unsupported fd in sys_write!");    // yifan 2026/5/12: 对不支持的 fd 直接 panic。
        }
    }
}
