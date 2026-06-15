use clap::{App, Arg};
use easy_fs::{BlockDevice, EasyFileSystem};
use std::fs::{read_dir, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::Arc;
use std::sync::Mutex;

const BLOCK_SZ: usize = 512;

struct BlockFile(Mutex<File>);    // yifan 2026/6/8: `BlockFile(Mutex<File>)` 表示“一个带锁的普通文件”，其中 `File` 是标准库里的普通文件对象，`Mutex` 用来保证并发读写这个模拟磁盘文件时不会发生数据混乱。

impl BlockDevice for BlockFile {    // yifan 2026/6/8: 这里让 `BlockFile` 实现 `BlockDevice` trait，这样 easy-fs 就不关心底层是真实硬盘、VirtIO 块设备还是普通文件，只要底层实现了这个抽象接口即可。
    fn read_block(&self, block_id: usize, buf: &mut [u8]) {    // yifan 2026/6/8: `read_block` 的职责是读取指定块号的数据，也就是把“读第几个块”的请求转换成“读这个普通文件中的哪一段字节”。
        let mut file = self.0.lock().unwrap();    // yifan 2026/6/8: 先给内部的文件加锁，因为这个普通文件正在扮演块设备，多个地方同时访问时需要串行化，避免并发读写破坏数据。
        file.seek(SeekFrom::Start((block_id * BLOCK_SZ) as u64))    // yifan 2026/6/8: 先 `seek` 到第 `block_id` 块的起始字节位置；因为普通文件的读写基于当前偏移量，不会自动知道你要访问第几个块，例如第 3 块对应偏移 `3 * 512 = 1536`。
            .expect("Error when seeking!");
        assert_eq!(file.read(buf).unwrap(), BLOCK_SZ, "Not a complete block!");    // yifan 2026/6/8: 从该位置读取恰好一个块的大小，也就是 512 字节到 `buf` 中；这说明用户态测试时所谓“读块”，本质上就是从模拟磁盘文件中读取固定长度的一段内容。
    }

    fn write_block(&self, block_id: usize, buf: &[u8]) {    // yifan 2026/6/8: `write_block` 与 `read_block` 对应，负责把某个块的数据写回去，本质上是把要写入的块映射到普通文件中的某段字节范围。
        let mut file = self.0.lock().unwrap();    // yifan 2026/6/8: 写入前同样先加锁，保证对这个模拟块设备文件的写操作是安全的。
        file.seek(SeekFrom::Start((block_id * BLOCK_SZ) as u64))    // yifan 2026/6/8: 写块之前也必须先移动文件指针到目标块起始位置，例如第 5 块就对应偏移 `5 * 512 = 2560`。
            .expect("Error when seeking!");
        assert_eq!(file.write(buf).unwrap(), BLOCK_SZ, "Not a complete block!");    // yifan 2026/6/8: 将 `buf` 中恰好一个块大小的数据写入文件，这说明 easy-fs 在用户态测试时实际是在把普通文件当作“虚拟磁盘”来按块读写。
    }
}

fn main() {
    easy_fs_pack().expect("Error when packing easy-fs!");
}

// yifan 2026/6/8:
// easy_fs_pack() 是一个用户态工具，其作用是：读取主机文件系统中的应用 ELF 文件
//               ↓
// 在 easy-fs 镜像中创建同名文件
//               ↓
// 把 ELF 二进制数据写入 easy-fs 文件
// 最终生成：target/fs.img
fn easy_fs_pack() -> std::io::Result<()> {    // yifan 2026/6/9: 这里定义了 `easy_fs_pack` 函数，返回类型是 `std::io::Result<()>`，表示执行过程中如果发生文件相关错误就返回 `Err`，成功时返回 `Ok(())`。
    // yifan 2026/6/9: 为了实现 easy-fs-fuse 和 os/user 的解耦，第 6~21 行使用 clap crate 进行命令行参数解析，
    // 需要通过 -s 和 -t 分别指定应用的源代码目录和保存应用 ELF 的目录，而不是在 easy-fs-fuse 中硬编码。如果解析成功的话它们会分别被保存在变量 src_path 和 target_path 中。
    let matches = App::new("EasyFileSystem packer")    // yifan 2026/6/9: 这里用 `clap` 创建一个命令行参数解析器，`EasyFileSystem packer` 是这个命令行程序的名字。
        .arg(
            Arg::with_name("source")    // yifan 2026/6/9: 这里定义第一个命令行参数，内部名字叫 `source`，后面会通过这个名字取出用户传入的值。
                .short("s")    // yifan 2026/6/9: 这里给 `source` 参数指定短选项 `-s`，所以用户可以用 `-s <路径>` 的形式传参。
                .long("source")    // yifan 2026/6/9: 这里给 `source` 参数指定长选项 `--source`，所以也可以写成 `--source <路径>`。
                .takes_value(true)    // yifan 2026/6/9: 这里说明 `source` 不是单独的开关，而是必须携带一个具体的值，也就是一个路径字符串。
                .help("Executable source dir(with backslash)"),    // yifan 2026/6/9: 这里是 `source` 参数的帮助信息，作用是在命令行帮助输出中提示它表示“可执行文件来源目录”。
        )
        .arg(
            Arg::with_name("target")    // yifan 2026/6/9: 这里定义第二个命令行参数，内部名字叫 `target`，后面会用它取得目标目录路径。
                .short("t")    // yifan 2026/6/9: 这里给 `target` 参数指定短选项 `-t`，因此用户可以写 `-t <路径>`。
                .long("target")    // yifan 2026/6/9: 这里给 `target` 参数指定长选项 `--target`，也可以写成 `--target <路径>`。
                .takes_value(true)    // yifan 2026/6/9: 这里说明 `target` 也必须带一个实际的值，而不是只写一个选项名。
                .help("Executable target dir(with backslash)"),    // yifan 2026/6/9: 这里是 `target` 参数的帮助信息，用来提示它表示“可执行文件目标目录”。
        )
        .get_matches();
    let src_path = matches.value_of("source").unwrap();    // yifan 2026/6/9: 这里把命令行参数 `source` 取出来，表示源目录路径；后面程序会用它去枚举有哪些应用文件需要打包。
    let target_path = matches.value_of("target").unwrap();    // yifan 2026/6/9: 这里把命令行参数 `target` 取出来，表示目标目录路径；后面会在这个目录下创建 `fs.img`，而且当前代码里也会从这个目录读取应用文件内容。
    println!("src_path = {}\ntarget_path = {}", src_path, target_path);
    let block_file = Arc::new(BlockFile(Mutex::new({    // yifan 2026/6/8: easy-fs 可以先在用户态测试，是因为它本身只是文件系统库，只依赖抽象的 `BlockDevice`，这里就是在普通操作系统里构造一个基于文件的块设备实现给它使用。
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(format!("{}{}", target_path, "fs.img"))?;    // yifan 2026/6/8: 这里打开或创建一个普通文件 `fs.img`，用它来模拟真实磁盘；文件中的字节区间会被按 512 字节一块切分成虚拟块设备。    // yifan 2026/6/9: 这里也能看出 `target` 的一个明确用途：`fs.img` 会被创建在 `target` 指向的目录下。
        f.set_len(16 * 2048 * 512).unwrap();
        f
    })));
    // 16MiB, at most 4095 files
    let efs = EasyFileSystem::create(block_file, 16 * 2048, 1);    // yifan 2026/6/8: easy-fs 的核心逻辑运行在这个模拟块设备之上，先在用户态把文件系统功能验证好，再接入内核，比一开始就在内核里调试更容易定位问题。
    let root_inode = Arc::new(EasyFileSystem::root_inode(&efs));
    let apps: Vec<_> = read_dir(src_path)    // yifan 2026/6/9: 这里说明 `source` 的直接用途是读取这个目录中的目录项，也就是先确定“有哪些 app”。获取源码目录中的每个应用的源代码文件并去掉后缀名，收集到向量 apps 中。
        .unwrap()
        .into_iter()
        .map(|dir_entry| {
            let mut name_with_ext = dir_entry.unwrap().file_name().into_string().unwrap();
            name_with_ext.drain(name_with_ext.find('.').unwrap()..name_with_ext.len());
            name_with_ext
        })
        .collect();
    for app in apps {
        // load app data from host file system
        let mut host_file = File::open(format!("{}{}", target_path, app)).unwrap();    // yifan 2026/6/9: 打开主机上的 ELF 文件。按当前实现，程序读取每个应用文件内容时用的也是 `target` 路径而不是 `source`，所以这份代码里的实际分工是：`source` 用来列出 app 名单，`target` 用来放 `fs.img`，并且也参与读取 app 文件。
        let mut all_data: Vec<u8> = Vec::new();
        host_file.read_to_end(&mut all_data).unwrap(); // yifan 2026/6/9: 把整个 ELF 文件内容读到 `all_data` 这个字节向量中，后面会把它写入 easy-fs 中对应的文件。
        // create a file in easy-fs
        let inode = root_inode.create(app.as_str()).unwrap(); // 在 easy-fs 中创建同名文件
        // write data to easy-fs
        inode.write_at(0, all_data.as_slice());// 从文件偏移量 0 开始，把整个 ELF 数据写入 easy-fs 文件。
    }
    // list apps
    // for app in root_inode.ls() {
    //     println!("{}", app);
    // }
    Ok(())
}

#[test]
fn efs_test() -> std::io::Result<()> {
    let block_file = Arc::new(BlockFile(Mutex::new({    // yifan 2026/6/8: `easy-fs-fuse` 是用户态测试程序，它可以使用 `std`、普通文件和调试输出；与真正要进内核、需要 `no_std` 的 `easy-fs` 核心库分工不同。BlockFile(Mutex::new(f))把普通文件 f 包装成一个带锁的块设备
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open("target/fs.img")?;    // yifan 2026/6/8: 测试里同样通过 `fs.img` 这样的普通文件来模拟块设备，因此不需要真实裸磁盘也能验证 easy-fs 的目录、inode 和文件读写逻辑。
        f.set_len(8192 * 512).unwrap(); // yifan 2026/6/9:8192 × 512 = 4194304 字节 = 4 MiB。target/fs.img 被当成一个 4MiB 的虚拟磁盘。
        f
    })));
    EasyFileSystem::create(block_file.clone(), 4096, 1); // yifan 2026/6/9: 这一步是在虚拟块设备上创建一个新的 easy-fs 文件系统。可以理解为格式化磁盘：把 target/fs.img 的前 4096 个块初始化成 easy-fs 文件系统
    let efs = EasyFileSystem::open(block_file.clone());
    let root_inode = EasyFileSystem::root_inode(&efs);
    root_inode.create("filea");
    root_inode.create("fileb");
    for name in root_inode.ls() {
        println!("{}", name);
    }
    let filea = root_inode.find("filea").unwrap();
    let greet_str = "Hello, world!";
    filea.write_at(0, greet_str.as_bytes());
    //let mut buffer = [0u8; 512];
    let mut buffer = [0u8; 233];
    let len = filea.read_at(0, &mut buffer);
    assert_eq!(greet_str, core::str::from_utf8(&buffer[..len]).unwrap(),);

    let mut random_str_test = |len: usize| {
        filea.clear();
        assert_eq!(filea.read_at(0, &mut buffer), 0,);
        let mut str = String::new();
        use rand;
        // random digit
        for _ in 0..len {
            str.push(char::from('0' as u8 + rand::random::<u8>() % 10));
        }
        filea.write_at(0, str.as_bytes());
        let mut read_buffer = [0u8; 127];
        let mut offset = 0usize;
        let mut read_str = String::new();
        loop {
            let len = filea.read_at(offset, &mut read_buffer);
            if len == 0 {
                break;
            }
            offset += len;
            read_str.push_str(core::str::from_utf8(&read_buffer[..len]).unwrap());
        }
        assert_eq!(str, read_str);
    };

    random_str_test(4 * BLOCK_SZ);
    random_str_test(8 * BLOCK_SZ + BLOCK_SZ / 2);
    random_str_test(100 * BLOCK_SZ);
    random_str_test(70 * BLOCK_SZ + BLOCK_SZ / 7);
    random_str_test((12 + 128) * BLOCK_SZ);
    random_str_test(400 * BLOCK_SZ);
    random_str_test(1000 * BLOCK_SZ);
    random_str_test(2000 * BLOCK_SZ);

    Ok(())
}
