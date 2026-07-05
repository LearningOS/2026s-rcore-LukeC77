use super::File;
use crate::mm::UserBuffer;
use crate::sync::UPSafeCell;
use alloc::sync::{Arc, Weak};

use crate::task::suspend_current_and_run_next;

/// IPC pipe
pub struct Pipe {    // yifan 2026/6/21: Pipe 表示一个“管道端点”对象，也就是某个文件描述符视角下看到的管道，而不是把读写双方和全部状态都揉成一个单独实例。
    readable: bool,    // yifan 2026/6/21: 这个布尔值表示当前端点是否允许读；读端通常为 true，写端通常为 false，用来区分这是管道的哪一端。
    writable: bool,    // yifan 2026/6/21: 这个布尔值表示当前端点是否允许写；写端通常为 true，读端通常为 false，因此 readable/writable 共同描述了这个 Pipe 的操作权限。
    buffer: Arc<UPSafeCell<PipeRingBuffer>>,    // yifan 2026/6/21: 这里保存真正共享的底层环形缓冲区；Arc 让多个 Pipe 端点共享同一份数据区，UPSafeCell 允许内核在共享场景下安全地修改缓冲区内容。
}

impl Pipe {
    /// create readable pipe
    pub fn read_end_with_buffer(buffer: Arc<UPSafeCell<PipeRingBuffer>>) -> Self {
        Self {
            readable: true,
            writable: false,
            buffer,
        }
    }
    /// create writable pipe
    pub fn write_end_with_buffer(buffer: Arc<UPSafeCell<PipeRingBuffer>>) -> Self {
        Self {
            readable: false,
            writable: true,
            buffer,
        }
    }
}

// yifan 2026/6/21: 管道可以看成内核中的共享通信通道，真正共享的数据区是 PipeRingBuffer；
// 读端和写端分别由不同的 Pipe 对象表示，但它们会共同指向同一个底层缓冲区。
// 例如进程 A 要向进程 B 发送消息时，A 持有写端并把数据写入 arr，B 持有读端并从 arr 中把这些数据读走；
// 数据不是直接从 A 的用户内存跳到 B 的用户内存，而是先进入管道缓冲区，再由 B 从管道中取出。
// head 表示下一次读取的位置，tail 表示下一次写入的位置，它们都会在数组末尾回绕到开头。
// 由于 head == tail 既可能表示空也可能表示满，所以代码额外用 RingBufferStatus 来区分缓冲区当前是 Empty、Full 还是 Normal。
const RING_BUFFER_SIZE: usize = 32;

#[derive(Copy, Clone, PartialEq)]    // yifan 2026/6/21: 这里为状态枚举派生 Copy、Clone 和 PartialEq，使它可以被按值复制、显式克隆，并且能够直接参与相等比较，例如判断当前缓冲区是否为空或已满。
enum RingBufferStatus {    // yifan 2026/6/21: RingBufferStatus 用来描述环形缓冲区的整体状态，因为仅靠 head 和 tail 在某些情况下无法区分“空”和“满”。
    Full,    // yifan 2026/6/21: Full 表示环形缓冲区已经写满，当前不能继续写入新字节。
    Empty,    // yifan 2026/6/21: Empty 表示环形缓冲区中没有可读数据，当前不能读取新字节。
    Normal,    // yifan 2026/6/21: Normal 表示既不是满也不是空，缓冲区处于正常的中间状态，此时通常既可能读也可能写。
}

pub struct PipeRingBuffer {    // yifan 2026/6/21: PipeRingBuffer 是管道真正共享的数据区，负责保存字节内容以及当前读写位置和附加状态。
    arr: [u8; RING_BUFFER_SIZE],    // yifan 2026/6/21: arr 是底层字节数组，真正的管道数据就存放在这里，容量由前面的 RING_BUFFER_SIZE 决定。
    head: usize,    // yifan 2026/6/21: head 是读指针，表示下一次读取应该从数组的哪个位置取数据。
    tail: usize,    // yifan 2026/6/21: tail 是写指针，表示下一次写入应该把数据放到数组的哪个位置。
    status: RingBufferStatus,    // yifan 2026/6/21: status 配合 head 和 tail 一起工作，用来消除 head == tail 时“到底是空还是满”的歧义。
    write_end: Option<Weak<Pipe>>,    // yifan 2026/6/21: 这里用弱引用记录管道写端是否还存在；读端可据此判断如果写端全部关闭且缓冲区为空，就不会再有新数据到来。使用 Weak 而不是 Arc 是因为这里只需要观察写端是否存活，不希望额外增加强引用计数。
}

impl PipeRingBuffer {
    pub fn new() -> Self {
        Self {
            arr: [0; RING_BUFFER_SIZE],
            head: 0,
            tail: 0,
            status: RingBufferStatus::Empty,
            write_end: None,
        }
    }
    pub fn set_write_end(&mut self, write_end: &Arc<Pipe>) {    // yifan 2026/6/21: 这个方法把“该缓冲区对应的写端是谁”登记到 PipeRingBuffer 中，供后续读端判断写端是否仍然存在。
        self.write_end = Some(Arc::downgrade(write_end));    // yifan 2026/6/21: 这里把写端的 Arc<Pipe> 转成 Weak<Pipe> 再保存，表示只保留一个观察引用而不增加强引用计数；这样缓冲区可以检查写端是否还活着，但不会因为自己持有强引用而阻止写端被正常释放。
    }
    pub fn write_byte(&mut self, byte: u8) {
        self.status = RingBufferStatus::Normal;
        self.arr[self.tail] = byte;
        self.tail = (self.tail + 1) % RING_BUFFER_SIZE;
        if self.tail == self.head {
            self.status = RingBufferStatus::Full;
        }
    }
    pub fn read_byte(&mut self) -> u8 {    // yifan 2026/6/21: 这个方法用于从管道环形缓冲区读取 1 个字节；调用它之前应当已经确保缓冲区不是空的，否则读取结果没有意义。
        self.status = RingBufferStatus::Normal;    // yifan 2026/6/21: 一旦开始执行读操作，就先把状态临时设为 Normal；因为接下来要根据最新的 head/tail 关系重新判断当前是否已经变空。
        let c = self.arr[self.head];    // yifan 2026/6/21: 从当前 head 指向的位置取出 1 个字节；head 表示“下一次读取的位置”，因此这里读取的是当前队头元素。
        self.head = (self.head + 1) % RING_BUFFER_SIZE;    // yifan 2026/6/21: 读完后把队头向前移动一格，并在到达数组末尾时通过取模回绕到 0，这正是环形队列的移动方式。
        if self.head == self.tail {
            self.status = RingBufferStatus::Empty;    // yifan 2026/6/21: 如果这次读取之后 head 与 tail 相等，说明最后一个可读字节刚被取走，缓冲区在“读后语境”下已经为空；单看 head == tail 本身无法区分空或满，因此必须在 read_byte 中同步把状态更新为 Empty。
        }
        c    // yifan 2026/6/21: 返回刚刚从缓冲区队头读出的那个字节。
    }
    pub fn available_read(&self) -> usize { // yifan 2026/6/21:计算管道中还有多少个字符可以读取
        if self.status == RingBufferStatus::Empty {
            0
        } else if self.tail > self.head {
            self.tail - self.head
        } else {
            self.tail + RING_BUFFER_SIZE - self.head
        }
    }
    pub fn available_write(&self) -> usize {
        if self.status == RingBufferStatus::Full {
            0
        } else {
            RING_BUFFER_SIZE - self.available_read()
        }
    }
    pub fn all_write_ends_closed(&self) -> bool {    // yifan 2026/6/21: 这个方法用于判断这条管道当前是否已经没有任何写端仍然存活；如果所有写端都关闭了，那么缓冲区中的数据读完之后就不会再有新数据补充。
        self.write_end.as_ref().unwrap().upgrade().is_none()    // yifan 2026/6/21: 这里先取出 PipeRingBuffer 中保存的写端弱引用，再尝试用 upgrade 把 Weak<Pipe> 升级成 Arc<Pipe>；如果升级失败返回 None，说明写端的强引用计数已经为 0，也就是所有写端都被关闭了，因此这个表达式最终返回 true。
    }
}

/// Return (read_end, write_end)
pub fn make_pipe() -> (Arc<Pipe>, Arc<Pipe>) {
    let buffer = Arc::new(unsafe { UPSafeCell::new(PipeRingBuffer::new()) });
    let read_end = Arc::new(Pipe::read_end_with_buffer(buffer.clone()));
    let write_end = Arc::new(Pipe::write_end_with_buffer(buffer.clone()));
    buffer.exclusive_access().set_write_end(&write_end);
    (read_end, write_end)
}

impl File for Pipe {
    fn readable(&self) -> bool {
        self.readable
    }
    fn writable(&self) -> bool {
        self.writable
    }
    fn read(&self, buf: UserBuffer) -> usize {    // yifan 2026/6/21: 这个 read 实现负责把管道中的数据读到用户缓冲区；它会在“有数据就读取、没数据时判断写端是否关闭、否则阻塞等待”之间循环。
        assert!(self.readable());    // yifan 2026/6/21: 先确保当前 Pipe 确实是可读端；如果把写端当成读端来用，这里会直接失败。
        let want_to_read = buf.len();    // yifan 2026/6/21: 记录用户这次总共希望读取多少字节，后面读满这个数量就可以返回。
        let mut buf_iter = buf.into_iter();    // yifan 2026/6/21: 把用户缓冲区变成逐字节可写的迭代器，后面可以一次取出一个位置，把读到的字节写进去。
        let mut already_read = 0usize;    // yifan 2026/6/21: 记录本次 read 调用到目前为止已经成功读取了多少字节。
        loop {
            let mut ring_buffer = self.buffer.exclusive_access();    // yifan 2026/6/21: 独占访问底层 PipeRingBuffer，准备检查当前缓冲区状态并执行实际读取。
            let loop_read = ring_buffer.available_read();    // yifan 2026/6/21: 计算当前这一轮最多还能从管道缓冲区中读出多少字节。
            if loop_read == 0 {
                if ring_buffer.all_write_ends_closed() {
                    return already_read;    // yifan 2026/6/21: 如果当前没有可读数据，而且所有写端都已经关闭了，就说明以后也不会再有新数据写入；此时直接返回当前已读字节数，若为 0 则相当于读到 EOF。
                }
                drop(ring_buffer);    // yifan 2026/6/21: 在挂起当前任务之前必须先释放对环形缓冲区的独占访问，否则其他任务将无法拿到缓冲区并向管道写数据。
                suspend_current_and_run_next();    // yifan 2026/6/21: 当前暂时没有数据但写端还活着，因此不能返回，只能挂起等待；等以后被重新调度时再回来继续尝试读取。
                continue;
            }
            for _ in 0..loop_read {
                if let Some(byte_ref) = buf_iter.next() {
                    unsafe {
                        *byte_ref = ring_buffer.read_byte();    // yifan 2026/6/21: 从管道环形缓冲区读出 1 个字节，并写入用户缓冲区当前位置；read_byte 会同时推进 head 并更新 Empty/Normal 状态。
                    }
                    already_read += 1;    // yifan 2026/6/21: 成功搬运 1 个字节后，累计已读字节数加一。
                    if already_read == want_to_read {
                        return want_to_read;    // yifan 2026/6/21: 如果已经达到用户请求的读取总量，就结束本次 read 并返回。
                    }
                } else {
                    return already_read;    // yifan 2026/6/21: 如果用户缓冲区已经没有可写位置，即使管道里还有数据，也只能先返回当前已经读到的字节数。
                }
            }
        }
    }
    fn write(&self, buf: UserBuffer) -> usize {
        assert!(self.writable());
        let want_to_write = buf.len();
        let mut buf_iter = buf.into_iter();
        let mut already_write = 0usize;
        loop {
            let mut ring_buffer = self.buffer.exclusive_access();
            let loop_write = ring_buffer.available_write();
            if loop_write == 0 {
                drop(ring_buffer);
                suspend_current_and_run_next();
                continue;
            }
            // write at most loop_write bytes
            for _ in 0..loop_write {
                if let Some(byte_ref) = buf_iter.next() {
                    ring_buffer.write_byte(unsafe { *byte_ref });
                    already_write += 1;
                    if already_write == want_to_write {
                        return want_to_write;
                    }
                } else {
                    return already_write;
                }
            }
        }
    }
}
