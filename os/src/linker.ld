OUTPUT_ARCH(riscv)                    /* yifan 2026/5/6: 指定目标架构为 RISC-V，确保链接产物使用正确的指令与重定位规则。 */
ENTRY(_start)                         /* yifan 2026/5/6: 指定内核入口符号为 _start，启动后从这里开始执行。 */
BASE_ADDRESS = 0x80200000;            /* yifan 2026/5/6: 定义内核链接基址，内核镜像从该物理地址开始布局。 */

SECTIONS                              /* yifan 2026/5/6: 开始定义各输出段在内存中的排布规则。 */
{                                     /* yifan 2026/5/6: SECTIONS 块起始。 */
    . = BASE_ADDRESS;                 /* yifan 2026/5/6: 位置计数器设为基址，后续段从这里向高地址分配。 */
    skernel = .;                      /* yifan 2026/5/6: 记录内核起始地址符号，值为当前地址（通常是 0x80200000）。 */

    stext = .;                        /* yifan 2026/5/6: 记录代码段起始地址，用于后续建立代码区边界。 */
    .text : {                         /* yifan 2026/5/6: 定义可执行代码输出段。 */
        *(.text.entry)                /* yifan 2026/5/6: 优先放置启动入口代码，确保最先可执行。 */
        . = ALIGN(4K);                /* yifan 2026/5/6: 对齐到 4KB 页边界，为 trampoline 独立页做准备。 */
        strampoline = .;              /* yifan 2026/5/6: 记录 trampoline 起始地址符号，供页表映射与特权切换使用。 */
        *(.text.trampoline);          /* yifan 2026/5/6: 放置 trampoline 代码，通常要求固定且页对齐。 */
        . = ALIGN(4K);                /* yifan 2026/5/6: 再次页对齐，隔离 trampoline 与普通代码。 */
        *(.text .text.*)              /* yifan 2026/5/6: 放置其余所有常规代码段。 */
    }                                 /* yifan 2026/5/6: 结束 .text 段定义。 */

    . = ALIGN(4K);                    /* yifan 2026/5/6: 下一段前按页对齐，便于按段设置权限。 */
    etext = .;                        /* yifan 2026/5/6: 记录代码段结束地址，与 stext 一起标记代码段起止范围。 */
    srodata = .;                      /* yifan 2026/5/6: 记录只读数据段起始地址。 */
    .rodata : {                       /* yifan 2026/5/6: 定义只读数据输出段。 */
        *(.rodata .rodata.*)          /* yifan 2026/5/6: 放置常规只读常量数据。 */
        *(.srodata .srodata.*)        /* yifan 2026/5/6: 放置小型只读数据小节。 */
    }                                 /* yifan 2026/5/6: 结束 .rodata 段定义。 */

    . = ALIGN(4K);                    /* yifan 2026/5/6: 对齐到新页，分隔 rodata 与 data。 */
    erodata = .;                      /* yifan 2026/5/6: 记录只读数据段结束地址。 */
    sdata = .;                        /* yifan 2026/5/6: 记录已初始化可写数据段起始地址。 */
    .data : {                         /* yifan 2026/5/6: 定义已初始化可写数据输出段。 */
        *(.data .data.*)              /* yifan 2026/5/6: 放置常规 .data 全局/静态变量。 */
        *(.sdata .sdata.*)            /* yifan 2026/5/6: 放置小型 .data 小节。 */
    }                                 /* yifan 2026/5/6: 结束 .data 段定义。 */

    . = ALIGN(4K);                    /* yifan 2026/5/6: 页对齐，开始布局零初始化区域。 */
    edata = .;                        /* yifan 2026/5/6: 记录 data 段结束地址。 */
    sbss_with_stack = .;              /* yifan 2026/5/6: 记录 bss 总起点（包含可能的启动栈区域）。 */
    .bss : {                          /* yifan 2026/5/6: 定义未初始化数据段，启动时通常清零。 */
        *(.bss.stack)                 /* yifan 2026/5/6: 先放栈相关 bss 空间。 */
        sbss = .;                     /* yifan 2026/5/6: 记录普通 bss 起点（不含前面 stack 区）。 */
        *(.bss .bss.*)                /* yifan 2026/5/6: 放置常规未初始化数据。 */
        *(.sbss .sbss.*)              /* yifan 2026/5/6: 放置小型未初始化数据。 */
    }                                 /* yifan 2026/5/6: 结束 .bss 段定义。 */

    . = ALIGN(4K);                    /* yifan 2026/5/6: 末尾页对齐，便于按页统计内核占用。 */
    ebss = .;                         /* yifan 2026/5/6: 记录 bss 结束地址，常用于清零边界。 */
    ekernel = .;                      /* yifan 2026/5/6: 记录整个内核镜像结束地址，供内存管理确定可分配起点。 */

    /DISCARD/ : {                     /* yifan 2026/5/6: 定义需要丢弃的输入段。 */
        *(.eh_frame)                  /* yifan 2026/5/6: 丢弃异常展开信息，裸机内核通常不需要。 */
    }                                 /* yifan 2026/5/6: 结束 /DISCARD/ 段定义。 */
}                                     /* yifan 2026/5/6: 结束 SECTIONS 块。 */
