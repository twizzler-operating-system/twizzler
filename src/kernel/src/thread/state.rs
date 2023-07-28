#[derive(PartialEq, Copy, Clone, Debug)]
#[repr(u32)]
pub enum ThreadState {
    Starting,
    Running,
    Blocked,
    Exiting,
    Exited,
}
