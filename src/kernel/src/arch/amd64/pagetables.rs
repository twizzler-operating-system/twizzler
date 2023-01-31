use crate::memory::{context::MappingPerms, map::CacheType, pagetables::Table};

use super::address::PhysAddr;

#[derive(Debug, Clone, Copy)]
#[repr(transparent)]
pub struct Entry(u64);

pub const PAGE_TABLE_ENTRIES: usize = 512;

impl Entry {
    pub fn new(addr: PhysAddr, flags: EntryFlags) -> Self {
        let addr: u64 = addr.into();
        Self(addr | flags.bits())
    }

    pub fn new_unused() -> Self {
        Self(0)
    }

    pub fn get_avail_bit(&self) -> bool {
        self.flags().contains(EntryFlags::AVAIL_1)
    }

    pub fn set_avail_bit(&mut self, value: bool) {
        let mut flags = self.flags();
        if value {
            flags.insert(EntryFlags::AVAIL_1);
        } else {
            flags.remove(EntryFlags::AVAIL_1);
        }
        self.set_flags(flags);
    }

    pub fn is_unused(&self) -> bool {
        self.0 & !(EntryFlags::AVAIL_1.bits()) == 0
    }

    pub fn is_huge(&self) -> bool {
        self.flags().contains(EntryFlags::HUGE_PAGE)
    }

    pub fn is_present(&self) -> bool {
        self.flags().contains(EntryFlags::PRESENT)
    }

    pub fn is_global(&self) -> bool {
        self.flags().contains(EntryFlags::GLOBAL)
    }

    pub fn addr(&self) -> PhysAddr {
        PhysAddr::new(self.0 & 0x000fffff_fffff000).unwrap()
    }

    pub fn set_addr(&mut self, addr: PhysAddr) {
        *self = Entry::new(addr, self.flags());
    }

    pub fn set_unused(&mut self) {
        let ab = self.get_avail_bit();
        self.0 = if ab { 1 << 9 } else { 0 };
    }

    pub fn flags(&self) -> EntryFlags {
        EntryFlags::from_bits_truncate(self.0)
    }

    pub fn set_flags(&mut self, flags: EntryFlags) {
        *self = Entry::new(self.addr(), flags);
    }
}

bitflags::bitflags! {
    pub struct EntryFlags: u64 {
        const PRESENT = 1 << 0;
        const WRITE = 1 << 1;
        const USER = 1 << 2;
        const WRITE_THROUGH = 1 << 3;
        const CACHE_DISABLE = 1 << 4;
        const ACCESSED = 1 << 5;
        const DIRTY = 1 << 6;
        const HUGE_PAGE = 1 << 7;
        const GLOBAL = 1 << 8;
        const AVAIL_1 = 1 << 8;
        const NO_EXECUTE = 1 << 63;
    }
}

impl EntryFlags {
    pub fn new(perms: MappingPerms, cache: CacheType) -> Self {
        let c = match cache {
            CacheType::WriteBack => EntryFlags::empty(),
            CacheType::WriteThrough => EntryFlags::WRITE_THROUGH,
            CacheType::WriteCombining => EntryFlags::empty(),
            CacheType::Uncacheable => EntryFlags::CACHE_DISABLE,
        };
        let mut p = EntryFlags::empty();
        if perms.contains(MappingPerms::WRITE) {
            p |= EntryFlags::WRITE;
        }
        if !perms.contains(MappingPerms::EXECUTE) {
            p |= EntryFlags::NO_EXECUTE;
        }
        p | c
    }

    pub fn perms(&self) -> MappingPerms {
        let rw = if self.contains(Self::WRITE) {
            MappingPerms::WRITE | MappingPerms::READ
        } else {
            MappingPerms::READ
        };
        let ex = if self.contains(Self::NO_EXECUTE) {
            MappingPerms::empty()
        } else {
            MappingPerms::EXECUTE
        };
        rw | ex
    }

    pub fn cache_type(&self) -> CacheType {
        if self.contains(Self::CACHE_DISABLE) {
            CacheType::Uncacheable
        } else {
            if self.contains(Self::WRITE_THROUGH) {
                CacheType::WriteThrough
            } else {
                CacheType::WriteBack
            }
        }
    }

    pub fn intermediate() -> Self {
        Self::USER | Self::WRITE | Self::PRESENT
    }
}

impl Table {
    pub fn can_map_at_level(level: usize) -> bool {
        match level {
            0 => true,
            1 => true,
            // TODO: check cpuid
            2 => true,
            _ => false,
        }
    }
}
