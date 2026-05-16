.altmacro
.macro SAVE_GP n
    sd x\n, \n*8(sp)
.endm
.macro LOAD_GP n
    ld x\n, \n*8(sp)
.endm
    .section .text.trampoline                                                   # yifan 2026/5/7: 将 trap 入口/返回汇编放入独立段，便于链接器精确放置到 trampoline 页。
    .globl __alltraps                                                            # yifan 2026/5/7: 导出统一 trap 入口符号，内核可把 stvec 指向该跳板入口。
    .globl __restore                                                             # yifan 2026/5/7: 导出返回用户态符号，和 __alltraps 一起构成用户态/内核态切换门厅。
    .align 2
__alltraps:
    csrrw sp, sscratch, sp                                                      # yifan 2026/5/10: 语法为 csrrw rd, csr, rs1；此处 rd=sp, csr=sscratch, rs1=sp，按旧值语义原子交换：sp_new=sscratch_old，sscratch_new=sp_old。
    # now sp->*TrapContext in user space, sscratch->user stack
    # save other general purpose registers
    sd x1, 1*8(sp)                                                              # yifan 2026/5/10: sd 为 64 位存储指令；将 x1(ra，返回地址) 保存到内存 sp+8(1*8) 处，即 TrapContext 的第 1 个槽位，避免后续被覆盖。
    # skip sp(x2), we will save it later
    sd x3, 3*8(sp)                                                              # yifan 2026/5/10: sd 将 64 位寄存器值写入内存；这里把 x3(gp, global pointer) 保存到 sp+24(3*8)，即 TrapContext 的第 3 个槽位以便后续恢复。
    # skip tp(x4), application does not use it
    # save x5~x31
    .set n, 5
    .rept 27
        SAVE_GP %n
        .set n, n+1
    .endr
    # we can use t0/t1/t2 freely, because they have been saved in TrapContext
    csrr t0, sstatus                                                            # yifan 2026/5/10: sstatus 是 S 态状态寄存器，含 SIE/SPIE/SPP/SUM/MXR/FS/XS 等位；此处读入 t0 以保存 trap 前状态，供后续恢复与 sret 返回使用。
    csrr t1, sepc                                                               # yifan 2026/5/10: 从 sepc(异常程序计数器) 读到 t1；sepc 保存本次 trap 的返回地址，随后会写入 TrapContext，返回时恢复给 sret 跳回正确位置。
    sd t0, 32*8(sp)                                                             # yifan 2026/5/10: 将 t0 的 64 位值写入 sp+256(32*8)；结合上一行可知这里保存的是 sstatus，放入 TrapContext 第 32 个槽位用于返回前恢复。
    sd t1, 33*8(sp)                                                             # yifan 2026/5/10: 将 t1 的 64 位值存到 sp+264(33*8)；结合第 28 行可知此处保存 sepc 到 TrapContext 第 33 个槽位，返回时恢复给 sret 跳回正确位置。
    # read user stack from sscratch and save it in TrapContext
    csrr t2, sscratch
    sd t2, 2*8(sp)                                                              # yifan 2026/5/10: 至此 13-33 行是在把本次 trap 的用户态现场写回 TrapContext 对应槽位（可覆盖的现场区）。
    # load kernel_satp into t0
    ld t0, 34*8(sp)                                                             # yifan 2026/5/10: 这里读取的是 TrapContext 中内核预先写好的 kernel_satp（元数据区），不是刚才保存的用户现场，因此需要“读”而不是“写”。
    # load trap_handler into t1
    ld t1, 36*8(sp)
    # move to kernel_sp
    ld sp, 35*8(sp)
    # switch to kernel space
    csrw satp, t0                                                               # yifan 2026/5/10: 将 t0 写入 satp；satp 决定当前页表与分页模式，结合第 35 行载入的 kernel_satp，此处完成地址空间切换到内核页表。
    sfence.vma                                                                  # yifan 2026/5/10: 使用 sfence.vma 刷新快表，这就切换到了内核地址空间；
    # jump to trap_handler
    jr t1                                                                       # yifan 2026/5/10: jr 为寄存器间接跳转(等价 jalr x0, 0(t1))，只跳转不保存返回地址；此处跳到 t1 指向的 trap_handler 入口执行内核 trap 处理逻辑。

__restore:
    # a0: *TrapContext in user space(Constant); a1: user space token
    # switch to user space
    csrw satp, a1
    sfence.vma
    csrw sscratch, a0
    mv sp, a0
    # now sp points to TrapContext in user space, start restoring based on it
    # restore sstatus/sepc
    ld t0, 32*8(sp)
    ld t1, 33*8(sp)
    csrw sstatus, t0
    csrw sepc, t1
    # restore general purpose registers except x0/sp/tp
    ld x1, 1*8(sp)
    ld x3, 3*8(sp)
    .set n, 5
    .rept 27
        LOAD_GP %n
        .set n, n+1
    .endr
    # back to user stack
    ld sp, 2*8(sp)
    sret

    .section .data
    # emergency stack for kernel trap
    # in order to print trap info even if the kernel stack is corrupted.
__emergency:
    .align 4
    .space 1024 * 4
__emergency_end:


    .section .text                  # yifan 2026/5/12: 切换到代码段，下面开始放置内核 trap 入口相关指令。
    .globl __trap_from_kernel       # yifan 2026/5/12: 导出 __trap_from_kernel 符号，供 Rust 侧 extern 引用并写入 stvec。
    # 2^2=4 bytes aligned for stvec
    .align 2                        # yifan 2026/5/12: 将入口按 4 字节对齐，满足 stvec 入口与指令对齐要求。
__trap_from_kernel:                 # yifan 2026/5/12: 内核态发生 trap 时的汇编入口标签。
    la sp, __emergency_end          # yifan 2026/5/12: 将 sp 切到紧急栈顶，避免原内核栈损坏导致无法继续处理。
    j trap_from_kernel              # yifan 2026/5/12: 无条件跳转到 Rust 的 trap_from_kernel 执行统一 trap 处理逻辑。
