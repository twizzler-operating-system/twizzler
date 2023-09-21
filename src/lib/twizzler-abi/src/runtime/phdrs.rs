#![allow(dead_code)]

use crate::object::ObjID;

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

static mut EXEC_ID: ObjID = ObjID::new(0);
static mut PHDR_INFO: Option<&'static [Phdr]> = None;

// TODO: this is a hack
pub(crate) unsafe fn get_exec_id() -> Option<ObjID> {
    let id = EXEC_ID;
    if id == 0.into() {
        None
    } else {
        Some(id)
    }
}

// TODO: this is a hack
pub(crate) fn get_load_seg(nr: usize) -> Option<(usize, usize)> {
    if let Some(phdrs) = unsafe { PHDR_INFO } {
        if nr < phdrs.len() {
            Some((phdrs[nr].vaddr as usize, phdrs[nr].memsz as usize))
        } else {
            None
        }
    } else {
        None
    }
}

#[allow(unreachable_code)]
#[allow(unused_variables)]
#[allow(unused_mut)]
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
