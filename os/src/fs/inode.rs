//! `Arc<Inode>` -> `OSInodeInner`: In order to open files concurrently
//! we need to wrap `Inode` into `Arc`,but `Mutex` in `Inode` prevents
//! file systems from being accessed simultaneously
//!
//! `UPSafeCell<OSInodeInner>` -> `OSInode`: for static `ROOT_INODE`,we
//! need to wrap `OSInodeInner` into `UPSafeCell`
use super::File;
use crate::drivers::BLOCK_DEVICE;
use crate::mm::UserBuffer;
use crate::sync::UPSafeCell;
use alloc::sync::Arc;
use alloc::vec::Vec;
use bitflags::*;
use easy_fs::{EasyFileSystem, Inode};
use lazy_static::*;

/// inode in memory
/// A wrapper around a filesystem inode
/// to implement File trait atop
pub struct OSInode {    // yifan 2026/6/14: OSInode 表示操作系统视角下的一个已打开文件对象，不只是底层文件本体，还封装了这次打开对应的权限和运行时状态。
    readable: bool,    // yifan 2026/6/14: 记录这个打开文件对象是否允许读，因为同一个底层文件被不同方式打开时权限可以不同。
    writable: bool,    // yifan 2026/6/14: 记录这个打开文件对象是否允许写，使内核能区分只读打开、只写打开和读写打开。
    inner: UPSafeCell<OSInodeInner>,    // yifan 2026/6/14: 把真正会在读写过程中变化的内部状态包进 UPSafeCell，便于通过 exclusive_access() 安全地修改 offset 和访问底层 inode。
}
/// The OS inode inner in 'UPSafeCell'
pub struct OSInodeInner {    // yifan 2026/6/14: OSInodeInner 专门存放打开文件对象内部可变的状态，把变化频繁的部分从外层 OSInode 中拆分出来。
    offset: usize,    // yifan 2026/6/14: offset 表示当前文件读写位置，每次 read/write 后都会变化，所以它属于打开实例的运行时状态而不是文件本体属性。
    inode: Arc<Inode>,    // yifan 2026/6/14: 这里用 Arc<Inode> 指向底层文件系统中的 inode 本体，使多个打开文件对象可以共享同一个文件节点。
}

impl OSInode {
    /// create a new inode in memory
    pub fn new(readable: bool, writable: bool, inode: Arc<Inode>) -> Self {
        Self {
            readable,
            writable,
            inner: unsafe { UPSafeCell::new(OSInodeInner { offset: 0, inode }) },
        }
    }
    /// read all data from the inode
    pub fn read_all(&self) -> Vec<u8> {    // yifan 2026/6/15: 这个函数会从当前文件偏移开始，把文件剩余内容全部读出并作为 Vec<u8> 返回。
        let mut inner = self.inner.exclusive_access();    // yifan 2026/6/15: 先独占访问 OSInodeInner，因为接下来既要访问底层 inode，又要更新当前读写偏移 offset。
        let mut buffer: Vec<u8> = Vec::with_capacity(512);    // yifan 2026/6/15: 先申请一个容量为 512 字节的临时缓冲区，用来每次分块读取文件内容。
        buffer.resize(512, 0);    // yifan 2026/6/15: 再把缓冲区的实际长度设为 512，这样 read_at 才能把数据真正写入这 512 字节空间。
        let mut v: Vec<u8> = Vec::new();    // yifan 2026/6/15: 这里创建最终结果缓冲区，用来累计保存整个文件读出来的所有字节。
        loop {    // yifan 2026/6/15: 进入循环，不断从当前 offset 开始分块读取，直到读到文件末尾为止。
            let len = inner.inode.read_at(inner.offset, &mut buffer);    // yifan 2026/6/15: 从当前偏移位置读取一块数据到 buffer 中，len 表示这次实际读到的字节数。
            if len == 0 {    // yifan 2026/6/15: 如果这次读到 0 字节，说明已经到达文件末尾，没有更多内容可读了。
                break;    // yifan 2026/6/15: 文件读完后退出循环。
            }
            inner.offset += len;    // yifan 2026/6/15: 把当前文件偏移向后推进 len 字节，表示这一段内容已经被读取过。
            v.extend_from_slice(&buffer[..len]);    // yifan 2026/6/15: extend_from_slice 的作用是把切片 &buffer[..len] 中的每个 u8 逐个追加到 Vec<u8> v 的末尾，所以这里只会把这次真正读到的前 len 个字节加入结果，而不是把整个 512 字节缓冲区都追加进去。
        }
        v    // yifan 2026/6/15: 循环结束后返回累计得到的全部文件内容。
    }
}

lazy_static! {    // yifan 2026/6/15: 这里用 lazy_static 定义延迟初始化的全局静态对象，因为初始化 ROOT_INODE 需要运行时打开文件系统并访问块设备，无法写成普通 static 常量。
    pub static ref ROOT_INODE: Arc<Inode> = {    // yifan 2026/6/15: ROOT_INODE 表示全局共享的根目录 inode，后续列目录、查找文件和创建文件都会从这个入口开始。
        let efs = EasyFileSystem::open(BLOCK_DEVICE.clone());    // yifan 2026/6/15: 先基于底层块设备 BLOCK_DEVICE 打开整个 EasyFileSystem 文件系统，得到文件系统管理对象 efs。
        Arc::new(EasyFileSystem::root_inode(&efs))    // yifan 2026/6/15: 再从 efs 中取出根目录对应的 inode，并用 Arc 包装成可共享的全局对象返回。
    };
}

/// List all apps in the root directory
pub fn list_apps() {
    println!("/**** APPS ****");
    for app in ROOT_INODE.ls() {
        println!("{}", app);
    }
    println!("**************/");
}

bitflags! {
    ///  The flags argument to the open() system call is constructed by ORing together zero or more of the following values:
    pub struct OpenFlags: u32 {
        /// readyonly
        const RDONLY = 0;
        /// writeonly
        const WRONLY = 1 << 0;
        /// read and write
        const RDWR = 1 << 1;
        /// create new file
        const CREATE = 1 << 9;
        /// truncate file size to 0
        const TRUNC = 1 << 10;
    }
}

impl OpenFlags {
    /// Do not check validity for simplicity
    /// Return (readable, writable)
    pub fn read_write(&self) -> (bool, bool) {
        if self.is_empty() {    // yifan 2026/6/15: is_empty 不是本文件手写的方法，而是上面 bitflags! 宏为 OpenFlags 自动生成的方法，用来判断当前标志位是否全为 0。
            (true, false)
        } else if self.contains(Self::WRONLY) {    // yifan 2026/6/15: contains 也不是本文件手写的方法，而是 bitflags! 宏自动生成的方法，用来判断当前标志集合里是否包含 WRONLY 这一位。
            (false, true)
        } else {
            (true, true)
        }
    }
}

/// Open a file
pub fn open_file(name: &str, flags: OpenFlags) -> Option<Arc<OSInode>> {    // yifan 2026/6/15: 这个函数按给定文件名和打开标志执行打开文件逻辑，并在成功时返回内核里的打开文件对象 Arc<OSInode>。
    let (readable, writable) = flags.read_write();    // yifan 2026/6/15: 先根据 flags 计算这次打开是否允许读和写，后面无论打开已有文件还是创建新文件都会把这两个权限写进 OSInode。
    if flags.contains(OpenFlags::CREATE) {    // yifan 2026/6/15: 如果带了 CREATE 标志，就进入“存在则清空后打开，不存在则创建后打开”的分支。
        if let Some(inode) = ROOT_INODE.find(name) {    // yifan 2026/6/15: 先在根目录下查找目标文件，判断它是否已经存在。
            // clear size
            inode.clear();    // yifan 2026/6/15: 如果文件已经存在，这里先把文件内容清空，相当于按当前实现执行“创建或覆盖”。
            Some(Arc::new(OSInode::new(readable, writable, inode)))    // yifan 2026/6/15: 然后基于这个底层 inode 构造新的 OSInode，并用 Some 包装表示打开成功。
        } else {
            // create file
            ROOT_INODE    // yifan 2026/6/15: 如果文件不存在，就从根目录出发创建一个同名新文件。
                .create(name)
                .map(|inode| Arc::new(OSInode::new(readable, writable, inode)))    // yifan 2026/6/15: create 成功时用 map 把新建文件的 inode 转成 Arc<OSInode> 返回，失败时自然保持 None。
        }
    } else {
        ROOT_INODE.find(name).map(|inode| {    // yifan 2026/6/15: 如果没有 CREATE 标志，就只尝试打开已有文件；find 失败时整个结果直接是 None。
            if flags.contains(OpenFlags::TRUNC) {    // yifan 2026/6/15: 如果额外带了 TRUNC 标志，就在打开前把已有文件内容截断为 0。
                inode.clear();    // yifan 2026/6/15: 这里执行真正的清空操作，把文件大小和内容重置为空。
            }
            Arc::new(OSInode::new(readable, writable, inode))    // yifan 2026/6/15: 最后把查找到的底层 inode 包装成带读写权限信息的打开文件对象返回。
        })
    }
}

impl File for OSInode {
    fn readable(&self) -> bool {
        self.readable
    }
    fn writable(&self) -> bool {
        self.writable
    }
    fn read(&self, mut buf: UserBuffer) -> usize {
        let mut inner = self.inner.exclusive_access();
        let mut total_read_size = 0usize;
        for slice in buf.buffers.iter_mut() {
            let read_size = inner.inode.read_at(inner.offset, *slice);
            if read_size == 0 {
                break;
            }
            inner.offset += read_size;
            total_read_size += read_size;
        }
        total_read_size
    }
    fn write(&self, buf: UserBuffer) -> usize {
        let mut inner = self.inner.exclusive_access();
        let mut total_write_size = 0usize;
        for slice in buf.buffers.iter() {
            let write_size = inner.inode.write_at(inner.offset, *slice);
            assert_eq!(write_size, slice.len());
            inner.offset += write_size;
            total_write_size += write_size;
        }
        total_write_size
    }
}
