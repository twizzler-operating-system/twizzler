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
}
