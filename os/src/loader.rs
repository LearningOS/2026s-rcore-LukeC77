//! Loading user applications into memory

/// Get the total number of applications.
pub fn get_num_app() -> usize {                                                   // yifan 2026/5/7: 定义读取内核已打包应用总数的函数，返回值类型为 usize。
    extern "C" {                                                                   // yifan 2026/5/7: 声明外部链接符号，表示其地址由链接结果提供而非 Rust 函数体定义。
        fn _num_app();                                                             // yifan 2026/5/7: _num_app 符号由 link_app.S 生成，指向存放应用数量的数据位置。
    }
    unsafe { (_num_app as usize as *const usize).read_volatile() }    // yifan 2026/5/7: 将符号地址转为 *const usize 后做易失读取，确保编译器实际发起 load，不省略、不缓存替代；unsafe 来自裸指针解引用。
}

/// get applications data
pub fn get_app_data(app_id: usize) -> &'static [u8] {                              // yifan 2026/5/7: 按 app_id 取出该应用的 ELF 字节切片，返回生命周期为 'static 的只读数据。
    extern "C" {                                                                   // yifan 2026/5/7: 声明由链接结果提供的外部符号，下面将通过其地址读取应用索引表。
        fn _num_app();                                                             // yifan 2026/5/7: _num_app 指向 link_app.S 生成的数据表起点（首项为应用总数）。
    }
    let num_app_ptr = _num_app as usize as *const usize;                          // yifan 2026/5/7: 将 _num_app 符号地址转换为 *const usize，便于按 usize 表项访问。
    let num_app = get_num_app();                                                   // yifan 2026/5/7: 读取应用总数 num_app，供后续构造地址表和边界检查使用。
    let app_start = unsafe { core::slice::from_raw_parts(num_app_ptr.add(1), num_app + 1) };    // yifan 2026/5/7: 只有“裸地址+长度”可用，需用 from_raw_parts 将 _num_app 后的起始地址表解释为 &[usize]；长度为 num_app+1，最后一项是最后一个应用结束地址。
    assert!(app_id < num_app);                                                     // yifan 2026/5/7: 检查 app_id 不越界，保证访问 app_start[app_id+1] 安全。
    unsafe {                                                                       // yifan 2026/5/7: 再次基于裸地址构造切片，需要调用者保证地址有效、对齐正确、长度合法且生命周期内内存不失效。
        core::slice::from_raw_parts(                                               // yifan 2026/5/7: 将“起始指针+长度”解释为该应用完整镜像的 &[u8] 视图。
            app_start[app_id] as *const u8,                                        // yifan 2026/5/7: 当前应用镜像起始地址。
            app_start[app_id + 1] - app_start[app_id],                             // yifan 2026/5/7: 镜像字节长度=下一应用起始地址-当前应用起始地址。
        )
    }
}
