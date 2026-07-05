//!Stdin & Stdout
use super::File;
use crate::mm::UserBuffer;
use crate::sbi::console_getchar;
use crate::task::suspend_current_and_run_next;

/// stdin file for getting chars from console
pub struct Stdin;

/// stdout file for putting chars to console
pub struct Stdout;

impl File for Stdin {
    fn readable(&self) -> bool {
        true
    }
    fn writable(&self) -> bool {
        false
    }
    fn read(&self, mut user_buf: UserBuffer) -> usize {    // yifan 2026/6/21: Stdin::read 表示“这一次 read 系统调用从标准输入读取多少数据”，不是说 stdin 整体一生只能读这么多。
        assert_eq!(user_buf.len(), 1);    // yifan 2026/6/21: 这里限制的是单次 read 调用的缓冲区长度必须为 1，也就是一次系统调用最多读 1 个字符；shell 之后会通过反复调用 read/getchar 把多个字符拼成完整命令。
        // busy loop
        let mut c: usize;    // yifan 2026/6/21: c 用来保存 console_getchar 返回的结果；它既可能是真正读到的字符编码，也可能是特殊值 0。
        loop {
            c = console_getchar();    // yifan 2026/6/21: 每次循环只尝试从控制台取一个字符，因此像 "ls -l" 这样的整条命令，本质上是多次取字符后由用户态 shell 累积出来的。
            if c == 0 {    // yifan 2026/6/21: 返回 0 表示“当前这一刻没有新的输入字符”，不是“命令输入结束”；命令结束依赖用户真实输入的回车字符 '\\n' 或 '\\r'，由 shell 代码识别。
                suspend_current_and_run_next();    // yifan 2026/6/21: 没有输入时当前任务会让出 CPU，等以后被重新调度后继续读；这不会吞掉已经输入完成的命令，因为回车本身会作为实际字符返回，而不是 0。
                continue;
            } else {
                break;    // yifan 2026/6/21: 只要读到的不是 0，就说明这次确实拿到了一个字符，可能是普通字符，也可能是回车；是否把回车解释为“执行命令”由上层 shell 决定，不在这里处理。
            }
        }
        let ch = c as u8;    // yifan 2026/6/21: 这里把底层返回值转换成一个字节，交给用户态；例如用户按下 'l'、's'、空格或回车时，shell 都是逐字节收到并自行解析。
        unsafe {
            user_buf.buffers[0].as_mut_ptr().write_volatile(ch);    // yifan 2026/6/21: 把本次读到的 1 个字符写入用户缓冲区；多次这样的单字符写入，最终构成 shell 维护的一整行输入。
        }
        1    // yifan 2026/6/21: 返回 1 表示这一次 read 成功交付了 1 个字节；之后如果 shell 还要继续读命令的后续字符，会再次发起新的 read。
    }
    fn write(&self, _user_buf: UserBuffer) -> usize {
        panic!("Cannot write to stdin!");
    }
}

impl File for Stdout {
    fn readable(&self) -> bool {
        false
    }
    fn writable(&self) -> bool {
        true
    }
    fn read(&self, _user_buf: UserBuffer) -> usize {
        panic!("Cannot read from stdout!");
    }
    fn write(&self, user_buf: UserBuffer) -> usize {
        for buffer in user_buf.buffers.iter() {
            print!("{}", core::str::from_utf8(*buffer).unwrap());
        }
        user_buf.len()
    }
}
