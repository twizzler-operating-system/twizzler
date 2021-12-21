use x86_64::VirtAddr;

pub struct BootModule {
    pub start: VirtAddr,
    pub length: usize,
}

impl BootModule {
    fn as_slice(&self) -> &[u8] {
        let p = self.start.as_ptr();
        unsafe { core::slice::from_raw_parts(p, self.length) }
    }
}

pub fn init(modules: &[BootModule]) {
    for module in modules {
        let tar = tar_no_std::TarArchiveRef::new(module.as_slice());
        for e in tar.entries() {
            //logln!("{:?}", e.filename());
        }
    }
}
