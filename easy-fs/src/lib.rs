//!An easy file system isolated from the kernel
#![no_std]
#![deny(missing_docs)]
extern crate alloc;
mod bitmap;
mod block_cache;
mod block_dev;
mod efs;
mod layout;
mod vfs;
/// Use a block size of 512 bytes
pub const BLOCK_SZ: usize = 512;
// yifan 2026/6/1: 定义整个 easy-fs 的最小 I/O 与空间管理单位。
// yifan 2026/6/1: 它决定磁盘读写粒度，read_block/write_block 一次处理 512B。
// yifan 2026/6/1: 它是元数据布局基准，超级块/位图/inode 区/数据区都按块数计算。
// yifan 2026/6/1: 它也是缓存管理单位，block cache 以 512B 作为一条缓存项。
// yifan 2026/6/1: 文件偏移到 (block_id, block_offset) 的地址换算依赖该常量。
// yifan 2026/6/1: 各模块共享这个常量可避免块大小不一致导致的数据错位与解析错误。
use bitmap::Bitmap;
use block_cache::{block_cache_sync_all, get_block_cache};
pub use block_dev::BlockDevice;
pub use efs::EasyFileSystem;
use layout::*;
pub use vfs::Inode;
