use super::{BlockDevice, BLOCK_SZ};
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use lazy_static::*;
use spin::Mutex;
/// Cached block inside memory
pub struct BlockCache {
    /// cached block data。yifan 2026/6/1: 是一个 512 字节的数组，表示位于内存中的缓冲区；
    cache: [u8; BLOCK_SZ],
    /// underlying block id. yifan 2026/6/1: 记录了这个块缓存来自于磁盘中的块的编号；
    block_id: usize,
    /// underlying block device. yifan 2026/6/1: 是一个底层块设备的引用，可通过它进行块读写；
    block_device: Arc<dyn BlockDevice>, // yifan 2026/6/1: dyn BlockDevice：trait 对象类型（运行时多态），表示“某个实现了 BlockDevice 的具体类型”，但此处不关心具体是哪种设备。
    /// whether the block is dirty. yifan 2026/6/1: 标记这个块缓存是否被修改过（即是否与磁盘中的数据不一致），用于决定何时需要将数据写回磁盘。
    modified: bool,
}

impl BlockCache {
    /// Load a new BlockCache from disk.
    pub fn new(block_id: usize, block_device: Arc<dyn BlockDevice>) -> Self {
        let mut cache = [0u8; BLOCK_SZ];
        block_device.read_block(block_id, &mut cache);
        Self {
            cache,
            block_id,
            block_device,
            modified: false,
        }
    }
    /// Get the address of an offset inside the cached block data
    fn addr_of_offset(&self, offset: usize) -> usize {
        &self.cache[offset] as *const _ as usize
    }

    pub fn get_ref<T>(&self, offset: usize) -> &T    // yifan 2026/6/1: get_ref 是泛型方法，用于把缓冲区中 offset 处的数据按类型 T 解释为不可变引用 &T。
    where
        T: Sized,    // yifan 2026/6/1: Trait Bound 要求 T 在编译期大小已知，才能用 size_of::<T>() 做边界计算。
    {
        let type_size = core::mem::size_of::<T>();    // yifan 2026/6/1: type_size 用于校验“读取一个 T 是否完整落在当前块内”；只读引用同样可能越界，因此必须检查。
        assert!(offset + type_size <= BLOCK_SZ);    // yifan 2026/6/1: 保证从 offset 起读取 T 不越过 BLOCK_SZ 边界，避免跨块/越界访问。
        let addr = self.addr_of_offset(offset);    // yifan 2026/6/1: 取得 cache[offset] 的起始地址，作为后续类型重解释的基址。
        unsafe { &*(addr as *const T) }    // yifan 2026/6/1: 先将整数地址转为 *const T，再解引用得到该位置的 T，最后取引用形成 &T；返回引用生命周期由 &self 推导，不超过 BlockCache。
    }

    pub fn get_mut<T>(&mut self, offset: usize) -> &mut T
    where
        T: Sized,
    {
        let type_size = core::mem::size_of::<T>();    // yifan 2026/6/1: 用于计算一个 T 占用的字节数，配合边界检查避免把可变引用落到块外。
        assert!(offset + type_size <= BLOCK_SZ);    // yifan 2026/6/1: 保证从 offset 开始的 T 完整落在当前缓存块内，避免越界访问。
        self.modified = true;
        let addr = self.addr_of_offset(offset);    // yifan 2026/6/1: 获取 cache[offset] 的起始地址，作为后续按 T 解释的基址。
        unsafe { &mut *(addr as *mut T) }    // yifan 2026/6/1: 先把 addr 转成 *mut T，再解引用得到该地址处的 T，最后取可变引用 &mut T；此处为 unsafe，需保证地址有效/可写、满足 T 对齐且不存在冲突别名。
    }

    pub fn read<T, V>(&self, offset: usize, f: impl FnOnce(&T) -> V) -> V {    // yifan 2026/6/1: read 表示“按类型读 + 回调处理”；T 是 offset 处字节要解释成的数据结构类型（如 DiskInode），V 是期望返回结果类型。 // yifan 2026/6/1: DiskInode 指“磁盘上的 inode 数据结构类型”（通常定义在 layout.rs），描述文件元数据在磁盘块中的布局。
        f(self.get_ref(offset))    // yifan 2026/6/1: 执行流程是先通过 get_ref 从 offset 处得到 &T，再交给闭包 f: FnOnce(&T)->V 处理并返回该结果；例如 |inode: &DiskInode| inode.size 时 T=DiskInode, V=u32，inode 只是参数名。 // yifan 2026/6/1: 这里的 inode 不是新类型，只是闭包参数变量名，类型是 &DiskInode（某个磁盘 inode 的只读引用）。
    }

    pub fn modify<T, V>(&mut self, offset: usize, f: impl FnOnce(&mut T) -> V) -> V {
        f(self.get_mut(offset))
    }

    pub fn sync(&mut self) {    // yifan 2026/6/1: sync 的职责是将脏块缓存回写到底层块设备，并在成功路径上清除脏标记。
        if self.modified {    // yifan 2026/6/1: 仅当缓存被修改过时才执行写回，未修改则跳过 I/O。
            self.modified = false;    // yifan 2026/6/1: 先清除脏标记，表示当前块准备同步为“已落盘”状态。
            self.block_device.write_block(self.block_id, &self.cache);    // yifan 2026/6/1: 将 cache 的整块数据写入 block_id 对应的磁盘块，完成内存与磁盘一致化。
        }
    }
}

impl Drop for BlockCache {    // yifan 2026/6/1: 这里实现 Drop 不是为了手动释放内存，而是定义析构时的附加行为。
    fn drop(&mut self) {    // yifan 2026/6/1: Rust 采用 RAII；drop 返回后字段会自动析构（如 Arc 引用计数递减、栈上数据自动回收）。
        self.sync()    // yifan 2026/6/1: 析构前先做业务一致性动作：把脏块回写磁盘；资源释放仍由 Rust 自动完成。
    }
}
/// Use a block cache of 16 blocks
const BLOCK_CACHE_SIZE: usize = 16; // yifan 2026/6/1: 定义 block cache 的容量，即最多缓存多少块数据；超过这个数量时需要进行替换（substitute）以腾出空间。

/// yifan 2026/6/1: 块缓存全局管理器
pub struct BlockCacheManager {
    queue: VecDeque<(usize, Arc<Mutex<BlockCache>>)>, // yifan 2026/6/1: 这里使用 VecDeque 来维护一个块缓存队列，元素是 (block_id, block_cache) 的元组；VecDeque 支持高效的头尾插入删除，适合实现简单的 FIFO 替换策略。
}

impl BlockCacheManager {
    pub fn new() -> Self {
        Self {
            queue: VecDeque::new(),
        }
    }

    pub fn get_block_cache(
        &mut self,
        block_id: usize,
        block_device: Arc<dyn BlockDevice>,
    ) -> Arc<Mutex<BlockCache>> {
        if let Some(pair) = self.queue.iter().find(|pair| pair.0 == block_id) {
            Arc::clone(&pair.1)
        } else {
            // substitute
            if self.queue.len() == BLOCK_CACHE_SIZE {
                // from front to tail
                if let Some((idx, _)) = self
                    .queue
                    .iter()
                    .enumerate()
                    .find(|(_, pair)| Arc::strong_count(&pair.1) == 1)
                {
                    self.queue.drain(idx..=idx);    // yifan 2026/6/1: drain 的语义是按“范围”移除元素并返回被移除元素的迭代器；这里 idx..=idx 只覆盖一个下标，所以实际是删除 queue 中 idx 这一项。 // yifan 2026/6/1: VecDeque 也有 remove(idx) 可删单个元素并返回 Option<T>；本处用 drain(idx..=idx) 与 remove(idx) 在效果上接近，但 drain 更强调“范围删除”。
                } else {
                    panic!("Run out of BlockCache!");
                }
            }
            // load block into mem and push back
            let block_cache = Arc::new(Mutex::new(BlockCache::new(
                block_id,
                Arc::clone(&block_device),    // yifan 2026/6/1: 这里用 Arc::clone 是为了复制共享指针而非复制底层设备对象本体；这样可把一个句柄移动进 BlockCache，同时外层仍可继续持有/使用原 block_device（仅引用计数 +1）。
            )));
            self.queue.push_back((block_id, Arc::clone(&block_cache)));
            block_cache
        }
    }
}

lazy_static! {
    /// The global block cache manager
    pub static ref BLOCK_CACHE_MANAGER: Mutex<BlockCacheManager> =
        Mutex::new(BlockCacheManager::new());
}
/// Get the block cache corresponding to the given block id and block device
/// 根据block_id和block_device获取对应的块缓存；如果缓存中已有该块则直接返回，否则加载该块到内存并插入缓存队列（必要时进行替换），最后返回新加载的块缓存。
pub fn get_block_cache(    // yifan 2026/6/1: 该函数先短暂锁住全局管理器以安全操作 queue（查找/插入/替换），再返回某个具体块的 Arc<Mutex<BlockCache>>。
    block_id: usize,
    block_device: Arc<dyn BlockDevice>,
) -> Arc<Mutex<BlockCache>> {    // yifan 2026/6/1: 返回 Arc 是为了让管理器与多个调用方共享同一个 BlockCache 所有权，管理器解锁后调用方仍可继续安全使用。
    BLOCK_CACHE_MANAGER    // yifan 2026/6/1: 这是“全局管理器锁”，职责是保护缓存映射表 queue，而不是长期保护某个块的数据访问。
        .lock()
        .get_block_cache(block_id, block_device)    // yifan 2026/6/1: 这一步结束即返回并释放管理器锁；后续 A/B 竞争的是具体 BlockCache 的 Mutex，只有访问同一块时才互斥等待。
}
/// Sync all block cache to block device
pub fn block_cache_sync_all() {
    let manager = BLOCK_CACHE_MANAGER.lock();
    for (_, cache) in manager.queue.iter() {
        cache.lock().sync();
    }
}
