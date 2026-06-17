use super::{
    block_cache_sync_all, get_block_cache, BlockDevice, DirEntry, DiskInode, DiskInodeType,
    EasyFileSystem, DIRENT_SZ,
};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::{Mutex, MutexGuard};
/// Virtual filesystem layer over easy-fs
/// Inode 是 easy-fs 暴露给上层使用者的“内存中的文件/目录操作对象”，它封装了磁盘上的 DiskInode，让上层不必直接关心磁盘块位置和块缓存细节。
pub struct Inode {
    block_id: usize, // yifan 2026/6/6: 表示这个 Inode 对应的 DiskInode 存在哪个磁盘块中。
    block_offset: usize, // yifan 2026/6/6: 表示这个 DiskInode 在该磁盘块内的偏移量。因为一个磁盘块可以保存多个 DiskInode。
    fs: Arc<Mutex<EasyFileSystem>>, // yifan 2026/6/6:  fs 是指向 EasyFileSystem 的一个指针，因为对 Inode 的种种操作实际上都是要通过底层的文件系统来完成。
    block_device: Arc<dyn BlockDevice>, // yifan 2026/6/6:表示底层块设备。因为 Inode 最终要读写磁盘块，所以需要能访问块设备。
    inode_id: u32, // yifan 2026/6/16: inode_id 是这个 Inode 对应的磁盘 inode 编号，后续在实现 get_stat 时需要用它来填充 Stat 结构体中的 ino 字段。
}

impl Inode {
    /// Create a vfs inode
    pub fn new(
        block_id: u32,
        block_offset: usize,
        fs: Arc<Mutex<EasyFileSystem>>,
        block_device: Arc<dyn BlockDevice>,
        inode_id: u32, // yifan 2026/6/16
    ) -> Self {
        Self {
            block_id: block_id as usize,
            block_offset,
            fs,
            block_device,
            inode_id, // yifan 2026/6/16
        }
    }
    /// Call a function over a disk inode to read it
    fn read_disk_inode<V>(&self, f: impl FnOnce(&DiskInode) -> V) -> V {
        get_block_cache(self.block_id, Arc::clone(&self.block_device))
            .lock()
            .read(self.block_offset, f)
    }
    /// Call a function over a disk inode to modify it
    fn modify_disk_inode<V>(&self, f: impl FnOnce(&mut DiskInode) -> V) -> V {
        get_block_cache(self.block_id, Arc::clone(&self.block_device))
            .lock()
            .modify(self.block_offset, f)
    }
    /// Find inode under a disk inode by name
    /// yifan 2026/6/6: 在目录文件的内容中查找某个文件名对应的 inode 编号。
    /// 1. 创建一个空 DirEntry
    /// 2. 从目录文件中依次读取第 i 个目录项
    /// 3. 把读取到的数据放入 dirent
    /// 4. 比较 dirent.name() 是否等于要查找的 name
    /// 5. 如果相等，返回 dirent.inode_number()
    /// 6. 如果全部遍历完都找不到，返回 None
    fn find_inode_id(&self, name: &str, disk_inode: &DiskInode) -> Option<u32> {
        // assert it is a directory
        assert!(disk_inode.is_dir()); // yifan 2026/6/6: 只有目录文件才有目录项，才能通过文件名查找 inode_id，所以这里断言 disk_inode 必须是一个目录。
        // yifan 2026/6/6: disk_inode.size 表示目录文件的总字节数。目录文件的内容是由一个个 DirEntry 组成的，每个 DirEntry 占用 DIRENT_SZ=32 字节。因此，目录文件的大小除以 DIRENT_SZ 就得到目录项的数量，也就是文件的数量。
        // 一个block size是512字节，那么一个block里可以存放16个dirent。如果超过16个dirent，就需要DiskInode::increase_size增加目录文件大小。
        let file_count = (disk_inode.size as usize) / DIRENT_SZ; 
        let mut dirent = DirEntry::empty();    // yifan 2026/6/7: 先创建一个空的 DirEntry 作为临时缓冲区，用来接收从目录内容中读出的一条目录项。
        for i in 0..file_count {    // yifan 2026/6/7: 遍历目录中的每一条目录项，i 表示当前是第几个目录项。
            assert_eq!(
                disk_inode.read_at(DIRENT_SZ * i, dirent.as_bytes_mut(), &self.block_device,),    // yifan 2026/6/7: 从目录 inode 的内容中读取第 i 个目录项；因为目录本质上也是文件，内容是一串连续排列的 DirEntry，所以第 i 项的起始偏移就是 DIRENT_SZ * i，而 dirent.as_bytes_mut() 提供了一个可写字节缓冲区来接收这 32 字节数据。
                DIRENT_SZ,    // yifan 2026/6/7: 断言这次读取的字节数必须正好等于一个完整目录项的大小，否则说明没有正确读出一条完整的 DirEntry。
            );
            if dirent.name() == name {
                return Some(dirent.inode_id() as u32);
            }
        }
        None
    }
    /// Find inode under current inode by name
    /// yifan 2026/6/6: 在 easy-fs 当前设计中，这个方法主要由根目录 Inode 调用。
    /// 1. 获取 EasyFileSystem 的锁
    /// 2. 读取当前目录对应的 DiskInode
    /// 3. 调用 find_inode_id 查找文件名对应的 inode_id
    /// 4. 如果找到 inode_id，就计算它在磁盘上的位置
    /// 5. 根据 block_id 和 block_offset 构造新的 Inode
    /// 6. 返回 Arc<Inode>
    pub fn find(&self, name: &str) -> Option<Arc<Inode>> {    // yifan 2026/6/7: 在当前目录下按名字查找一个文件或子目录；如果找到就返回对应的 Inode 句柄，否则返回 None，所以返回类型是 Option<Arc<Inode>>。
        let fs = self.fs.lock();    // yifan 2026/6/7: 先拿到文件系统锁，后面需要用它根据 inode_id 计算目标 inode 在块设备上的逻辑位置。
        self.read_disk_inode(|disk_inode| {    // yifan 2026/6/7: 读取当前 Inode 在磁盘上的 DiskInode 内容，这里这个 disk_inode 表示当前目录本身的磁盘 inode。
            self.find_inode_id(name, disk_inode).map(|inode_id| {    // yifan 2026/6/7: 先在当前目录中查找名字对应的 inode_id；find_inode_id 的返回值是 Option<u32>，这里用 map 表示：找到时把 inode_id 转成 Arc<Inode>，没找到时保持 None。
                let (block_id, block_offset) = fs.get_disk_inode_pos(inode_id);    // yifan 2026/6/7: 将找到的 inode 编号转换成它在块设备上的逻辑块号和块内偏移。
                Arc::new(Self::new(    // yifan 2026/6/7: 这里闭包内部只构造并返回一个 Arc<Inode>；外层之所以仍然是 Option，是因为 map 会自动把这个 Arc 包装成 Some(Arc<Inode>)，而在没找到时直接返回 None。
                    block_id,    // yifan 2026/6/7: 目标 inode 所在的逻辑块号。
                    block_offset,    // yifan 2026/6/7: 目标 inode 在该逻辑块内的字节偏移。
                    self.fs.clone(),    // yifan 2026/6/7: 共享同一个 EasyFileSystem 实例，使返回的 Inode 句柄后续仍能访问整个文件系统。
                    self.block_device.clone(),    // yifan 2026/6/7: 复制底层块设备指针，使返回的 Inode 句柄后续可以继续读写对应的磁盘块。
                    inode_id,    // yifan 2026/6/16: 设置 inode_id，使返回的 Inode 句柄可以在 get_stat 时使用。
                ))
            })
        })
    }
    /// Increase the size of a disk inode
    fn increase_size(
        &self,
        new_size: u32,
        disk_inode: &mut DiskInode,
        fs: &mut MutexGuard<EasyFileSystem>,
    ) {
        if new_size < disk_inode.size { // yifan 2026/6/8: 如果新的大小 new_size 比当前 inode 的大小还小，说明不需要增加数据块了，直接返回
            return;
        }
        let blocks_needed = disk_inode.blocks_num_needed(new_size);
        let mut v: Vec<u32> = Vec::new();
        for _ in 0..blocks_needed {
            v.push(fs.alloc_data());
        }
        disk_inode.increase_size(new_size, v, &self.block_device);
    }
    /// Create inode under current inode by name
    /// yifan 2026/6/17: 在当前目录下创建一个新文件；如果成功就返回新文件对应的 Inode 句柄，否则返回 None。
    /// 因为本项目的 easy-fs 设计中，只有根目录 Inode 才会调用这个方法，所以这里的 self 实际上是根目录 Inode。
    /// 而op闭包中的root_inode是调用这个方法的Inode对应的DiskInode，也就是根目录的inode。
    pub fn create(&self, name: &str) -> Option<Arc<Inode>> {
        let mut fs = self.fs.lock();
        let op = |root_inode: &DiskInode| {    // yifan 2026/6/7: 定义一个闭包 op，它接收当前目录对应的 DiskInode 作为参数；后面会把这个闭包传给 read_disk_inode(op)，让 read_disk_inode 先读出当前 inode 的磁盘内容，再交给这个闭包处理。
            // assert it is a directory
            assert!(root_inode.is_dir());
            // has the file been created?
            // yifan 2026/6/7: 遍历根目录中的所有 DirEntry，检查是否已有同名文件。
            // 如果找到，返回的结果是 Some(inode_id)，于是 create 返回 None。表示创建失败，因为不允许根目录中出现两个同名目录项。
            self.find_inode_id(name, root_inode) 
        };
        if self.read_disk_inode(op).is_some() {
            return None;
        }
        // create a new file
        // alloc a inode with an indirect block
        let new_inode_id = fs.alloc_inode();    // yifan 2026/6/8: 从 inode 位图中分配一个新的空闲 inode 编号，这说明当前是在创建一个原本不存在的新文件。
        // initialize inode
        let (new_inode_block_id, new_inode_block_offset) = fs.get_disk_inode_pos(new_inode_id);    // yifan 2026/6/8: 根据新分配的 inode 编号，计算它在 inode 区中的逻辑块号和块内偏移，后面需要到这个位置去写入新的 DiskInode。
        get_block_cache(new_inode_block_id as usize, Arc::clone(&self.block_device))    // yifan 2026/6/8: 取出保存这个新 inode 的逻辑块缓存，准备修改其中对应的位置。
            .lock()    // yifan 2026/6/8: 对块缓存加锁，因为接下来要修改块里的内容。
            .modify(new_inode_block_offset, |new_inode: &mut DiskInode| {    // yifan 2026/6/8: 从块内 new_inode_block_offset 偏移处开始修改，并把这段内容当成一个新的 DiskInode。
                new_inode.initialize(DiskInodeType::File);    // yifan 2026/6/8: 将这个新 DiskInode 初始化为普通文件类型；初始化后它还是一个空文件，大小为 0，数据块索引也都清空。
            });
        self.modify_disk_inode(|root_inode| {    // yifan 2026/6/8: 下面开始修改当前目录自己的 DiskInode，把新文件对应的目录项追加到目录内容中。
            // append file in the dirent
            let file_count = (root_inode.size as usize) / DIRENT_SZ;    // yifan 2026/6/8: 计算当前目录里已有多少个目录项，因为目录内容本质上是一串连续的 DirEntry。
            let new_size = (file_count + 1) * DIRENT_SZ;    // yifan 2026/6/8: 由于现在要新增一个目录项，所以目录大小需要扩展到能够容纳 file_count + 1 条目录项。
            // increase size
            self.increase_size(new_size as u32, root_inode, &mut fs);    // yifan 2026/6/8: 先扩容当前目录 inode；如果目录原有的数据块不够放新的目录项，这里会为目录再分配新的数据块。
            // write dirent
            let dirent = DirEntry::new(name, new_inode_id);    // yifan 2026/6/8: 构造一条新的目录项，内容是“文件名 name -> inode 编号 new_inode_id”的映射。
            root_inode.write_at(
                file_count * DIRENT_SZ,    // yifan 2026/6/8: 这一条新目录项要写到目录末尾，因此起始偏移正好是当前已有目录项数量乘以每条目录项的大小。
                dirent.as_bytes(),    // yifan 2026/6/8: 将刚构造好的目录项按字节形式写入目录内容中。
                &self.block_device,    // yifan 2026/6/8: 通过底层块设备把这条目录项真正写入目录对应的数据块。
            );
        });

        let (block_id, block_offset) = fs.get_disk_inode_pos(new_inode_id);    // yifan 2026/6/8: 再次根据 new_inode_id 计算这个新文件 inode 在 inode 区中的逻辑块号和块内偏移，目的是为后面构造返回的 Inode 句柄提供定位信息。
        block_cache_sync_all();    // yifan 2026/6/8: 将前面通过块缓存完成的修改统一刷回块设备，包括新文件 inode 的初始化、目录项的追加以及可能发生的目录扩容。
        // return inode
        Some(Arc::new(Self::new(    // yifan 2026/6/8: 构造并返回新文件对应的 Inode 句柄；外层的 Some 表示创建成功，因为 create 的返回类型是 Option<Arc<Inode>>。
            block_id,    // yifan 2026/6/8: 新文件对应的 DiskInode 所在逻辑块号。
            block_offset,    // yifan 2026/6/8: 新文件对应的 DiskInode 在该逻辑块内的字节偏移。
            self.fs.clone(),    // yifan 2026/6/8: 让返回的 Inode 继续共享同一个 EasyFileSystem 实例，后续可以继续访问整个文件系统。
            self.block_device.clone(),    // yifan 2026/6/8: 复制底层块设备指针，使返回的 Inode 句柄后续仍能通过块设备读写自己的数据。
            new_inode_id,    // yifan 2026/6/16: 设置 inode_id，使返回的 Inode 句柄可以在 get_stat 时使用。
        )))
        // release efs lock automatically by compiler
    }
    /// List inodes under current inode
    pub fn ls(&self) -> Vec<String> {
        let _fs = self.fs.lock();    // yifan 2026/6/7: 先拿到文件系统锁，保证后续读取当前目录内容的过程与文件系统其他操作保持一致。
        self.read_disk_inode(|disk_inode| {    // yifan 2026/6/7: 读取当前 Inode 对应的磁盘 inode，这里的 disk_inode 就是当前目录本身的 DiskInode。
            let file_count = (disk_inode.size as usize) / DIRENT_SZ;    // yifan 2026/6/7: 计算当前目录中有多少个目录项；因为目录内容是一串连续的 DirEntry，而每个 DirEntry 的大小固定为 DIRENT_SZ。
            let mut v: Vec<String> = Vec::new();    // yifan 2026/6/7: 创建一个空的字符串向量，用来收集目录中所有文件名或子目录名。
            for i in 0..file_count {    // yifan 2026/6/7: 依次遍历目录中的每一个目录项。
                let mut dirent = DirEntry::empty();    // yifan 2026/6/7: 创建一个空的 DirEntry 作为临时缓冲区，用来接收从目录文件中读出的第 i 条目录项。
                assert_eq!(
                    disk_inode.read_at(i * DIRENT_SZ, dirent.as_bytes_mut(), &self.block_device,),    // yifan 2026/6/7: 从目录 inode 的内容中读取第 i 个目录项；偏移 i * DIRENT_SZ 表示第 i 条目录项在目录文件中的起始位置，dirent.as_bytes_mut() 提供一个可写字节缓冲区来接收这条目录项的数据。
                    DIRENT_SZ,    // yifan 2026/6/7: 断言本次读取的字节数必须正好等于一条完整 DirEntry 的大小，否则说明目录项没有被完整读出。
                );
                v.push(String::from(dirent.name()));    // yifan 2026/6/7: 取出刚读到的目录项名字，并转换成 String 后放入结果向量 v。
            }
            v
        })
    }
    /// Read data from current inode
    pub fn read_at(&self, offset: usize, buf: &mut [u8]) -> usize {
        let _fs = self.fs.lock();
        self.read_disk_inode(|disk_inode| disk_inode.read_at(offset, buf, &self.block_device))
    }
    /// Write data to current inode
    pub fn write_at(&self, offset: usize, buf: &[u8]) -> usize {
        let mut fs = self.fs.lock();
        let size = self.modify_disk_inode(|disk_inode| {
            self.increase_size((offset + buf.len()) as u32, disk_inode, &mut fs); // yifan 2026/6/8: 在写入数据之前，先调用 increase_size 来确保当前 inode 的大小足够容纳 offset + buf.len() 这么多字节；如果当前 inode 的数据块不够用，这里会为它分配新的数据块。如果当前inode原来就足够大了，increase_size 就直接返回，不做任何修改。
            disk_inode.write_at(offset, buf, &self.block_device)
        });
        block_cache_sync_all();
        size
    }
    /// Clear the data in current inode
    pub fn clear(&self) {    // yifan 2026/6/8: 清空当前文件对应的内容，并回收它占用的所有数据块和相关索引块。
        let mut fs = self.fs.lock();    // yifan 2026/6/8: 先获取文件系统锁，因为后面回收数据块时需要修改全局的数据块分配状态。
        self.modify_disk_inode(|disk_inode| {    // yifan 2026/6/8: 进入当前文件对应的 DiskInode，准备直接修改它的大小和块索引信息。
            let size = disk_inode.size;    // yifan 2026/6/8: 先记录清空前文件原来的大小，后面要用它来校验理论上应该释放多少个块。
            let data_blocks_dealloc = disk_inode.clear_size(&self.block_device);    // yifan 2026/6/8: 清空这个 DiskInode 的大小和块索引，并返回所有应该被释放的块号列表，其中不仅包括普通数据块，也可能包括间接索引块。
            assert!(data_blocks_dealloc.len() == DiskInode::total_blocks(size) as usize);    // yifan 2026/6/8: 做一致性检查，确认 clear_size 返回的待释放块数，正好等于该文件在原大小 size 下理论上占用的总块数。
            for data_block in data_blocks_dealloc.into_iter() {    // yifan 2026/6/8: 遍历所有待释放的块号，逐个交给文件系统回收。
                fs.dealloc_data(data_block);    // yifan 2026/6/8: 回收当前这个块；dealloc_data 会把块内容清零，并在 data bitmap 中把它重新标记为空闲。
            }
        });
        block_cache_sync_all();    // yifan 2026/6/8: 将前面对 inode 和数据块回收状态的修改统一刷回块设备，确保清空操作真正生效。
    }

    /// yifan 2026/6/16: get inode id
    pub fn inode_id(&self) -> u32 {
        self.inode_id
    }

    /// yifan 2026/6/16: 判断为目录
    pub fn is_dir(&self) -> bool {
        self.read_disk_inode(|disk_inode| disk_inode.is_dir())
    }

    /// yifan 2026/6/16: 判断为文件
    pub fn is_file(&self) -> bool {
        self.read_disk_inode(|disk_inode| disk_inode.is_file())
    }

    /// yifan 2026/6/16: set nlink
    pub fn set_nlink(&self, nlink: u32) {
        self.modify_disk_inode(|disk_inode| disk_inode.set_nlink(nlink));
        // block_cache_sync_all(); // 这里先不做磁盘同步，较少开销。在调用set_nlink的函数中做同步。
    }

    /// yifan 2026/6/16: get nlink
    pub fn get_nlink(&self) -> u32 {
        self.read_disk_inode(|disk_inode| disk_inode.nlink())
    }

    /// yifan 2026/6/17: bind new path to current inode, 只有root node会调用
    pub fn link(&self, old_name: &str, new_name: &str) -> bool {
        if let Some(inode_id) = self.find(old_name) { // yifan 2026/6/17: 先在当前目录下查找 old_name 对应的 inode；如果找不到就返回false，表示 link 失败。
            let mut fs = self.fs.lock();
            // yifan 2026/6/17: 在当前目录下创建一个新的目录项，名字是 new_name，指向目标 inode 编号 target_inode；不考虑_new_name已经存在的情况。
            self.modify_disk_inode(|root_inode| {    
                // append file in the dirent
                let file_count = (root_inode.size as usize) / DIRENT_SZ;    
                let new_size = (file_count + 1) * DIRENT_SZ;    
                // increase size
                self.increase_size(new_size as u32, root_inode, &mut fs);    
                // write dirent
                let dirent = DirEntry::new(new_name, inode_id.inode_id());    
                root_inode.write_at(
                    file_count * DIRENT_SZ,    
                    dirent.as_bytes(),    
                    &self.block_device,    
                );
            });

            // yifan 2026/6/17: 更新target_inode的nlink字段，表示又多了一个名字指向它了。
            let current_nlink = inode_id.get_nlink();
            inode_id.set_nlink(current_nlink + 1); // yifan 2026/6/17: 先获取当前 nlink 的值，然后加 1 后再设置

            block_cache_sync_all(); // yifan 2026/6/17: 将前面通过块缓存完成的修改统一刷回块设备，包括目录项的追加以及可能发生的目录扩容。
            true

        } else {
            false
        }
    }

    /// yifan 2026/6/18: unlink a path from current inode, 只有root node会调用
    pub fn unlink(&self, name:&str) -> bool {
        if let Some(inode_id) = self.find(name) {
            if inode_id.get_nlink() == 1 {
                // yifan 2026/6/18: 如果nlink为1，直接清空inode内容并回收数据块
                inode_id.clear();
            } 
            
            // yifan 2026/6/18: 如果nlink大于1，只需要把nlink减1，删除目录项即可，不需要清空inode内容和回收数据块。
            let current_nlink = inode_id.get_nlink();
            inode_id.set_nlink(current_nlink - 1);

            // yifan 2026/6/18: 在当前目录下删除一个目录项，名字是 name。
            let delete_index = self.read_disk_inode(|root_inde| {
                self.find_direntry_index(name, root_inde)   // yifan 2026/6/18: 在当前目录下查找要删除的目录项的索引位置；如果找不到就 panic，因为前面 find 已经确认了这个文件存在。
            });
            let delete_index = match delete_index {
                Some(index) => index,
                None => return false,
            };
            self.modify_disk_inode(|root_inode| {    
                // yifan 2026/6/18: 在当前目录下删除一个目录项，名字是 name，指向目标 inode 编号 target_inode。
                let file_count = (root_inode.size as usize) / DIRENT_SZ;

                let mut dirent = DirEntry::empty();
                for i in delete_index + 1..file_count {
                    // yifan 2026/6/18: 从被删除目录项的下一条开始，依次把后面的目录项往前移动一条位置，覆盖掉被删除的目录项；最后再把目录大小缩小一个目录项的大小。
                    assert_eq!(
                        root_inode.read_at(DIRENT_SZ * i, dirent.as_bytes_mut(), &self.block_device,),    // yifan 2026/6/18: 从目录 inode 的内容中读取第 i 个目录项；因为目录本质上也是文件，内容是一串连续排列的 DirEntry，所以第 i 项的起始偏移就是 DIRENT_SZ * i，而 dirent.as_bytes_mut() 提供了一个可写字节缓冲区来接收这 32 字节数据。
                        DIRENT_SZ,    // yifan 2026/6/18: 断言这次读取的字节数必须正好等于一个完整目录项的大小，否则说明没有正确读出一条完整的 DirEntry。
                    );
                    assert_eq!(
                        root_inode.write_at(DIRENT_SZ * (i - 1), dirent.as_bytes(), &self.block_device,),    // yifan 2026/6/18: 把刚读到的目录项写到前一个目录项的位置上，覆盖掉被删除的目录项；偏移 (i - 1) * DIRENT_SZ 表示前一个目录项的位置。
                        DIRENT_SZ,    // yifan 2026/6/18: 断言这次写入的字节数必须正好等于一条完整目录项的大小，否则说明没有正确写入一条完整的 DirEntry。
                    );
                }
                // yifan 2026/6/18: 最后再把目录大小缩小一个目录项的大小。
                let new_size = (file_count - 1) * DIRENT_SZ;
                root_inode.decrease_size(new_size as u32);
            });
            block_cache_sync_all(); // yifan 2026/6/18: 将前面通过块缓存完成的修改统一刷回块设备，包括nlink的修改。
            
            true
        } else {
            false
        }
    }

    /// yifan 2026/6/18: find direntry index by name, 只有root node会调用
    pub fn find_direntry_index(&self, name: &str, disk_inode: &DiskInode) ->Option<usize> {
        // assert it is a directory
        assert!(disk_inode.is_dir()); 
        // yifan 2026/6/6: disk_inode.size 表示目录文件的总字节数。目录文件的内容是由一个个 DirEntry 组成的，每个 DirEntry 占用 DIRENT_SZ=32 字节。因此，目录文件的大小除以 DIRENT_SZ 就得到目录项的数量，也就是文件的数量。
        // 一个block size是512字节，那么一个block里可以存放16个dirent。如果超过16个dirent，就需要DiskInode::increase_size增加目录文件大小。
        let file_count = (disk_inode.size as usize) / DIRENT_SZ; 
        let mut dirent = DirEntry::empty();    // yifan 2026/6/7: 先创建一个空的 DirEntry 作为临时缓冲区，用来接收从目录内容中读出的一条目录项。
        for i in 0..file_count {    // yifan 2026/6/7: 遍历目录中的每一条目录项，i 表示当前是第几个目录项。
            assert_eq!(
                disk_inode.read_at(DIRENT_SZ * i, dirent.as_bytes_mut(), &self.block_device,),    // yifan 2026/6/7: 从目录 inode 的内容中读取第 i 个目录项；因为目录本质上也是文件，内容是一串连续排列的 DirEntry，所以第 i 项的起始偏移就是 DIRENT_SZ * i，而 dirent.as_bytes_mut() 提供了一个可写字节缓冲区来接收这 32 字节数据。
                DIRENT_SZ,    // yifan 2026/6/7: 断言这次读取的字节数必须正好等于一个完整目录项的大小，否则说明没有正确读出一条完整的 DirEntry。
            );
            if dirent.name() == name {
                return Some(i);
            }
        }
        None
    }
}
