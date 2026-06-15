use super::{get_block_cache, BlockDevice, BLOCK_SZ};
use alloc::sync::Arc;
/// A bitmap block
/// yifan 2026/6/2:
/// BitmapBlock 是一个磁盘数据结构，它将位图区域中的一个磁盘块解释为长度为 64 的一个 u64 数组， 每个 u64 打包了一组 64 bits，于是整个数组包含 
/// 64* 64 = 4096 bits，且可以以组为单位进行操作。
type BitmapBlock = [u64; 64];
/// Number of bits in a block
const BLOCK_BITS: usize = BLOCK_SZ * 8;
/// A bitmap
/// yifan 2026/6/2: Bitmap 只是记录：位图区域从哪里开始,位图区域有多长,
/// 它本身不直接保存所有 bit。真正的 bit 数据保存在磁盘的位图区域中。
/// 
/// Bitmap 是内存中的描述对象，记录这组位图从哪个磁盘块开始、有多少个块；每个磁盘块按 BitmapBlock = [u64; 64] 解释；所有这些 BitmapBlock 合起来就是一整套位图。
pub struct Bitmap {
    start_block_id: usize, // yifan 2026/6/1: 该 Bitmap 管理的第一个块在磁盘中的块编号；后续块编号依次递增，直到 start_block_id + blocks - 1。
    blocks: usize, // yifan 2026/6/1: 该 Bitmap 管理的块数量；因此 Bitmap 管理的块编号范围是 [start_block_id, start_block_id + blocks)。
}

/// Decompose bits into (block_pos, bits64_pos, inner_pos)
fn decomposition(mut bit: usize) -> (usize, usize, usize) {    // yifan 2026/6/2: 将“全局 bit 编号”拆成三级位置：(位图块下标, 块内 u64 下标, u64 内位下标)。
    let block_pos = bit / BLOCK_BITS;    // yifan 2026/6/2: 先定位该 bit 落在第几个 bitmap block（按每块 BLOCK_BITS 个 bit 分组）。
    bit %= BLOCK_BITS;    // yifan 2026/6/2: 将 bit 转换为“块内偏移”，便于继续在该块内部定位。
    (block_pos, bit / 64, bit % 64)    // yifan 2026/6/2: 块内再拆为“第几个 u64”和“该 u64 的第几位”；分配/释放时据此精确置位或清位。
}

impl Bitmap {
    /// A new bitmap from start block id and number of blocks
    pub fn new(start_block_id: usize, blocks: usize) -> Self {
        Self {
            start_block_id,
            blocks,
        }
    }
    /// Allocate a new block from a block device
    pub fn alloc(&self, block_device: &Arc<dyn BlockDevice>) -> Option<usize> {    // yifan 2026/6/2: 在位图中分配一个空闲位；成功返回全局位编号，失败返回 None。
        for block_id in 0..self.blocks {    // yifan 2026/6/2: 逐个位图块扫描，查找仍有空位的块。
            let pos = get_block_cache(    // yifan 2026/6/2: 取出当前位图块对应的缓存项。
                block_id + self.start_block_id as usize,
                Arc::clone(block_device),
            )
            .lock()
            .modify(0, |bitmap_block: &mut BitmapBlock| {    // yifan 2026/6/2: 在锁保护下原地修改该位图块数据。
                if let Some((bits64_pos, inner_pos)) = bitmap_block    // yifan 2026/6/2: 若能找到某个含空位的 u64 段，则得到段位置和段内空位位置。
                    .iter()
                    .enumerate()
                    .find(|(_, bits64)| **bits64 != u64::MAX)    // yifan 2026/6/2: 这里是两次解引用：iter() 先产生 &u64，find 闭包参数又是对枚举元素的引用，模式匹配后 bits64 实际类型为 &&u64，所以需要 **bits64 才能得到 u64 与 u64::MAX 比较；逻辑上是在找第一个“不是全1”的 64 位段（仍有空闲 bit 可分配）。
                    .map(|(bits64_pos, bits64)| (bits64_pos, bits64.trailing_ones() as usize))    // yifan 2026/6/2: trailing_ones 给出最低位连续 1 的数量，即该段第一个 0 位的下标 inner_pos。
                {
                    // modify cache
                    bitmap_block[bits64_pos] |= 1u64 << inner_pos;    // yifan 2026/6/2: 将找到的空闲位从 0 置为 1，表示该资源已分配。
                    Some(block_id * BLOCK_BITS + bits64_pos * 64 + inner_pos as usize)    // yifan 2026/6/2: 计算并返回该位的全局编号（块偏移 + 段偏移 + 段内偏移），这里的BLOCK_BITS = BLOCK_SZ * 8 = 512 * 8 = 4096。
                } else {
                    None    // yifan 2026/6/2: 当前位图块无空位，交由外层循环继续扫描下一个块。
                }
            });
            if pos.is_some() {    // yifan 2026/6/2: 一旦当前块分配成功就立即返回，停止继续扫描。
                return pos;    // yifan 2026/6/2: 返回 Some(全局位编号)。
            }
        }
        None    // yifan 2026/6/2: 所有位图块都无空位时返回 None。
    }
    /// Deallocate a block
    pub fn dealloc(&self, block_device: &Arc<dyn BlockDevice>, bit: usize) {    // yifan 2026/6/2: 释放一个已分配资源位；bit 是全局位编号。
        let (block_pos, bits64_pos, inner_pos) = decomposition(bit);    // yifan 2026/6/2: 将全局位编号拆成（位图块下标、块内 u64 下标、u64 内位下标）。
        get_block_cache(block_pos + self.start_block_id, Arc::clone(block_device))    // yifan 2026/6/2: 定位到该位所在的位图块缓存。
            .lock()
            .modify(0, |bitmap_block: &mut BitmapBlock| {    // yifan 2026/6/2: 在锁保护下对位图块做原地修改。
                assert!(bitmap_block[bits64_pos] & (1u64 << inner_pos) > 0);    // yifan 2026/6/2: 释放前先断言该位当前为 1，避免重复释放或状态错误。
                bitmap_block[bits64_pos] -= 1u64 << inner_pos;    // yifan 2026/6/2: 将对应位从 1 清为 0，表示该资源已释放。
            });
    }
    /// Get the max number of allocatable blocks
    pub fn maximum(&self) -> usize {
        self.blocks * BLOCK_BITS
    }
}
