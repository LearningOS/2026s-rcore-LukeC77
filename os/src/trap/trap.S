.altmacro           # yifan 2026/5/2: enable GAS alternate macro syntax (supports forms like \n substitution).
.macro SAVE_GP n    # yifan 2026/5/2: define macro SAVE_GP with parameter n (general-purpose register index).
    sd x\n, \n*8(sp)    # yifan 2026/5/2: macro body template: store x<n> to stack slot at offset n*8 from sp.
.endm               # yifan 2026/5/2: end of SAVE_GP macro definition.
.macro LOAD_GP n    # yifan 2026/5/2: define macro LOAD_GP; parameter n is the register index.
    ld x\n, \n*8(sp)    # yifan 2026/5/2: load from stack offset n*8 into x<n>, used to restore GPRs.
.endm
    .section .text    # yifan 2026/5/2: place following instructions in the executable text section.
    .globl __alltraps    # yifan 2026/5/2: export __alltraps as a global symbol for linker/Rust reference.
    .globl __restore    # yifan 2026/5/2: export __restore as a global symbol for context-restore entry.
    .align 2    # yifan 2026/5/2: align next code to 2^2 = 4-byte boundary.
__alltraps:    # yifan 2026/5/2: define the unified trap entry label where CPU jumps on traps.
    csrrw sp, sscratch, sp    # yifan 2026/5/2: atomically swap sp and sscratch; save user sp and switch to kernel sp.
    # now sp->kernel stack, sscratch->user stack
    # allocate a TrapContext on kernel stack
    addi sp, sp, -34*8 # TrapContext是34字节
    # save general-purpose registers
    sd x1, 1*8(sp)
    # skip sp(x2), we will save it later
    sd x3, 3*8(sp)
    # skip tp(x4), application does not use it
    # save x5~x31
    .set n, 5    # yifan 2026/5/2: initialize assembler-time variable n to 5, meaning the save loop starts from register x5.
    .rept 27    # yifan 2026/5/2: repeat the following block 27 times so SAVE_GP with n=n+1 saves x5 through x31.
        SAVE_GP %n
        .set n, n+1
    .endr
    # we can use t0/t1/t2 freely, because they were saved on kernel stack
    csrr t0, sstatus    # yifan 2026/5/2: read sstatus into t0 so privilege and interrupt-state bits can be saved for later restore.
    csrr t1, sepc    # yifan 2026/5/2: read sepc into t1 to preserve the trapped PC used by sret to return to the correct instruction.
    sd t0, 32*8(sp)
    sd t1, 33*8(sp)
    # read user stack from sscratch and save it on the kernel stack
    csrr t2, sscratch
    sd t2, 2*8(sp)
    # set input argument of trap_handler(cx: &mut TrapContext)
    mv a0, sp # yifan 2026/5/2: 把当前栈指针 sp 的值放进参数寄存器 a0，准备传给后面的函数。
    call trap_handler

__restore:
    # case1: start running app by __restore
    # case2: back to U after handling trap
    # yifan 2026/5/2: csrr t2, sscratch in __alltraps only reads sscratch and does not clear it.
    # yifan 2026/5/2: however, __restore is used both for returning after a trap and for starting/restoring another app.
    # yifan 2026/5/2: when switching contexts, the TrapContext pointed by a0/sp may belong to a different app.
    # yifan 2026/5/2: so we must reload user sp from TrapContext (ld t2, 2*8(sp)) and write it back to sscratch.
    # yifan 2026/5/2: this guarantees csrrw sp, sscratch, sp swaps to the correct current app user stack.
    mv sp, a0    # yifan 2026/5/2: switch sp to the TrapContext pointer in a0 so subsequent ld ... (sp) restores the intended context; this is an intentional stack-context switch, not an accidental overwrite.
    # now sp->kernel stack(after allocated), sscratch->user stack
    # restore sstatus/sepc
    ld t0, 32*8(sp)
    ld t1, 33*8(sp)
    ld t2, 2*8(sp)
    csrw sstatus, t0
    csrw sepc, t1
    csrw sscratch, t2
    # restore general-purpuse registers except sp/tp
    ld x1, 1*8(sp)
    ld x3, 3*8(sp)
    .set n, 5
    .rept 27
        LOAD_GP %n
        .set n, n+1
    .endr
    # release TrapContext on kernel stack
    addi sp, sp, 34*8
    # now sp->kernel stack, sscratch->user stack
    csrrw sp, sscratch, sp
    sret
