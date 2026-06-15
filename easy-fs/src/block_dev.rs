use core::any::Any;    // yifan 2026/6/1: 引入 Any，让实现该 trait 的类型具备运行时类型能力（常用于向下转型/类型擦除场景）。
/// Trait for block devices
/// which reads and writes data in the unit of blocks
pub trait BlockDevice: Send + Sync + Any {    // yifan 2026/6/1: 这是 easy-fs 对底层存储设备的统一接口；上层不关心具体设备类型，只要实现该 trait 即可使用。
    ///Read data form block to buffer
    fn read_block(&self, block_id: usize, buf: &mut [u8]);    // yifan 2026/6/1: 从编号为 block_id 的块读取数据到 buf，属于按块读接口，调用者通常保证 buf 大小等于块大小。
    ///Write data from buffer to block
    fn write_block(&self, block_id: usize, buf: &[u8]);    // yifan 2026/6/1: 将 buf 中的数据写入 block_id 对应的块，属于按块写接口，调用者通常保证 buf 为完整块数据。
}
