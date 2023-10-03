use super::tls::{set_tls_info, TlsInfo};

#[repr(C)]
pub struct Phdr {
    ty: u32,
    flags: u32,
    off: u64,
    vaddr: u64,
    paddr: u64,
    filesz: u64,
    memsz: u64,
    align: u64,
}

static mut PHDR_INFO: Option<&'static [Phdr]> = None;

// Called during runtime init to work through phdrs.
pub fn process_phdrs(phdrs: &'static [Phdr]) {
    for ph in phdrs {
        if ph.ty == 7 {
            set_tls_info(TlsInfo {
                template_start: ph.vaddr as *const u8,
                memsz: ph.memsz as usize,
                filsz: ph.filesz as usize,
                align: ph.align as usize,
            })
        }
    }
    unsafe {
        PHDR_INFO = Some(phdrs);
    }
}
