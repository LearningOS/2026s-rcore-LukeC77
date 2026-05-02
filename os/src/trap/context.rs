use riscv::register::sstatus::{self, Sstatus, SPP};
/// Trap Context
#[repr(C)]
pub struct TrapContext {
    /// general regs[0..31]
    pub x: [usize; 32],
    /// CSR sstatus      
    pub sstatus: Sstatus,
    /// CSR sepc
    pub sepc: usize,
}

impl TrapContext {
    /// set stack pointer to x_2 reg (sp)
    pub fn set_sp(&mut self, sp: usize) {
        self.x[2] = sp;
    }
    /// init app context
    pub fn app_init_context(entry: usize, sp: usize) -> Self {    // yifan 2026/5/2: build an initial TrapContext for first entering a user app.
        let mut sstatus = sstatus::read(); // CSR sstatus    // yifan 2026/5/2: read current sstatus as the base status value.
        sstatus.set_spp(SPP::User); //previous privilege mode: user mode    // yifan 2026/5/2: set SPP=User so sret will drop privilege from S-mode to U-mode.
        let mut cx = Self {    // yifan 2026/5/2: start constructing the context that __restore will load.
            x: [0; 32],    // yifan 2026/5/2: initialize all general-purpose registers to zero.
            sstatus,    // yifan 2026/5/2: store the prepared status with user-mode return target.
            sepc: entry, // entry point of app    // yifan 2026/5/2: set sepc to app entry so execution begins there after sret.
        };
        cx.set_sp(sp); // app's user stack pointer    // yifan 2026/5/2: write provided user stack pointer into x2(sp) slot.
        cx // return initial Trap Context of app    // yifan 2026/5/2: return this context for __restore to switch into the app.
    }
}
