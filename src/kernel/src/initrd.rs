use x86_64::VirtAddr;

pub struct BootModule {
    pub start: VirtAddr,
    pub length: usize,
}
