//! batch subsystem

use crate::sync::UPSafeCell;
use crate::trap::TrapContext;
use core::arch::asm;
use lazy_static::*;

const USER_STACK_SIZE: usize = 4096 * 2;
const KERNEL_STACK_SIZE: usize = 4096 * 2;
const MAX_APP_NUM: usize = 16;
const APP_BASE_ADDRESS: usize = 0x80400000;
const APP_SIZE_LIMIT: usize = 0x20000;

#[repr(align(4096))]
struct KernelStack {
    data: [u8; KERNEL_STACK_SIZE],
}

#[repr(align(4096))]
struct UserStack {
    data: [u8; USER_STACK_SIZE],
}

static KERNEL_STACK: KernelStack = KernelStack {
    data: [0; KERNEL_STACK_SIZE],
};
static USER_STACK: UserStack = UserStack {
    data: [0; USER_STACK_SIZE],
};

impl KernelStack {
    fn get_sp(&self) -> usize {
        // Yifan 2026/5/1: returns the initial stack top of this region (empty stack),
        // not the current runtime CPU sp register value.
        self.data.as_ptr() as usize + KERNEL_STACK_SIZE
    }
    pub fn push_context(&self, cx: TrapContext) -> &'static mut TrapContext {
        let cx_ptr = (self.get_sp() - core::mem::size_of::<TrapContext>()) as *mut TrapContext;
        unsafe {
            *cx_ptr = cx;
        }
        unsafe { cx_ptr.as_mut().unwrap() }
    }
}

impl UserStack {
    fn get_sp(&self) -> usize {
        // Yifan 2026/5/1: returns the initial stack top of this region (empty stack),
        // not the current runtime CPU sp register value.
        self.data.as_ptr() as usize + USER_STACK_SIZE
    }
}

struct AppManager {
    num_app: usize,
    current_app: usize,
    app_start: [usize; MAX_APP_NUM + 1],
}

impl AppManager {
    pub fn print_app_info(&self) {
        println!("[kernel] num_app = {}", self.num_app);
        for i in 0..self.num_app {
            println!(
                "[kernel] app_{} [{:#x}, {:#x})",
                i,
                self.app_start[i],
                self.app_start[i + 1]
            );
        }
    }

    unsafe fn load_app(&self, app_id: usize) {
        if app_id >= self.num_app {
            println!("All applications completed!");
            use crate::board::QEMUExit;
            crate::board::QEMU_EXIT_HANDLE.exit_success();
        }
        println!("[kernel] Loading app_{}", app_id);
        // clear app area
        core::slice::from_raw_parts_mut(APP_BASE_ADDRESS as *mut u8, APP_SIZE_LIMIT).fill(0);
        let app_src = core::slice::from_raw_parts(
            self.app_start[app_id] as *const u8,
            self.app_start[app_id + 1] - self.app_start[app_id],
        );
        let app_dst = core::slice::from_raw_parts_mut(APP_BASE_ADDRESS as *mut u8, app_src.len());
        app_dst.copy_from_slice(app_src);
        // Memory fence about fetching the instruction memory
        // It is guaranteed that a subsequent instruction fetch must
        // observes all previous writes to the instruction memory.
        // Therefore, fence.i must be executed after we have loaded
        // the code of the next app into the instruction memory.
        // See also: riscv non-priv spec chapter 3, 'Zifencei' extension.
        asm!("fence.i"); // 保证 在它之后的取指过程必须能够看到在它之前的所有对于取指内存区域的修改 
    }

    pub fn get_current_app(&self) -> usize {
        self.current_app
    }

    pub fn move_to_next_app(&mut self) {
        self.current_app += 1;
    }
}

lazy_static! { // 用宏定义“延迟初始化的全局静态变量”。
    // static ref 是 lazy_static! 宏提供的语法，不是 Rust 原生关键字组合。
    // 定义一个全局静态变量，但它不是程序启动时立刻初始化，而是在第一次使用时才初始化。
    static ref APP_MANAGER: UPSafeCell<AppManager> = unsafe { 
        UPSafeCell::new({
            extern "C" { // 声明外部符号（来自汇编/链接产物）。
                fn _num_app(); // 声明 _num_app 符号。这里把它当“可取地址的符号”用，不是真的要调用逻辑函数。
            } // 结束 extern 声明块。
            let num_app_ptr = _num_app as usize as *const usize; // 把 _num_app 符号地址转成 *const usize 指针。此地址指向 app 信息表开头。
            let num_app = num_app_ptr.read_volatile(); // 从表头读第一个 usize，得到应用数量 num_app。volatile 表示按“易失读取”执行，不被优化掉。
            let mut app_start: [usize; MAX_APP_NUM + 1] = [0; MAX_APP_NUM + 1]; // 在栈上准备一个固定大小数组，先全 0，用来存每个 app 的边界地址。
            let app_start_raw: &[usize] = // 定义一个切片变量，准备绑定“原始地址表视图”。
                core::slice::from_raw_parts(num_app_ptr.add(1), num_app + 1); // 从 num_app_ptr 后一个元素开始（跳过数量字段），构造长度 num_app+1 的 &[usize]，即边界地址表。
            app_start[..=num_app].copy_from_slice(app_start_raw); // 把原始地址表复制到本地数组前 num_app+1 个位置。
            AppManager {
                num_app,
                current_app: 0,
                app_start, // 填入刚复制好的边界地址数组。
            }
        })
    };
}

/// init batch subsystem
pub fn init() {
    print_app_info();
}

/// print apps info
pub fn print_app_info() {
    APP_MANAGER.exclusive_access().print_app_info();
}

/// run next app
pub fn run_next_app() -> ! {
    let mut app_manager = APP_MANAGER.exclusive_access();
    let current_app = app_manager.get_current_app();
    unsafe {
        app_manager.load_app(current_app);
    }
    app_manager.move_to_next_app();
    drop(app_manager);
    // before this we have to drop local variables related to resources manually
    // and release the resources
    extern "C" {
        fn __restore(cx_addr: usize);
    }
    unsafe {
        __restore(KERNEL_STACK.push_context(TrapContext::app_init_context(
            APP_BASE_ADDRESS,
            USER_STACK.get_sp(),
        )) as *const _ as usize);
    }
    panic!("Unreachable in batch::run_current_app!");
}
