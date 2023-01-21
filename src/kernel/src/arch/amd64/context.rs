use x86::controlregs::Cr4;

use crate::{
    memory::{map::Mapping, pagetables::Mapper},
    mutex::Mutex,
};

use super::address::VirtAddr;

pub struct ArchContextInner {
    mapper: Mapper,
    tlb_mgr: TlbMgr,
}

pub struct ArchContext {
    inner: Mutex<ArchContextInner>,
}

impl ArchContext {
    pub fn map(&self, mapping: &Mapping) {
        self.inner.lock().map(mapping);
    }
    pub fn unmap(&self, addr: VirtAddr, len: usize) {
        self.inner.lock().unmap(addr, len);
    }
}

impl ArchContextInner {
    fn map(&mut self, mapping: &Mapping) {
        self.mapper.map(mapping);
    }

    fn unmap(&mut self, addr: VirtAddr, len: usize) {
        self.mapper.unmap(addr, len);
    }
}

struct TlbMgr {}

impl TlbMgr {
    fn tlb_non_global_inv() {
        unsafe {
            core::arch::asm!(
                "mov %cr3, %rax
                  mov %rax, %cr3",
                "rax",
                "volatile"
            )
        }
    }

    fn tlb_global_inv() {
        unsafe {
            let cr4 = x86::controlregs::cr4();
            if cr4.contains(Cr4::CR4_ENABLE_GLOBAL_PAGES) {
                let cr4_without_pge = cr4 & !Cr4::CR4_ENABLE_GLOBAL_PAGES;
                x86::controlregs::cr4_write(cr4_without_pge);
                x86::controlregs::cr4_write(cr4);
            } else {
                Self::tlb_non_global_inv();
            }
        }
    }
}

struct TlbInvData {
    target_cr3: u64,
    instructions: [InvInstruction; 16],
    vpid: u16,
    len: u8,
    flags: u8,
}

#[repr(transparent)]
struct InvInstruction(u64);

impl InvInstruction {
    fn new(addr: VirtAddr, is_global: bool, is_terminal: bool, level: u8) -> Self {
        let addr: u64 = addr.into();
        let val = addr
            | if is_global { 1 << 0 } else { 0 }
            | if is_terminal { 1 << 1 } else { 0 }
            | (level as u64) << 2;
        Self(val)
    }

    fn addr(&self) -> VirtAddr {
        let val = self.0 & 0xfffffffffffff000;
        val.try_into().unwrap()
    }

    fn is_global(&self) -> bool {
        self.0 & 1 != 0
    }

    fn is_terminal(&self) -> bool {
        self.0 & 2 != 0
    }

    fn level(&self) -> u8 {
        (self.0 >> 2 & 0xff) as u8
    }
}
