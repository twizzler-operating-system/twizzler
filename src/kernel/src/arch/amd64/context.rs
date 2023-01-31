use x86::controlregs::Cr4;

use crate::{
    memory::{
        context::MappingPerms,
        map::{CacheType, Mapping},
        pagetables::{Mapper, MappingCursor, PhysAddrProvider},
    },
    mutex::Mutex,
};

use super::address::{PhysAddr, VirtAddr};

pub struct ArchContextInner {
    mapper: Mapper,
}

pub struct ArchContext {
    inner: Mutex<ArchContextInner>,
}

impl ArchContext {
    pub fn map(
        &self,
        cursor: MappingCursor,
        phys: &mut impl PhysAddrProvider,
        perms: MappingPerms,
        cache: CacheType,
    ) {
        self.inner.lock().map(cursor, phys, perms, cache);
    }

    pub fn unmap(&self, cursor: MappingCursor) {
        self.inner.lock().unmap(cursor);
    }
}

impl ArchContextInner {
    fn map(
        &mut self,
        cursor: MappingCursor,
        phys: &mut impl PhysAddrProvider,
        perms: MappingPerms,
        cache: CacheType,
    ) {
        self.mapper.map(cursor, phys, perms, cache);
    }

    fn unmap(&mut self, cursor: MappingCursor) {
        self.mapper.unmap(cursor);
    }
}

const MAX_INVALIDATION_INSTRUCTIONS: usize = 16;
struct TlbInvData {
    target_cr3: u64,
    instructions: [InvInstruction; MAX_INVALIDATION_INSTRUCTIONS],
    len: u8,
    flags: u8,
}

fn tlb_non_global_inv() {
    unsafe {
        let x = x86::controlregs::cr3();
        x86::controlregs::cr3_write(x);
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
            tlb_non_global_inv();
        }
    }
}

impl TlbInvData {
    const GLOBAL: u8 = 1;
    const FULL: u8 = 2;

    fn set_global(&mut self) {
        self.flags |= Self::GLOBAL;
    }

    fn set_full(&mut self) {
        self.flags |= Self::FULL;
    }

    fn full(&self) -> bool {
        self.flags & Self::FULL != 0
    }

    fn global(&self) -> bool {
        self.flags & Self::GLOBAL != 0
    }

    fn target(&self) -> u64 {
        self.target_cr3
    }

    fn instructions(&self) -> &[InvInstruction] {
        &self.instructions[0..(self.len as usize)]
    }

    fn enqueue(&mut self, inst: InvInstruction) {
        if inst.is_global() {
            self.set_global();
        }

        if self.len as usize == MAX_INVALIDATION_INSTRUCTIONS {
            self.set_full();
            return;
        }

        self.instructions[self.len as usize] = inst;
        self.len += 1;
    }

    unsafe fn do_invalidation(&self) {
        let our_cr3 = x86::controlregs::cr3();
        if our_cr3 != self.target() && !self.global() {
            return;
        }

        if self.full() {
            if self.global() {
                tlb_global_inv();
            } else {
                tlb_non_global_inv();
            }
            return;
        }

        for inst in self.instructions() {
            inst.execute();
        }
    }
}

#[derive(Debug, Clone, Copy)]
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

    fn execute(&self) {
        let addr: u64 = self.addr().into();
        unsafe {
            core::arch::asm!("invlpg [{addr}]", addr = in(reg) addr);
        }
    }
}

#[derive(Default)]
pub struct ArchCacheLineMgr {}

impl ArchCacheLineMgr {
    pub fn flush(&self, line: VirtAddr) {
        let addr: u64 = line.into();
        unsafe {
            core::arch::asm!("clflush [{addr}]", addr = in(reg) addr);
        }
    }
}

pub struct ArchTlbMgr {
    data: TlbInvData,
}

impl ArchTlbMgr {
    pub fn new(target: PhysAddr) -> Self {
        Self {
            data: TlbInvData {
                target_cr3: target.into(),
                instructions: [InvInstruction::new(
                    unsafe { VirtAddr::new_unchecked(0) },
                    false,
                    false,
                    0,
                ); MAX_INVALIDATION_INSTRUCTIONS],
                len: 0,
                flags: 0,
            },
        }
    }

    pub fn enqueue(&mut self, addr: VirtAddr, is_global: bool, is_terminal: bool, level: usize) {
        self.data.enqueue(InvInstruction::new(
            addr,
            is_global,
            is_terminal,
            level as u8,
        ));
    }

    pub fn finish(&mut self) {
        unsafe {
            self.data.do_invalidation();
        }
        logln!("TODO: invalidation across all cpus");
    }
}
