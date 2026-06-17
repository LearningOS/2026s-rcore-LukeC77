//!Stdin & Stdout
use super::File;
use super::{Stat, StatMode};// yifan 2026/6/16: 引入 Stat 和 StatMode 结构体，因为后续在实现 File trait 的 get_stat 方法时需要用到它们来构造返回的 Stat 信息。
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
    fn read(&self, mut user_buf: UserBuffer) -> usize {
        assert_eq!(user_buf.len(), 1);
        // busy loop
        let mut c: usize;
        loop {
            c = console_getchar();
            if c == 0 {
                suspend_current_and_run_next();
                continue;
            } else {
                break;
            }
        }
        let ch = c as u8;
        unsafe {
            user_buf.buffers[0].as_mut_ptr().write_volatile(ch);
        }
        1
    }
    fn write(&self, _user_buf: UserBuffer) -> usize {
        panic!("Cannot write to stdin!");
    }

    //yifan 2026/6/16: stdin的get_stat方法，只是一个placeholder
    fn get_stat(&self) -> Stat {
        Stat::new(0, StatMode::NULL, 1)
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
    //yifan 2026/6/16: stdout的get_stat方法，只是一个placeholder
    fn get_stat(&self) -> Stat {
        Stat::new(0, StatMode::NULL, 1)
    }
}
