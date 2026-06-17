use super::{get_block_cache, BlockDevice, BLOCK_SZ};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt::{Debug, Formatter, Result};

/// Magic number for sanity check
const EFS_MAGIC: u32 = 0x3b800001;
/// The max number of direct inodes
const INODE_DIRECT_COUNT: usize = 28;
/// The max length of inode name
const NAME_LENGTH_LIMIT: usize = 27;
/// The max number of indirect1 inodes
const INODE_INDIRECT1_COUNT: usize = BLOCK_SZ / 4; // yifan 2026/6/2: 一个间接块能存放 BLOCK_SZ / 4 个 u32 块编号，因为一个块是 512 字节，一个 u32 是 4 字节，所以一个间接索引块能存放 128 个块编号。
/// The max number of indirect2 inodes
const INODE_INDIRECT2_COUNT: usize = INODE_INDIRECT1_COUNT * INODE_INDIRECT1_COUNT;
/// The upper bound of direct inode index
const DIRECT_BOUND: usize = INODE_DIRECT_COUNT;
/// The upper bound of indirect1 inode index
const INDIRECT1_BOUND: usize = DIRECT_BOUND + INODE_INDIRECT1_COUNT; // yifan 2026/6/4: INDIRECT1_BOUND = 28 + 128 = 156
/// The upper bound of indirect2 inode indexs
#[allow(unused)]
const INDIRECT2_BOUND: usize = INDIRECT1_BOUND + INODE_INDIRECT2_COUNT;
/// Super block of a filesystem
#[repr(C)]
pub struct SuperBlock {    // yifan 2026/6/1: SuperBlock 的逻辑意义是记录整个文件系统在磁盘上的布局目录，挂载后据此定位各区域并执行分配/读取。
    magic: u32,    // yifan 2026/6/1: 文件系统魔数签名，用于校验该块设备是否为 easy-fs 格式，避免误解析。
    pub total_blocks: u32,    // yifan 2026/6/1: 文件系统总块数，给出磁盘空间边界；这里的“块数”是磁盘块数量，不是内存占用数量。
    pub inode_bitmap_blocks: u32,    // yifan 2026/6/1: inode 位图区占用的磁盘块数，用于记录 inode 是否已分配。
    pub inode_area_blocks: u32,    // yifan 2026/6/1: inode 区占用的磁盘块数，实际存放 inode 数据结构。
    pub data_bitmap_blocks: u32,    // yifan 2026/6/1: 数据块位图区占用的磁盘块数，用于记录数据块是否已分配。
    pub data_area_blocks: u32,    // yifan 2026/6/1: 数据区占用的磁盘块数，实际存放文件内容数据。
}

impl Debug for SuperBlock {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result {
        f.debug_struct("SuperBlock")
            .field("total_blocks", &self.total_blocks)
            .field("inode_bitmap_blocks", &self.inode_bitmap_blocks)
            .field("inode_area_blocks", &self.inode_area_blocks)
            .field("data_bitmap_blocks", &self.data_bitmap_blocks)
            .field("data_area_blocks", &self.data_area_blocks)
            .finish()
    }
}

impl SuperBlock {
    /// Initialize a super block
    pub fn initialize(
        &mut self,
        total_blocks: u32,
        inode_bitmap_blocks: u32,
        inode_area_blocks: u32,
        data_bitmap_blocks: u32,
        data_area_blocks: u32,
    ) {
        *self = Self {
            magic: EFS_MAGIC,
            total_blocks,
            inode_bitmap_blocks,
            inode_area_blocks,
            data_bitmap_blocks,
            data_area_blocks,
        }
    }
    /// Check if a super block is valid using efs magic
    pub fn is_valid(&self) -> bool {
        self.magic == EFS_MAGIC
    }
}
/// Type of a disk inode
#[derive(PartialEq)]
pub enum DiskInodeType {
    File,
    Directory,
}

/// A indirect block
type IndirectBlock = [u32; BLOCK_SZ / 4]; // yifan 2026/6/2: IndirectBlock也就是一个索引块，里面每个 u32 都保存一个块编号，一共 128 个。类似direct: [u32; INODE_DIRECT_COUNT]。
/// A data block
type DataBlock = [u8; BLOCK_SZ];
/// A disk inode
/// yifan 2026/6/2:
/// 这里的 DiskInode 并不是直接的磁盘硬件对象，而是 操作系统文件系统里表示磁盘上 inode 的内存镜像。
/// 换句话说：
/// 在磁盘上，每个 inode 都是以固定的字节布局存储在 inode 区域的块 中。
/// 当文件系统需要访问 inode 时，会通过 块缓存层 (BlockCache) 读取对应磁盘块到内存。
/// 内存中的数据会被解释为 DiskInode 类型，形成 inode 的 内存镜像。
/// 对这个 DiskInode 的读写，最终会通过 块缓存层 和 write_block 回写到磁盘上。
/// 
/// 实际的DiskInode 是保存在磁盘 inode 区域中的数据结构。每个文件或目录对应一个 DiskInode。
/// 它保存两类信息：
/// 1. 文件/目录的元数据
/// 2. 文件/目录内容所在数据块的索引
#[repr(C)]
pub struct DiskInode {
    pub size: u32, // yifan 2026/6/2: size 表示文件或目录内容的字节数。 
    // yifan 2026/6/2:direct 是直接索引数组。它里面最多保存 28 个数据块编号。因为 easy-fs 一个块是 512 字节，所以直接索引最多能找到：28 * 512 = 14336 字节 = 14 KiB,也就是说，小文件只需要 direct 就够了。
    // 例如：direct[0] = 100,direct[1] = 101, direct[2] = 102, 表示文件内容在 data block 100、101、102 中。
    pub direct: [u32; INODE_DIRECT_COUNT],  
    // 当文件超过 14 KiB，direct 放不下所有数据块编号，就需要 indirect1。
    // indirect1 指向一个一级索引块。注意：这个一级索引块也存放在 data area 中，但它保存的不是文件内容，而是一组数据块编号。
    // 一个块大小是 512 字节，一个 u32 是 4 字节，所以一个一级索引块能保存：512 / 4 = 128 个 u32 块编号, 每个块编号指向一个 512 字节的数据块，所以一级间接索引最多能索引：128 * 512 = 65536 字节 = 64 KiB
    // 所以：direct 支持 14 KiB, indirect1 额外支持 64 KiB, 合计 78 KiB。
    pub indirect1: u32,
    // 如果文件超过 78 KiB，就需要 indirect2。
    // indirect2 指向一个二级索引块。
    // 二级索引块中保存的不是文件数据块编号，而是一级索引块编号。
    // 结构类似：
    // indirect2
    // ↓
    // 二级索引块
    // ├── 一级索引块 A -> 128 个数据块
    // ├── 一级索引块 B -> 128 个数据块
    // ├── 一级索引块 C -> 128 个数据块
    // └── ...
    // 一个二级索引块可以保存 128 个一级索引块编号。 每个一级索引块可以索引 64 KiB 文件数据。 所以二级间接索引最多支持：128 * 64 KiB = 8192 KiB = 8 MiB
    pub indirect2: u32,
    type_: DiskInodeType, // yifan 2026/6/2: type_ 表示该 inode 是文件还是目录.
    nlink: u32, // yifan 2026/6/16: nlink 表示硬链接数量，初始为1，每当有新的硬链接指向这个 inode 时 nlink 就加 1；

    // yifan 2026/6/2: indirect1 和 indirect2的区别：
    // 文件增长时先用满 direct，再用 DiskInode.indirect1 指向的那个一级索引块，再用 DiskInode.indirect2 指向的二级索引块。DiskInode.indirect1 是一个单独的一级索引块指针；而 indirect2 指向的二级索引块中保存的是一批额外的一级索引块指针。它们不是重复，而是为了支持更大的文件。
    // 一个直观的图：
    // DiskInode
    // ├── direct[0]
    // ├── direct[1]
    // ├── ...
    // ├── direct[27]
    // │
    // ├── indirect1
    // │     ↓
    // │   一级索引块 A
    // │   ├── data block 28
    // │   ├── data block 29
    // │   └── ...
    // │
    // └── indirect2
    //       ↓
    //     二级索引块 B
    //     ├── 一级索引块 C
    //     │   ├── data block 156
    //     │   ├── data block 157
    //     │   └── ...
    //     ├── 一级索引块 D
    //     │   ├── data block 284
    //     │   ├── data block 285
    //     │   └── ...
    //     └── ...
}

impl DiskInode {
    /// Initialize a disk inode, as well as all direct inodes under it
    /// indirect1 and indirect2 block are allocated only when they are needed
    pub fn initialize(&mut self, type_: DiskInodeType) {
        self.size = 0;
        self.direct.iter_mut().for_each(|v| *v = 0);
        self.indirect1 = 0;
        self.indirect2 = 0;
        self.type_ = type_;
        self.nlink = 1; // yifan 2026/6/16: 初始化 nlink 为 1，因为新创建的文件或目录至少有一个链接（它自己）。
    }
    /// Whether this inode is a directory
    pub fn is_dir(&self) -> bool {
        self.type_ == DiskInodeType::Directory
    }
    /// Whether this inode is a file
    #[allow(unused)]
    pub fn is_file(&self) -> bool {
        self.type_ == DiskInodeType::File
    }
    /// Return block number correspond to size.
    pub fn data_blocks(&self) -> u32 {
        Self::_data_blocks(self.size)
    }
    fn _data_blocks(size: u32) -> u32 {    // yifan 2026/6/4: 这里定义“向上取整除法”，把字节数换算成需要占用的磁盘块数。
        (size + BLOCK_SZ as u32 - 1) / BLOCK_SZ as u32    // yifan 2026/6/4: 通过(size + BLOCK_SZ - 1) / BLOCK_SZ实现ceil(size / BLOCK_SZ)，保证有剩余字节时也会多算一个块。公式 (a + b - 1) / b 是整数除法里常见的“ceil(a / b)”写法
    }
    /// Return number of blocks needed include indirect1/2.
    pub fn total_blocks(size: u32) -> u32 {    // yifan 2026/6/4: 计算一个 inode 保存该 size 文件时，一共需要占用多少个磁盘块。
        let data_blocks = Self::_data_blocks(size) as usize;    // yifan 2026/6/4: 先把文件大小换算成“数据块”数量，也就是文件内容本身需要的块数。
        let mut total = data_blocks as usize;    // yifan 2026/6/4: 先把总块数初始化为数据块数，后面如果需要索引块再额外累加。
        // indirect1
        if data_blocks > INODE_DIRECT_COUNT {    // yifan 2026/6/4: 如果数据块数量超过直接索引容量，就必须启用一级间接索引块。
            total += 1;    // yifan 2026/6/4: 这里额外加 1，是把一级间接索引块本身算进去。
        }
        // indirect2
        if data_blocks > INDIRECT1_BOUND {    // yifan 2026/6/4: 如果数据块数量连一级间接索引也放不下，就需要使用二级间接索引。
            total += 1;    // yifan 2026/6/4: 这里额外加 1，是把二级间接索引的根索引块算进去。
            // sub indirect1
            total +=    // yifan 2026/6/4: 这里统计二级间接索引下还需要多少个“子一级间接索引块”。
                (data_blocks - INDIRECT1_BOUND + INODE_INDIRECT1_COUNT - 1) / INODE_INDIRECT1_COUNT;    // yifan 2026/6/4: 使用向上取整计算剩余数据块需要多少个子索引块，每个子索引块最多管理 INODE_INDIRECT1_COUNT 个数据块。
        }
        total as u32    // yifan 2026/6/4: 返回总块数，类型从 usize 转回 u32。
    }
    /// Get the number of data blocks that have to be allocated given the new size of data
    pub fn blocks_num_needed(&self, new_size: u32) -> u32 {
        assert!(new_size >= self.size);
        Self::total_blocks(new_size) - Self::total_blocks(self.size)
    }
    /// Get id of block given inner id
    /// 返回值：这个数据块在磁盘 data area 中的真实 block 编号
    pub fn get_block_id(&self, inner_id: u32, block_device: &Arc<dyn BlockDevice>) -> u32 {    // yifan 2026/6/2: inner_id 是“文件内部的数据块序号”，不是磁盘的绝对 block ID；它表示该文件内容按 BLOCK_SZ 切块后当前要访问的是第几块，通常由 read_at/write_at 根据文件内偏移 offset / BLOCK_SZ 计算得到。
        let inner_id = inner_id as usize;    // yifan 2026/6/2: 这里把文件内部的数据块序号转换成 usize，方便后续作为数组下标去访问 direct 或间接索引表。
        if inner_id < INODE_DIRECT_COUNT {    // yifan 2026/6/2: 如果 inner_id 落在直接索引范围内，就说明这块数据可以直接从 direct 数组中找到对应的磁盘块号。
            self.direct[inner_id]    // yifan 2026/6/2: direct[inner_id] 保存的是该文件第 inner_id 个数据块在磁盘上的真实 block ID。
        } else if inner_id < INDIRECT1_BOUND {    // yifan 2026/6/2: 超过 direct 范围但仍未超过一级间接范围时，需要先从 indirect1 指向的索引块里查找真实 block ID。
            // yifan 2026/6/2:
            // 1. 获取索引块所在磁盘块的缓存
            // 2. 锁住这个 BlockCache
            // 3. 从 offset = 0 开始，把整个块解释为 IndirectBlock
            // 4. 从 IndirectBlock 中读取某个 u32 块编号
            get_block_cache(self.indirect1 as usize, Arc::clone(block_device)) // yifan 2026/6/2: get_block_cache返回的是Arc<Mutex<BlockCache>>，所以后面需要read将其读取为IndirectBlock。
                .lock()
                .read(0, |indirect_block: &IndirectBlock| {// yifan 2026/6/2: 此时indirect1指向的块存储了inner_id对应的数据块编号。
                    indirect_block[inner_id - INODE_DIRECT_COUNT]    // yifan 2026/6/2: 这里把文件内部序号减去 direct 数量，得到在一级间接索引块中的下标，再取出真实磁盘块号。由于indirect_block也是512字节，而BlockCache的cache也是512字节，所以offset=0表示从块的起始位置开始读取整个块数据，解释为IndirectBlock类型。
                })
        } else {    // yifan 2026/6/2: 再往后就进入二级间接索引范围，需要先找到二级索引块，再定位到对应的一级索引块，最后取出真实数据块号。
            let last = inner_id - INDIRECT1_BOUND;    // yifan 2026/6/2: last 是去掉 direct 和一级间接覆盖范围后的“剩余块序号”，用于在二级间接结构里继续分解索引。
            let indirect1 = get_block_cache(self.indirect2 as usize, Arc::clone(block_device))
                .lock()
                .read(0, |indirect2: &IndirectBlock| {
                    // yifan 2026/6/3: 这里的除法不是为了算“标签位置”，而是把 last 这个“还剩下第几个数据块”的编号按 128 个一组分组，因为一个一级间接块只能装 128 个数据块地址，所以商就是该数据块属于第几个一级间接块。
                    indirect2[last / INODE_INDIRECT1_COUNT]    // yifan 2026/6/2: 先在二级间接块中按“第几个一级间接块”找到对应的一级间接块号。
                });
            get_block_cache(indirect1 as usize, Arc::clone(block_device))
                .lock()
                .read(0, |indirect1: &IndirectBlock| {
                    // yifan 2026/6/3: 这里的取余不是直接“算出 block id”，而是得到该数据块在当前一级间接块中的下标，因为一级间接块里存的是 128 个 u32 条目，每个条目保存一个真实的数据块号，所以余数对应的是“第几个条目”，条目里的值才是最终的 block id。
                    indirect1[last % INODE_INDIRECT1_COUNT]    // yifan 2026/6/2: 再在该一级间接块中按余数定位到最终的数据块号，这个返回值才是磁盘上的真实 block ID。
                })
        }
    }
    /// Inncrease the size of current disk inode
    /// yifan 2026/6/4: 这里的 inode 严格指 DiskInode，也就是磁盘上的 inode 结构；increase_size 的职责是给 DiskInode 扩容并建立“文件偏移 -> 磁盘块”的映射，它只负责把外部已经分配好的块号写进 direct / indirect 索引里，不负责写入这些块里的文件数据，也不关心块内此刻是否已有内容。
    pub fn increase_size(
        &mut self,
        new_size: u32,
        new_blocks: Vec<u32>,    // yifan 2026/6/4: 这里的 new_blocks 是外部已经分配好的磁盘块号列表，increase_size 只负责把它们写入 inode 结构，不负责块分配本身；这些块在语义上是“已分配但尚未写入 inode”的块号，但块内容本身不一定由此函数初始化，通常只是保留给后续填充文件内容使用。
        block_device: &Arc<dyn BlockDevice>,
    ) {
        let mut current_blocks = self.data_blocks();
        self.size = new_size;
        let mut total_blocks = self.data_blocks();
        let mut new_blocks = new_blocks.into_iter();
        // fill direct
        while current_blocks < total_blocks.min(INODE_DIRECT_COUNT as u32) {
            self.direct[current_blocks as usize] = new_blocks.next().unwrap();
            current_blocks += 1;
        }
        // alloc indirect1
        if total_blocks > INODE_DIRECT_COUNT as u32 {
            if current_blocks == INODE_DIRECT_COUNT as u32 {
                self.indirect1 = new_blocks.next().unwrap();
            }
            current_blocks -= INODE_DIRECT_COUNT as u32;
            total_blocks -= INODE_DIRECT_COUNT as u32;
        } else {
            return;
        }
        // fill indirect1
        get_block_cache(self.indirect1 as usize, Arc::clone(block_device))
            .lock()
            .modify(0, |indirect1: &mut IndirectBlock| {
                while current_blocks < total_blocks.min(INODE_INDIRECT1_COUNT as u32) {
                    indirect1[current_blocks as usize] = new_blocks.next().unwrap();
                    current_blocks += 1;
                }
            });
        // alloc indirect2
        if total_blocks > INODE_INDIRECT1_COUNT as u32 {
            if current_blocks == INODE_INDIRECT1_COUNT as u32 {
                self.indirect2 = new_blocks.next().unwrap();
            }
            current_blocks -= INODE_INDIRECT1_COUNT as u32;
            total_blocks -= INODE_INDIRECT1_COUNT as u32;
        } else {
            return;
        }
        // fill indirect2 from (a0, b0) -> (a1, b1)
        let mut a0 = current_blocks as usize / INODE_INDIRECT1_COUNT;
        let mut b0 = current_blocks as usize % INODE_INDIRECT1_COUNT;
        let a1 = total_blocks as usize / INODE_INDIRECT1_COUNT;
        let b1 = total_blocks as usize % INODE_INDIRECT1_COUNT;
        // alloc low-level indirect1
        get_block_cache(self.indirect2 as usize, Arc::clone(block_device))
            .lock()
            .modify(0, |indirect2: &mut IndirectBlock| {
                while (a0 < a1) || (a0 == a1 && b0 < b1) {
                    if b0 == 0 {
                        indirect2[a0] = new_blocks.next().unwrap();
                    }
                    // fill current
                    get_block_cache(indirect2[a0] as usize, Arc::clone(block_device))
                        .lock()
                        .modify(0, |indirect1: &mut IndirectBlock| {
                            indirect1[b0] = new_blocks.next().unwrap();
                        });
                    // move to next
                    b0 += 1;
                    if b0 == INODE_INDIRECT1_COUNT {
                        b0 = 0;
                        a0 += 1;
                    }
                }
            });
    }

    /// Clear size to zero and return blocks that should be deallocated.
    /// We will clear the block contents to zero later.
    pub fn clear_size(&mut self, block_device: &Arc<dyn BlockDevice>) -> Vec<u32> {
        let mut v: Vec<u32> = Vec::new();
        let mut data_blocks = self.data_blocks() as usize;
        self.size = 0;
        let mut current_blocks = 0usize;
        // direct
        while current_blocks < data_blocks.min(INODE_DIRECT_COUNT) {
            v.push(self.direct[current_blocks]);
            self.direct[current_blocks] = 0;
            current_blocks += 1;
        }
        // indirect1 block
        if data_blocks > INODE_DIRECT_COUNT {
            v.push(self.indirect1);
            data_blocks -= INODE_DIRECT_COUNT;
            current_blocks = 0;
        } else {
            return v;
        }
        // indirect1
        get_block_cache(self.indirect1 as usize, Arc::clone(block_device))
            .lock()
            .modify(0, |indirect1: &mut IndirectBlock| {
                while current_blocks < data_blocks.min(INODE_INDIRECT1_COUNT) {
                    v.push(indirect1[current_blocks]);
                    //indirect1[current_blocks] = 0;
                    current_blocks += 1;
                }
            });
        self.indirect1 = 0;
        // indirect2 block
        if data_blocks > INODE_INDIRECT1_COUNT {
            v.push(self.indirect2);
            data_blocks -= INODE_INDIRECT1_COUNT;
        } else {
            return v;
        }
        // indirect2
        assert!(data_blocks <= INODE_INDIRECT2_COUNT);
        let a1 = data_blocks / INODE_INDIRECT1_COUNT;
        let b1 = data_blocks % INODE_INDIRECT1_COUNT;
        get_block_cache(self.indirect2 as usize, Arc::clone(block_device))
            .lock()
            .modify(0, |indirect2: &mut IndirectBlock| {
                // full indirect1 blocks
                for entry in indirect2.iter_mut().take(a1) {
                    v.push(*entry);
                    get_block_cache(*entry as usize, Arc::clone(block_device))
                        .lock()
                        .modify(0, |indirect1: &mut IndirectBlock| {
                            for entry in indirect1.iter() {
                                v.push(*entry);
                            }
                        });
                }
                // last indirect1 block
                if b1 > 0 {
                    v.push(indirect2[a1]);
                    get_block_cache(indirect2[a1] as usize, Arc::clone(block_device))
                        .lock()
                        .modify(0, |indirect1: &mut IndirectBlock| {
                            for entry in indirect1.iter().take(b1) {
                                v.push(*entry);
                            }
                        });
                    //indirect2[a1] = 0;
                }
            });
        self.indirect2 = 0;
        v
    }
    /// Read data from current disk inode
    pub fn read_at(
        &self,
        offset: usize,
        buf: &mut [u8],
        block_device: &Arc<dyn BlockDevice>,
    ) -> usize {
        let mut start = offset; // yifan 2026/6/4: start 是本次读取的起始位置，单位是字节。它是文件内的偏移量，表示从文件的哪个字节开始读。
        let end = (offset + buf.len()).min(self.size as usize); // yifan 2026/6/4: end 是本次读取的结束位置，不能超过文件实际大小 self.size。
        if start >= end {
            return 0; // yifan 2026/6/4: 如果 offset 已经超过文件大小，就没有任何内容可以读。
        }
        let mut start_block = start / BLOCK_SZ; // yifan 2026/6/4: 当前要读取的是文件内部的第几个数据块。注意，它不是磁盘块号，而是文件内部块编号。
        let mut read_size = 0usize; // yifan 2026/6/4: read_size 记录目前已经成功读取了多少字节，初始为0，每次循环处理一个块时会增加 block_read_size，直到覆盖整个 buf 或者读到文件末尾。
        loop { // yifan 2026/6/4: 文件要读取的区间可能跨多个数据块。循环每次处理一个数据块。
            // calculate end of current block
            let mut end_current_block = (start / BLOCK_SZ + 1) * BLOCK_SZ; // yifan 2026/6/4: end_current_block 是当前块的结束位置，通常是 start 的下一个块边界。
            end_current_block = end_current_block.min(end); // yifan 2026/6/4: end_current_block 表示当前这个块的结束位置，通常是 start 的下一个块边界，但如果 end 在当前块内，就以 end 作为结束位置。
            // read and update read size
            let block_read_size = end_current_block - start; // yifan 2026/6/4: 表示当前这个块中要读取多少字节
            let dst = &mut buf[read_size..read_size + block_read_size]; // yifan 2026/6/4:read_size 记录目前已经读了多少字节。所以 dst 表示：当前这一轮读到的数据，应该写入 buf 的哪个位置。
            get_block_cache(
                self.get_block_id(start_block as u32, block_device) as usize, // yifan 2026/6/4: start_block 是文件内部块号。但是磁盘上真实块号可能不是连续的。所以要通过 inode 的索引结构找到真实块号：
                Arc::clone(block_device),
            )
            .lock() // yifan 2026/6/4: 这里的 get_block_cache 返回一个 Arc<Mutex<BlockCache>>，所以需要 lock() 来获取对块缓存的独占访问权，才能安全地读取数据。
            .read(0, |data_block: &DataBlock| { // yifan 2026/6/4: 从 offset 0 开始，把整个块解释成 DataBlock = [u8; 512]；
                let src = &data_block[start % BLOCK_SZ..start % BLOCK_SZ + block_read_size]; // yifan 2026/6/4: 从 data_block 中取出当前需要的那一段；
                dst.copy_from_slice(src); // yifan 2026/6/4: 复制到 buf 对应位置。
            });
            read_size += block_read_size;
            // move to next block
            if end_current_block == end {
                break;
            }
            start_block += 1;
            start = end_current_block;
        }
        read_size
    }
    /// Write data into current disk inode
    /// size must be adjusted properly beforehand
    pub fn write_at(    // yifan 2026/6/5: 这是 DiskInode 的写入函数，作用是把 buf 的内容按 offset 写入该 inode 对应的文件数据区，并且支持跨多个磁盘块连续写入。
        &mut self,
        offset: usize,
        buf: &[u8],
        block_device: &Arc<dyn BlockDevice>,
    ) -> usize {
        let mut start = offset;    // yifan 2026/6/5: start 表示当前写入的字节位置，初始值就是传入的 offset。
        let end = (offset + buf.len()).min(self.size as usize);    // yifan 2026/6/5: end 是本次写入的结束位置，但不会超过当前 inode 的 size，所以这里只写文件已有空间内的内容。
        assert!(start <= end);    // yifan 2026/6/5: 确保写入区间合法。
        let mut start_block = start / BLOCK_SZ;    // yifan 2026/6/5: 把字节偏移转换成文件内部的块号，定位当前应该写到第几个数据块。
        let mut write_size = 0usize;    // yifan 2026/6/5: 记录已经从 buf 中写出了多少字节。
        loop {    // yifan 2026/6/5: 使用循环是因为一次写入可能跨越多个磁盘块，需要逐块处理。
            // calculate end of current block
            let mut end_current_block = (start / BLOCK_SZ + 1) * BLOCK_SZ;    // yifan 2026/6/5: 当前块的结束边界，也就是下一个块的起始位置。
            end_current_block = end_current_block.min(end);    // yifan 2026/6/5: 如果本次写入在当前块内结束，就把结束位置限制为 end。
            // write and update write size
            let block_write_size = end_current_block - start;    // yifan 2026/6/5: 这一轮实际需要写入当前块的字节数。
            get_block_cache(
                self.get_block_id(start_block as u32, block_device) as usize,    // yifan 2026/6/5: 通过文件内部块号找到真实的磁盘块号，再去块缓存中定位该块。
                Arc::clone(block_device),
            )
            .lock()
            .modify(0, |data_block: &mut DataBlock| {    // yifan 2026/6/5: 这里把块缓存中的 512 字节块按 DataBlock 视图打开，然后在内存中修改它。
                let src = &buf[write_size..write_size + block_write_size];    // yifan 2026/6/5: src 是本轮从 buf 中取出的待写入片段。
                let dst = &mut data_block[start % BLOCK_SZ..start % BLOCK_SZ + block_write_size];    // yifan 2026/6/5: dst 是当前磁盘块内对应的写入位置，偏移从块内位置 start % BLOCK_SZ 开始。
                dst.copy_from_slice(src);    // yifan 2026/6/5: 把 src 复制到 dst，完成当前块内这一段内容的写入。
            });
            write_size += block_write_size;    // yifan 2026/6/5: 累加已经写入的字节数。
            // move to next block
            if end_current_block == end {    // yifan 2026/6/5: 如果已经写到最终位置，就结束循环。
                break;
            }
            start_block += 1;    // yifan 2026/6/5: 切换到下一个文件数据块。
            start = end_current_block;    // yifan 2026/6/5: 更新当前写入起点到下一个块边界。
        }
        write_size    // yifan 2026/6/5: 返回本次实际写入的总字节数。
    }

    /// yifan 2026/6/16: 修改nlink字段
    pub fn set_nlink(&mut self, nlink: u32) {
        self.nlink = nlink;
    }

    /// yifan 2026/6/16: 获取nlink字段
    pub fn nlink(&self) -> u32 {
        self.nlink
    }   

    /// yifan 2026/6/18: 减少size
    pub fn decrease_size(&mut self, new_size: u32) {
        assert!(new_size <= self.size);
        self.size = new_size;
    }
    
}
/// A directory entry
/// yifan 2026/6/6: DirEntry 不是“目录本身”，而是目录中的一条记录，也就是“目录项”。
/// 目录文件的内容由很多个 DirEntry 组成。
#[repr(C)]
pub struct DirEntry {
    name: [u8; NAME_LENGTH_LIMIT + 1], // yifan 2026/6/5: name 是目录项的名字，使用固定长度的字节数组存储，长度限制为 NAME_LENGTH_LIMIT + 1，其中最后一个字节用于存储字符串结束符 '\0'。
    inode_id: u32, // yifan 2026/6/5: inode_id 是该目录项对应的 inode 编号，也就是磁盘上 inode 区域中的索引号。通过这个 inode_id，文件系统可以找到对应的 DiskInode，从而访问该目录项指向的文件或子目录。
}
/// Size of a directory entry
pub const DIRENT_SZ: usize = 32;

impl DirEntry {
    /// Create an empty directory entry
    pub fn empty() -> Self {
        Self {
            name: [0u8; NAME_LENGTH_LIMIT + 1],
            inode_id: 0,
        }
    }
    /// Crate a directory entry from name and inode number
    pub fn new(name: &str, inode_id: u32) -> Self {
        let mut bytes = [0u8; NAME_LENGTH_LIMIT + 1];    // yifan 2026/6/5: 这里创建一个固定长度的字节数组来存放目录项名字，先全部初始化为 0，方便后面用 0 作为字符串结束标记。
        bytes[..name.len()].copy_from_slice(name.as_bytes());    // yifan 2026/6/5: 这里把 name 的 UTF-8 字节内容复制到 bytes 的前 name.len() 个位置；[..name.len()] 是一个可变切片范围，copy_from_slice 会按字节逐个拷贝，剩余位置保持 0。
        Self {
            name: bytes,
            inode_id,
        }
    }
    /// Serialize into bytes
    pub fn as_bytes(&self) -> &[u8] {    // yifan 2026/6/5: 这是一个普通函数定义，返回类型 `&[u8]` 表示它要返回“字节切片引用”。
        unsafe { core::slice::from_raw_parts(self as *const _ as usize as *const u8, DIRENT_SZ) }    // yifan 2026/6/5: 这一行的语法核心是 `unsafe { ... }` 和函数调用 `core::slice::from_raw_parts(ptr, len)`；其中 `self as *const _ as usize as *const u8` 是连续的 `as` 类型转换，先把 `&DirEntry` 转成原始指针，再转成整数地址，再转回 `*const u8`，最后作为参数传给 `from_raw_parts` 来构造 `&[u8]`。
    }
    /// Serialize into mutable bytes
    pub fn as_bytes_mut(&mut self) -> &mut [u8] {
        unsafe { core::slice::from_raw_parts_mut(self as *mut _ as usize as *mut u8, DIRENT_SZ) }
    }
    /// Get name of the entry
    pub fn name(&self) -> &str {
        // yifan 2026/6/5: `(0usize..)` 在 Rust 里表示一个从 0 开始、一直向后增长的开区间范围，类型通常是 `std::ops::RangeFrom<usize>`；这里把它当迭代器来用，让 `find(...)` 依次尝试 0, 1, 2, 3, ...，直到找到第一个满足条件的下标就停止。
        let len = (0usize..).find(|i| self.name[*i] == 0).unwrap();    // yifan 2026/6/5: 这里用 `(0usize..)` 创建一个无限范围迭代器，再用 `find(...)` 找到 `self.name` 中第一个值为 0 的下标；`unwrap()` 表示这个下标必须存在，否则直接 panic。
        core::str::from_utf8(&self.name[..len]).unwrap()    // yifan 2026/6/5: 这里把 `self.name[..len]` 这个字节切片传给 `from_utf8`，把它按 UTF-8 规则解释成 `&str`；最后的 `unwrap()` 表示这个字节序列必须是合法 UTF-8。
    }
    /// Get inode number of the entry
    pub fn inode_id(&self) -> u32 {
        self.inode_id
    }
    
}
