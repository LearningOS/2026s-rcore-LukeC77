//! File and filesystem-related syscalls
use crate::mm::translated_byte_buffer;
use crate::sbi::console_getchar;
use crate::task::{current_task, current_user_token, suspend_current_and_run_next};

const FD_STDIN: usize = 0;
const FD_STDOUT: usize = 1;

/// write buf of length `len`  to a file with `fd`
pub fn sys_write(fd: usize, buf: *const u8, len: usize) -> isize {
    trace!("kernel:pid[{}] sys_write", current_task().unwrap().pid.0);
    match fd {
        FD_STDOUT => {
            let buffers = translated_byte_buffer(current_user_token(), buf, len);
            for buffer in buffers {
                print!("{}", core::str::from_utf8(buffer).unwrap());
            }
            len as isize
        }
        _ => {
            panic!("Unsupported fd in sys_write!");
        }
    }
}

/// yifan 2026/5/26
/// 检查 fd 是否为标准输入
/// → 检查 len 是否为 1
/// → 调用 console_getchar 尝试读取键盘字符
/// → 如果没有输入，就让出 CPU，之后再试
/// → 如果有输入，就把字符写入用户缓冲区
/// → 返回 1，表示成功读取 1 个字节
pub fn sys_read(fd: usize, buf: *const u8, len: usize) -> isize {
    trace!("kernel:pid[{}] sys_read", current_task().unwrap().pid.0);
    match fd {
        FD_STDIN => {
            assert_eq!(len, 1, "Only support len = 1 in sys_read!");
            let mut c: usize;
            loop {
                c = console_getchar();    // yifan 2026/5/26: 尝试从控制台读取一个字符（无输入时约定返回 0）。
                if c == 0 {    // yifan 2026/5/26: c==0 表示当前没有可读输入。
                    suspend_current_and_run_next();    // yifan 2026/5/26: 当前进程让出 CPU，避免在内核中忙等占用时间片。
                    continue;    // yifan 2026/5/26: 之后被再次调度时回到循环开头继续尝试读取。
                } else {
                    break;    // yifan 2026/5/26: 读到非 0 的有效字符，退出等待循环进入后续写用户缓冲区流程。
                }
            }
            let ch = c as u8;    // yifan 2026/5/26: 将 console_getchar 得到的 usize 转成单字节字符，准备写入用户缓冲区。
            let mut buffers = translated_byte_buffer(current_user_token(), buf, len);    // yifan 2026/5/26: buffers 是按页表把用户指针 buf 翻译出的可写切片视图（可跨页），不是独立数据副本。
            unsafe {
                buffers[0].as_mut_ptr().write_volatile(ch);    // yifan 2026/5/26: 这里直接写入 buf 指向的用户实际内存；即使 buffers 不返回，输入也已保存在用户缓冲区中。yifan 2026/5/26: 使用 volatile 确保该次内存写不会被优化器省略/重排。
            }
            1
        }
        _ => {
            panic!("Unsupported fd in sys_read!");
        }
    }
}
