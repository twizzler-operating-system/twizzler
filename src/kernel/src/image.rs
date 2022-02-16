use x86_64::VirtAddr;
use xmas_elf::program::{self};

use crate::once::Once;
static KERNEL_IMAGE: Once<&'static [u8]> = Once::new();

pub fn init(kernel_image: &'static [u8]) {
    KERNEL_IMAGE.call_once(|| kernel_image);
}

#[derive(Copy, Clone)]
pub struct TlsInfo {
    pub start_addr: VirtAddr,
    pub file_size: usize,
    pub mem_size: usize,
}
pub fn get_tls() -> TlsInfo {
    let elf = xmas_elf::ElfFile::new(*KERNEL_IMAGE.wait()).expect("failed to parse kernel image");
    for ph in elf.program_iter() {
        if let Ok(program::Type::Tls) = ph.get_type() {
            return TlsInfo {
                start_addr: VirtAddr::new(ph.virtual_addr()),
                file_size: ph.file_size() as usize,
                mem_size: ph.mem_size() as usize,
            };
        }
    }
    panic!("failed to find TLS program header in kernel image");
}
