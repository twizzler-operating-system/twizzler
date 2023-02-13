use crate::{
    arch::address::PhysAddr,
    memory::{
        context::MappingPerms,
        map::CacheType,
        pagetables::{MappingFlags, MappingSettings},
    },
};

#[derive(Debug, Clone, Copy)]
#[repr(transparent)]
/// The type of a single entry in a page table.
pub struct Entry(u64);

impl Entry {
    fn new_internal(addr: PhysAddr, flags: EntryFlags) -> Self {
        let addr: u64 = addr.into();
        Self(addr | flags.bits())
    }

    /// Construct a new _present_ [Entry] out of an address and flags.
    pub fn new(addr: PhysAddr, flags: EntryFlags) -> Self {
        Self::new_internal(addr, flags | EntryFlags::PRESENT)
    }

    /// Get the raw u64.
    pub fn raw(&self) -> u64 {
        self.0
    }

    /// Construct a new, unused [Entry].
    pub fn new_unused() -> Self {
        Self(0)
    }

    pub(super) fn get_avail_bit(&self) -> bool {
        self.flags().contains(EntryFlags::AVAIL_1)
    }

    pub(super) fn set_avail_bit(&mut self, value: bool) {
        let mut flags = self.flags();
        if value {
            flags.insert(EntryFlags::AVAIL_1);
        } else {
            flags.remove(EntryFlags::AVAIL_1);
        }
        self.set_flags(flags);
    }

    /// Is this a huge page, or a page table?
    pub fn is_huge(&self) -> bool {
        self.flags().contains(EntryFlags::HUGE_PAGE)
    }

    /// Is the entry mapped Present?
    pub fn is_present(&self) -> bool {
        self.flags().contains(EntryFlags::PRESENT)
    }

    /// Address contained in the [Entry].
    pub fn addr(&self) -> PhysAddr {
        PhysAddr::new(self.0 & 0x000fffff_fffff000).unwrap()
    }

    /// Set the address.
    pub fn set_addr(&mut self, addr: PhysAddr) {
        *self = Entry::new_internal(addr, self.flags());
    }

    /// Clear the entry.
    pub fn clear(&mut self) {
        let ab = self.get_avail_bit();
        self.0 = if ab { 1 << 9 } else { 0 };
    }

    /// Get the flags.
    pub fn flags(&self) -> EntryFlags {
        EntryFlags::from_bits_truncate(self.0)
    }

    /// Set the flags.
    pub fn set_flags(&mut self, flags: EntryFlags) {
        *self = Entry::new_internal(self.addr(), flags);
    }
}

bitflags::bitflags! {
    /// The possible flags in an X86 page table entry.
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
        const AVAIL_1 = 1 << 9;
        const NO_EXECUTE = 1 << 63;
    }
}

impl EntryFlags {
    /// Convert the flags to a [MappingSettings].
    pub fn settings(&self) -> MappingSettings {
        MappingSettings::new(self.perms(), self.cache_type(), self.flags())
    }

    /// Extract the [MappingFlags].
    pub fn flags(&self) -> MappingFlags {
        let mut flags = MappingFlags::empty();
        if self.contains(EntryFlags::GLOBAL) {
            flags.insert(MappingFlags::GLOBAL);
        }
        if self.contains(EntryFlags::USER) {
            flags.insert(MappingFlags::USER);
        }
        flags
    }

    /// Get the represented permissions as a [MappingPerms].
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

    /// Retrieve the [CacheType].
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

    /// Get the set of flags to use for an intermediate (page table) entry.
    pub fn intermediate() -> Self {
        Self::USER | Self::WRITE | Self::PRESENT
    }

    /// Get the flags needed to indicate a huge page.
    pub fn huge() -> Self {
        Self::HUGE_PAGE
    }
}

impl From<&MappingSettings> for EntryFlags {
    fn from(settings: &MappingSettings) -> Self {
        let c = match settings.cache() {
            CacheType::WriteBack => EntryFlags::empty(),
            CacheType::WriteThrough => EntryFlags::WRITE_THROUGH,
            CacheType::WriteCombining => EntryFlags::empty(),
            CacheType::Uncacheable => EntryFlags::CACHE_DISABLE,
        };
        let mut p = EntryFlags::empty();
        if settings.perms().contains(MappingPerms::WRITE) {
            p |= EntryFlags::WRITE;
        }
        if !settings.perms().contains(MappingPerms::EXECUTE) {
            p |= EntryFlags::NO_EXECUTE;
        }
        let f = if settings.flags().contains(MappingFlags::GLOBAL) {
            EntryFlags::GLOBAL
        } else {
            EntryFlags::empty()
        };
        let u = if settings.flags().contains(MappingFlags::USER) {
            EntryFlags::USER
        } else {
            EntryFlags::empty()
        };
        p | c | f | u
    }
}
