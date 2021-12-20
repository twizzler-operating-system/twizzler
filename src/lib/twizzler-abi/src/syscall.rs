#[derive(Copy, Clone, Debug)]
#[repr(u64)]
pub enum Syscall {
    Null,
    KernelConsoleIO,
    ThreadSync,
    ThreadCtrl,
}

impl Syscall {
    pub fn num(&self) -> u64 {
        *self as u64
    }
}


