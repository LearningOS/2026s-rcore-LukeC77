//! Loading user applications into memory

/// Get the total number of applications.
use alloc::vec::Vec;
use lazy_static::*;
///get app number
pub fn get_num_app() -> usize {
    extern "C" {
        fn _num_app();
    }
    unsafe { (_num_app as usize as *const usize).read_volatile() }
}
/// get applications data
pub fn get_app_data(app_id: usize) -> &'static [u8] {
    extern "C" {    // yifan 2026/5/23: 声明外部符号 `_num_app`，它在 link_app.S 中对应“应用数量 + 应用边界地址表”的起始位置。
        fn _num_app();    // yifan 2026/5/23: 该符号地址会被当作指针读取：第 0 项是数量，后续项是各 app 起止地址。
    }
    let num_app_ptr = _num_app as usize as *const usize;    // yifan 2026/5/23: 将 `_num_app` 的符号地址转为 `*const usize`，便于按 usize 表项读取。
    let num_app = get_num_app();    // yifan 2026/5/23: 读取应用总数。
    let app_start = unsafe { core::slice::from_raw_parts(num_app_ptr.add(1), num_app + 1) };    // yifan 2026/5/23: 把数量字段后的 `num_app+1` 个地址视作切片：`[app_0_start, ..., app_{n-1}_start, app_{n-1}_end]`。
    assert!(app_id < num_app);    // yifan 2026/5/23: 越界检查，确保 app_id 合法。
    unsafe {
        core::slice::from_raw_parts(
            app_start[app_id] as *const u8,    // yifan 2026/5/23: 当前应用字节块起始地址。
            app_start[app_id + 1] - app_start[app_id],    // yifan 2026/5/23: 以相邻地址差作为长度，返回该应用 ELF 原始二进制切片（通常含 ELF 头/.text/.rodata/.data；.bss 通常为 NOBITS，不在文件字节中）。
        )
    }
}

lazy_static! {    // yifan 2026/5/23: 声明惰性初始化的静态数据结构，首次使用时才执行初始化代码。
    ///All of app's name
    static ref APP_NAMES: Vec<&'static str> = {    // yifan 2026/5/23: 定义全局应用名表 APP_NAMES，类型是 `Vec<&'static str>`。
        let num_app = get_num_app();    // yifan 2026/5/23: 读取应用总数，后续据此循环解析同等数量的名字。
        extern "C" {    // yifan 2026/5/23: 声明来自汇编/链接结果的外部符号。
            fn _app_names();    // yifan 2026/5/23: `_app_names` 是名字表起始地址标签，在 Rust 中通过其符号地址访问名字区。
        }
        let mut start = _app_names as usize as *const u8;    // yifan 2026/5/23: 将 `_app_names` 符号地址转为字节指针，作为当前要解析的应用名起始位置（后续会不断前移）。
        let mut v = Vec::new();    // yifan 2026/5/23: 创建空向量，用于收集后续解析得到的所有应用名字符串。
        unsafe {
            for _ in 0..num_app {    // yifan 2026/5/23: 按应用总数循环，每次解析一个以 `\0` 结尾的应用名字符串。
                let mut end = start;    // yifan 2026/5/23: 从当前字符串起点 start 出发寻找结尾位置。
                while end.read_volatile() != b'\0' {    // yifan 2026/5/23: 逐字节读取，直到遇到空字符 `\0`。
                    end = end.add(1);    // yifan 2026/5/23: 未到结尾则指针后移一字节继续扫描。
                }
                let slice = core::slice::from_raw_parts(start, end as usize - start as usize);    // yifan 2026/5/23: 用 `[start, end)` 构造当前名字的字节切片（不包含结尾 `\0`）。
                let str = core::str::from_utf8(slice).unwrap();    // yifan 2026/5/23: 将字节切片按 UTF-8 解析为 `&str`，解析失败则 panic。
                v.push(str);    // yifan 2026/5/23: 把解析出的应用名加入 APP_NAMES 向量。
                start = end.add(1);    // yifan 2026/5/23: 跳过当前字符串末尾的 `\0`，指向下一个应用名起始地址。
            }
        }
        v
    };
}

#[allow(unused)]
///get app data from name
pub fn get_app_data_by_name(name: &str) -> Option<&'static [u8]> {
    let num_app = get_num_app();    // yifan 2026/5/23: 先读取应用总数，后续在 `[0, num_app)` 范围内按名字查找对应索引。
    (0..num_app)
        .find(|&i| APP_NAMES[i] == name)    // yifan 2026/5/23: 找到第一个满足 `APP_NAMES[i] == name` 的应用索引；找不到则返回 None。
        .map(get_app_data)    // yifan 2026/5/23: 找到索引后返回该应用的二进制字节切片（打包进内核的 ELF 原始字节流），因此返回类型是 `Option<&'static [u8]>` 而不是 `Option<String>`。
}
///list all apps
pub fn list_apps() {
    println!("/**** APPS ****");
    for app in APP_NAMES.iter() {
        println!("{}", app);
    }
    println!("**************/");
}
