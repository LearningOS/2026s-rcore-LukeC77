//! Building applications linker

use std::fs::{read_dir, File};
use std::io::{Result, Write};

fn main() {
    println!("cargo:rerun-if-changed=../user/src/");
    println!("cargo:rerun-if-changed={}", TARGET_PATH);
    insert_app_data().unwrap();
}

static TARGET_PATH: &str = "../user/build/elf/";

/// get app data and build linker
fn insert_app_data() -> Result<()> {
    let mut f = File::create("src/link_app.S").unwrap();    // yifan 2026/5/23: 创建或覆盖 os/src/link_app.S，得到后续 writeln! 使用的可写文件句柄；若创建失败则 unwrap() 直接 panic 终止构建脚本。
    let mut apps: Vec<_> = read_dir("../user/build/elf/")    // yifan 2026/5/23: 从 ../user/build/elf/ 读取目录项，准备收集所有用户程序名。
        .unwrap()
        .into_iter()    // yifan 2026/5/23: 遍历读取到的每个目录项。
        .map(|dir_entry| {
            let mut name_with_ext = dir_entry.unwrap().file_name().into_string().unwrap();    // yifan 2026/5/23: 取出文件名并转为 String（失败则 unwrap() 触发 panic）。
            name_with_ext.drain(name_with_ext.find('.').unwrap()..name_with_ext.len());    // yifan 2026/5/23: 删除从第一个 '.' 到末尾的内容，即去掉扩展名（如 .elf）。
            name_with_ext    // yifan 2026/5/23: 返回去掉扩展名后的应用名，作为 map 的输出。
        })
        .collect();    // yifan 2026/5/23: 将所有处理后的应用名收集为 Vec 并赋给 apps。
    apps.sort();

    writeln!(    // yifan 2026/5/23: 将一段汇编头部写入 src/link_app.S；`?` 会在写入失败时向上返回错误。
        f,    // yifan 2026/5/23: 目标是前面创建的 link_app.S 文件句柄。
        r#"
    .align 3    // yifan 2026/5/23: 按 8 字节边界对齐。
    .section .data    // yifan 2026/5/23: 切换到数据段。
    .global _num_app    // yifan 2026/5/23: 导出全局符号 _num_app，供内核侧引用。
_num_app:
    .quad {}"#,
        apps.len()    // yifan 2026/5/23: 用 apps 的长度替换 `{}`，写入用户程序总数（8字节常量）。
    )?;

    for i in 0..apps.len() {    // yifan 2026/5/23: 遍历每个用户程序索引，准备生成“应用起始地址表”中的各项。
        writeln!(f, r#"    .quad app_{}_start"#, i)?;    // yifan 2026/5/23: 每次写入一个8字节地址项（.quad），其值为 app_i_start，对应第 i 个程序起始位置。
    }
    writeln!(f, r#"    .quad app_{}_end"#, apps.len() - 1)?;    // yifan 2026/5/23: 追加最后一个应用的结束地址项（app_{n-1}_end），与前面的各 app_i_start 一起形成边界表，便于用相邻项确定每个 app 的范围。

    writeln!(    // yifan 2026/5/23: 向 link_app.S 写入应用名字符串区的起始标记，`?` 在写失败时返回错误。
        f,    // yifan 2026/5/23: 写入目标是前面创建的文件句柄 f。
        r#"    // yifan 2026/5/23: 下面这段汇编将定义应用名表的全局入口标签。
    .global _app_names
_app_names:"#
    )?;    // yifan 2026/5/23: 后续 `.string` 会从 `_app_names` 标签处连续写入各应用名称。
    for app in apps.iter() {    // yifan 2026/5/23: 遍历 apps 中每个应用名，逐项生成用户程序名字表。
        writeln!(f, r#"    .string "{}""#, app)?;    // yifan 2026/5/23: 向 _app_names 后连续写入 `.string "应用名"`（以 \0 结尾），供内核运行时按名字匹配索引并结合地址表定位/加载对应程序；`?` 表示写失败即返回错误。
    }

    for (idx, app) in apps.iter().enumerate() {    // yifan 2026/5/23: 同时获取应用索引 idx 与应用名 app，逐个生成对应的打包汇编片段。
        println!("app_{}: {}", idx, app);    // yifan 2026/5/23: 构建时打印索引到应用名的映射，便于调试核对。
        writeln!(    // yifan 2026/5/23: 将每个应用的 `[start, end)` 边界与 incbin 规则写入 link_app.S，供内核后续按索引定位并加载。
            f,    // yifan 2026/5/23: 写入目标是 link_app.S 文件句柄 f。
            r#"
    .section .data    // yifan 2026/5/23: 切到内核镜像数据段；这里放的是用户程序 ELF 原始字节（静态数据），不是用户虚拟地址表。
    .global app_{0}_start    // yifan 2026/5/23: 导出该应用字节块起始符号，便于内核引用。
    .global app_{0}_end    // yifan 2026/5/23: 导出该应用字节块结束符号，和 start 共同定义范围。
    .align 3    // yifan 2026/5/23: 将后续起始地址按 8 字节对齐。
app_{0}_start:
    .incbin "{2}{1}.elf"    // yifan 2026/5/23: 把 `{TARGET_PATH}{app}.elf` 的原始文件字节嵌入镜像；运行时由内核解析 ELF 并映射到用户地址空间。
app_{0}_end:"#,
            idx, app, TARGET_PATH
        )?;
    }
    Ok(())
}
