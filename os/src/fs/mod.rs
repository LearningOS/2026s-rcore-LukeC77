//! File trait & inode(dir, file, pipe, stdin, stdout)

mod inode;
mod stdio;

use crate::mm::UserBuffer;

/// trait File for all file types
pub trait File: Send + Sync {
    /// the file readable?
    fn readable(&self) -> bool;
    /// the file writable?
    fn writable(&self) -> bool;
    /// read from the file to buf, return the number of bytes read
    fn read(&self, buf: UserBuffer) -> usize;
    /// write to the file from buf, return the number of bytes written
    fn write(&self, buf: UserBuffer) -> usize;
    /// yifan: 2026/6/16: get the stat of the file, return a Stat struct
    fn get_stat(&self) -> Stat;
}

/// The stat of a inode
#[repr(C)]
#[derive(Debug)]
pub struct Stat {
    /// ID of device containing file. 文件所在磁盘驱动器号，该实验中写死为 0 即可
    pub dev: u64,
    /// inode number. 
    pub ino: u64,
    /// file type and mode
    pub mode: StatMode,
    /// number of hard links. 硬链接数量，初始为1
    pub nlink: u32,
    /// unused pad. 无需考虑，为了兼容性设计
    pad: [u64; 7],
}


impl Stat {
    /// yifan 2026/6/16: 这是 stat 结构体的一个构造函数，方便后续创建 Stat 实例时直接传入 ino、mode 和 nlink 参数，dev 固定为0，pad 填充为0。
    pub fn new(ino: u64, mode: StatMode, nlink: u32) -> Self {
        Self {
            dev: 0,
            ino,
            mode,
            nlink,
            pad: [0; 7],
        }
    }
}

bitflags! {
    /// The mode of a inode
    /// whether a directory or a file
    pub struct StatMode: u32 {
        /// null
        const NULL  = 0;
        /// directory
        const DIR   = 0o040000;
        /// ordinary regular file
        const FILE  = 0o100000;
    }
}

pub use inode::{list_apps, open_file, OSInode, OpenFlags, ROOT_INODE}; // yifan 2026/6/17: 从 inode 模块中导出 ROOT_INODE。
pub use stdio::{Stdin, Stdout};
