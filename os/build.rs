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
    let mut f = File::create("src/link_app.S").unwrap();
    let mut apps: Vec<_> = read_dir("../user/build/elf/")
        .unwrap()
        .into_iter()
        .map(|dir_entry| {                                                       // yifan 2026/5/7: dir_entry 是目录中的一条记录（DirEntry），在这里通常对应一个用户程序 .elf 文件。
            let mut name_with_ext = dir_entry.unwrap().file_name().into_string().unwrap();    // yifan 2026/5/7: 取出该目录项的文件名字符串（如 ch2b_hello.elf）。
            name_with_ext.drain(name_with_ext.find('.').unwrap()..name_with_ext.len());    // yifan 2026/5/7: 去掉扩展名 .elf，仅保留应用名供后续生成符号与打包使用。
            name_with_ext
        })
        .collect();
    apps.sort();

    writeln!(
        f,
        r#"
    .align 3
    .section .data
    .global _num_app
_num_app:
    .quad {}"#,
        apps.len()
    )?;

    for i in 0..apps.len() {
        writeln!(f, r#"    .quad app_{}_start"#, i)?;
    }
    writeln!(f, r#"    .quad app_{}_end"#, apps.len() - 1)?;

    for (idx, app) in apps.iter().enumerate() {
        println!("app_{}: {}", idx, app);
        writeln!(
            f,
            r#"
    .section .data
    .global app_{0}_start
    .global app_{0}_end
    .align 3
app_{0}_start:
    .incbin "{2}{1}.elf"
app_{0}_end:"#,
            idx, app, TARGET_PATH
        )?;
    }
    Ok(())
}
