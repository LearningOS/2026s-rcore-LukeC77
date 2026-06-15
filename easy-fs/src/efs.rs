use super::{
    block_cache_sync_all, get_block_cache, Bitmap, BlockDevice, DiskInode, DiskInodeType, Inode,
    SuperBlock,
};
use crate::BLOCK_SZ;
use alloc::sync::Arc;
use spin::Mutex;
// yifan 2026/6/5:
// 实现 easy-fs 的整体磁盘布局，将各段区域及上面的磁盘数据结构整合起来就是简易文件系统 EasyFileSystem 的职责。
// 它知道每个布局区域所在的位置，磁盘块的分配和回收也需要经过它才能完成，因此某种意义上讲它还可以看成一个磁盘块管理器。
///An easy file system on block
pub struct EasyFileSystem {
    ///Real device
    pub block_device: Arc<dyn BlockDevice>, // yifan 2026/6/5: 块设备指针，该指针会被拷贝并传递给下层的数据结构，让它们也能够直接访问块设备。
    ///Inode bitmap
    pub inode_bitmap: Bitmap, // yifan 2026/6/5: 索引节点位图
    ///Data bitmap
    pub data_bitmap: Bitmap, // yifan 2026/6/5: 数据块位图
    inode_area_start_block: u32, // yifan 2026/6/5: 索引节点区起始块号
    data_area_start_block: u32, // yifan 2026/6/5: 数据区起始块号
}

type DataBlock = [u8; BLOCK_SZ];
/// An easy fs over a block device
impl EasyFileSystem {
    /// A data block of block size
    pub fn create(
        block_device: Arc<dyn BlockDevice>,
        total_blocks: u32,
        inode_bitmap_blocks: u32, // yifan 2026/6/5: 是“inode 位图区”占多少块。这一部分不存 inode 内容，只存位图，也就是一堆二进制位, 可以理解为座位表，记录哪些座位有人。inode_area_blocks：座位本身，真正让人坐的位置
    ) -> Arc<Mutex<Self>> {
        // calculate block size of areas & create bitmaps
        let inode_bitmap = Bitmap::new(1, inode_bitmap_blocks as usize);    // yifan 2026/6/5: 创建 inode 位图，其中 1 表示 inode 位图区从磁盘第 1 块开始，因为第 0 块留给 SuperBlock。
        let inode_num = inode_bitmap.maximum();    // yifan 2026/6/5: 计算这个 inode 位图最多能管理多少个 inode，本质上是一位对应一个 inode，因此这里是在算位图总共能表示多少个 inode。
        // yifan 2026/6/5: inode_area_blocks这里不是要求所有 DiskInode 恰好填满某一个 512B 的块，而是把所有 DiskInode 看成一串连续的字节，顺序存放到 inode 区的多个连续磁盘块里。
        // yifan 2026/6/5: 每个磁盘块固定是 BLOCK_SZ=512 字节，因此只要先算出全部 DiskInode 需要的总字节数，再除以 512 并向上取整，就能得到至少需要多少个块来容纳它们。
        // yifan 2026/6/5: 如果最后一个块没有放满也没有关系，剩余空间只是块内碎片；磁盘分配的最小单位仍然是整块，所以必须按块数向上取整。
        let inode_area_blocks =
            ((inode_num * core::mem::size_of::<DiskInode>() + BLOCK_SZ - 1) / BLOCK_SZ) as u32;    // yifan 2026/6/5: 计算 inode 区需要多少个磁盘块，先用 inode 总数乘单个 DiskInode 大小得到总字节数，再按 BLOCK_SZ 向上取整换算成块数。
        let inode_total_blocks = inode_bitmap_blocks + inode_area_blocks;    // yifan 2026/6/5: 计算 inode 相关区域总共占多少块，也就是 inode 位图区和 inode 区两部分之和。
        let data_total_blocks = total_blocks - 1 - inode_total_blocks;    // yifan 2026/6/5: 从总块数中减去 1 个超级块和 inode 相关区域后，得到数据区相关区域可用的总块数，这里面还包括后面要划分的数据位图区。
        let data_bitmap_blocks = (data_total_blocks + 4096) / 4097;    // yifan 2026/6/5: 计算数据位图需要多少块；一个位图块可表示 4096 个数据块，而 data_total_blocks 同时包含数据位图块和数据块本身，因此可由关系式推导出这里的公式。
        let data_area_blocks = data_total_blocks - data_bitmap_blocks;    // yifan 2026/6/5: data_total_blocks 表示数据相关区域总共可用的块数，其中既包括数据位图区也包括真正的数据区，因此这里减去 data_bitmap_blocks 后，剩下的才是真正存放文件内容的数据区块数。
        let data_bitmap = Bitmap::new(
            (1 + inode_bitmap_blocks + inode_area_blocks) as usize,    // yifan 2026/6/5: 数据位图区的起始块号要放在 SuperBlock、inode 位图区和 inode 区之后；其中第 0 块是 SuperBlock，所以这里用 1 加上前面两段 inode 相关区域的块数。
            data_bitmap_blocks as usize,    // yifan 2026/6/5: 数据位图区本身占用的块数，由前面计算出的 data_bitmap_blocks 给出，这个值决定了位图一共能管理多少个数据块。
        );
        let mut efs = Self {
            block_device: Arc::clone(&block_device),
            inode_bitmap,
            data_bitmap,
            inode_area_start_block: 1 + inode_bitmap_blocks,
            data_area_start_block: 1 + inode_total_blocks + data_bitmap_blocks,
        };
        // clear all blocks
        for i in 0..total_blocks {
            get_block_cache(i as usize, Arc::clone(&block_device))
                .lock()
                .modify(0, |data_block: &mut DataBlock| {
                    for byte in data_block.iter_mut() {
                        *byte = 0;
                    }
                });
        }
        // initialize SuperBlock
        get_block_cache(0, Arc::clone(&block_device)).lock().modify(
            0,
            |super_block: &mut SuperBlock| {
                super_block.initialize(
                    total_blocks,
                    inode_bitmap_blocks,
                    inode_area_blocks,
                    data_bitmap_blocks,
                    data_area_blocks,
                );
            },
        );
        // write back immediately
        // create a inode for root node "/"
        assert_eq!(efs.alloc_inode(), 0);    // yifan 2026/6/6: 分配一个新的 inode，并断言它的编号必须是 0；因为这是新创建的文件系统，第一个分配到的 inode 应该就是 0，而 easy-fs 约定根目录 / 使用 inode 0。
        let (root_inode_block_id, root_inode_offset) = efs.get_disk_inode_pos(0);    // yifan 2026/6/6: 查询 inode 0 在块设备上的逻辑存储位置，返回它所在的逻辑块号和块内偏移；这里不是 HDD/SSD 的物理地址，而是 easy-fs 这套布局下的逻辑位置，后面据此把 inode 编号转换成可读写的存储位置。
        get_block_cache(root_inode_block_id as usize, Arc::clone(&block_device))    // yifan 2026/6/6: 取出根目录 inode 所在逻辑块的缓存；这里访问的是 block_device 视角下的逻辑块，而不是底层硬件的物理扇区。
            .lock()    // yifan 2026/6/6: 对这个块缓存加锁，因为接下来要修改块里的内容。
            .modify(root_inode_offset, |disk_inode: &mut DiskInode| {    // yifan 2026/6/6: 从该逻辑块内的 root_inode_offset 字节偏移处开始修改，并把这段内容当成一个 DiskInode；因此这个 offset 也是块内逻辑偏移，不是硬件层地址。
                disk_inode.initialize(DiskInodeType::Directory);    // yifan 2026/6/6: 将这个 DiskInode 初始化为目录类型，因为 inode 0 被约定为根目录 /，所以这里要把它设成目录而不是普通文件。
            });
        block_cache_sync_all();    // yifan 2026/6/6: 将前面通过块缓存修改过的数据统一刷回块设备；这样清空磁盘块、写入 SuperBlock、初始化根目录 inode 等操作才会真正落到存储介质上，而不只是停留在内存缓存里。
        Arc::new(Mutex::new(efs))    // yifan 2026/6/6: 将初始化完成的 efs 封装成 Arc<Mutex<EasyFileSystem>> 并返回；Arc 用于共享同一个文件系统实例，Mutex 用于在访问或修改它时提供互斥保护。
    }
    /// Open a block device as a filesystem
    /// EasyFileSystem::open 用来打开一个已经存在的 easy-fs 文件系统。
    /// 它只需要读取块设备的 block 0，也就是超级块 SuperBlock，检查魔数是否合法，
    /// 然后根据超级块中记录的各区域大小，重新构造内存中的 EasyFileSystem 对象。
    /// open 不会清零磁盘，也不会重新初始化根目录，因为磁盘上的文件系统内容已经存在。
    pub fn open(block_device: Arc<dyn BlockDevice>) -> Arc<Mutex<Self>> {    // yifan 2026/6/6: 从一个已经存在的块设备中打开 easy-fs，而不是像 create() 那样重新格式化并创建文件系统。
        // read SuperBlock
        get_block_cache(0, Arc::clone(&block_device))    // yifan 2026/6/6: 读取块设备的第 0 个逻辑块，因为 easy-fs 约定 SuperBlock 存放在第 0 块。
            .lock()    // yifan 2026/6/6: 对这个块缓存加锁，准备读取其中的超级块内容。
            .read(0, |super_block: &SuperBlock| {    // yifan 2026/6/6: 从块内偏移 0 开始读取，并把这段内容解释成一个 SuperBlock；open() 需要先知道整个文件系统的布局信息。
                assert!(super_block.is_valid(), "Error loading EFS!");    // yifan 2026/6/6: 检查读到的超级块是否合法；如果它不是一个有效的 easy-fs 超级块，说明这个块设备不是 easy-fs 或者内容已经损坏。
                let inode_total_blocks =
                    super_block.inode_bitmap_blocks + super_block.inode_area_blocks;    // yifan 2026/6/6: 计算 inode 相关区域总共占多少块，即 inode 位图区和 inode 区之和，后面据此推导数据相关区域的位置。
                let efs = Self {    // yifan 2026/6/6: 根据超级块中记录的布局信息，在内存中恢复一个 EasyFileSystem 对象，而不是重新初始化磁盘内容。
                    block_device,    // yifan 2026/6/6: 保存底层块设备指针，后续文件系统操作仍然通过它访问逻辑块。
                    inode_bitmap: Bitmap::new(1, super_block.inode_bitmap_blocks as usize),    // yifan 2026/6/6: 恢复 inode 位图对象；inode 位图区固定从第 1 个逻辑块开始，占用的块数由超级块记录。
                    data_bitmap: Bitmap::new(
                        (1 + inode_total_blocks) as usize,    // yifan 2026/6/6: 数据位图区的起始逻辑块号位于 SuperBlock 和整个 inode 相关区域之后，因此这里用 1 加 inode_total_blocks。
                        super_block.data_bitmap_blocks as usize,    // yifan 2026/6/6: 数据位图区本身占用多少块，直接由超级块中的记录恢复。
                    ),
                    inode_area_start_block: 1 + super_block.inode_bitmap_blocks,    // yifan 2026/6/6: inode 区起始逻辑块号位于 SuperBlock 之后，再跳过 inode 位图区即可到达。
                    data_area_start_block: 1 + inode_total_blocks + super_block.data_bitmap_blocks,    // yifan 2026/6/6: 数据区起始逻辑块号位于 SuperBlock、inode 相关区域和数据位图区之后，这是后续定位数据块的基准位置。
                };
                Arc::new(Mutex::new(efs))    // yifan 2026/6/6: 将恢复出的文件系统对象封装成 Arc<Mutex<_>> 返回，供多个地方共享访问，并通过互斥锁保护并发读写。
            })
    }
    /// Get the root inode of the filesystem
    /// 1. 从 EasyFileSystem 中取出 block_device
    /// 2. 调用 get_disk_inode_pos(0)
    /// 3. 得到 inode 0 所在的磁盘块号 block_id 和块内偏移 block_offset
    /// 4. 调用 Inode::new 创建根目录 Inode
    pub fn root_inode(efs: &Arc<Mutex<Self>>) -> Inode {
        let block_device = Arc::clone(&efs.lock().block_device);
        // acquire efs lock temporarily
        let (block_id, block_offset) = efs.lock().get_disk_inode_pos(0);
        // release efs lock
        Inode::new(block_id, block_offset, Arc::clone(efs), block_device)
    }
    /// Get inode by id
    /// 根据 inode 编号，计算这个 DiskInode 存放在哪个磁盘块中，以及在这个块内的偏移量是多少。
    pub fn get_disk_inode_pos(&self, inode_id: u32) -> (u32, usize) {
        let inode_size = core::mem::size_of::<DiskInode>();    // yifan 2026/6/5: 单个 DiskInode 占用的字节数，它是 inode 区里重复排列的基本单位。DiskInode 大小 = 128 字节
        let inodes_per_block = (BLOCK_SZ / inode_size) as u32;    // yifan 2026/6/5: 一个 512B 磁盘块里最多能顺序放下多少个 DiskInode；如果不能整除，块尾剩余字节会空着。一个磁盘块可以保存 4 个 DiskInode。
        // 例如：inode_area_start_block = 10，那么 inode 区域从磁盘 block 10 开始。
        // 如果一个块能放 4 个 inode：
        // inode_id 0,1,2,3   -> block 10
        // inode_id 4,5,6,7   -> block 11
        // inode_id 8,9,10,11 -> block 12
        // 所以：inode_id / inodes_per_block表示这个 inode 在 inode 区域中的第几个块里。
        let block_id = self.inode_area_start_block + inode_id / inodes_per_block;    // yifan 2026/6/5: 用 inode_id 除以每块可容纳的 inode 数量，就能定位这个 inode 落在哪个磁盘块。
        (
            block_id,
            // 例如一个块放 4 个 inode，每个 inode 128 字节, inode_size=128：
            // inode_id 0 -> offset 0
            // inode_id 1 -> offset 128
            // inode_id 2 -> offset 256
            // inode_id 3 -> offset 384
            (inode_id % inodes_per_block) as usize * inode_size,    // yifan 2026/6/5: 用余数算出它在该块中的第几个位置，再乘 inode_size 得到块内偏移，因此 DiskInode 是在多个块中连续排布的，而不是一个 inode 对应一个块。
        )
    }
    /// Get data block by id
    /// data_block_id 是 data 位图分配出来的编号，它是数据块区域内部的相对编号。但真正读写块设备时，需要整个磁盘上的真实块号。
    pub fn get_data_block_id(&self, data_block_id: u32) -> u32 {
        self.data_area_start_block + data_block_id
    }
    /// Allocate a new inode
    /// 从 inode_bitmap 中找一个空闲 bit，并把它置为 1。这个 bit 的编号就作为 inode 编号返回。
    // 例如：
    // inode_bitmap 分配到 bit 0
    // => inode_id = 0
    // inode_bitmap 分配到 bit 5
    // => inode_id = 5
    // 注意：这里返回的是 inode 编号，不是磁盘块编号。
    pub fn alloc_inode(&mut self) -> u32 {
        self.inode_bitmap.alloc(&self.block_device).unwrap() as u32
    }

    /// Allocate a data block
    /// 分配一个真实磁盘数据块编号,它先从 data_bitmap 中分配一个空闲 bit。这个 bit 编号表示：数据块区域内部的相对编号
    /// 例如：data_bitmap 分配到 bit 3 => data area 中第 3 个数据块
    /// 但是返回值不是这个相对编号，而是：真实块设备上的 block_id，所以要加上 self.data_area_start_block
    /// data_area_start_block = 100
    /// data_bitmap 分配到 bit 3
    /// alloc_data 返回：
    /// 100 + 3 = 103
    /// 也就是说，返回的是块设备上的真实块号 103。
    pub fn alloc_data(&mut self) -> u32 {
        self.data_bitmap.alloc(&self.block_device).unwrap() as u32 + self.data_area_start_block
    }
    /// Deallocate a data block 回收一个数据块
    pub fn dealloc_data(&mut self, block_id: u32) { // yifan 2026/6/5: 这里传入的 block_id 是：块设备上的真实磁盘块编号, 不是 data bitmap 中的 bit 编号。
        get_block_cache(block_id as usize, Arc::clone(&self.block_device))
            .lock()
            .modify(0, |data_block: &mut DataBlock| {
                data_block.iter_mut().for_each(|p| {    // yifan 2026/6/6: 遍历 data_block 中的每一个字节，并拿到这些字节的可变引用。
                    *p = 0;    // yifan 2026/6/6: 将当前字节置为 0，因此整个数据块最终会被清空；这样回收数据块时不会残留旧内容。
                })
            });
        self.data_bitmap.dealloc(
            &self.block_device,
            // yifan 2026/6/5:
            // dealloc_data 传入的是：
            // 真实磁盘 block_id
            // 所以需要转换：相对编号 = block_id - data_area_start_block
            // 例如：
            // data_area_start_block = 100
            // block_id = 103
            // 相对编号 = 103 - 100 = 3
            // 然后把 data bitmap 中的第 3 个 bit 从 1 改回 0，表示这个数据块重新变为空闲。
            (block_id - self.data_area_start_block) as usize,
        )
    }
}
