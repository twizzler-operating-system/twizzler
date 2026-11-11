use twizzler_abi::{device::CacheType, object::Protections};

use crate::{
    arch::address::PhysAddr,
    memory::pagetables::{MappingFlags, MappingSettings},
};

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Ord, Eq)]
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

    pub fn is_object_table(&self) -> bool {
        self.flags().contains(EntryFlags::OBJECT_TABLE)
    }

    pub fn set_object_table(&mut self, value: bool) {
        let mut flags = self.flags();
        flags.set(EntryFlags::OBJECT_TABLE, value);
        self.set_flags(flags);
    }

    pub(super) fn get_avail_bit(&self) -> bool {
        self.flags().contains(EntryFlags::AVAIL_1)
    }

    pub(super) fn set_avail_bit(&mut self, value: bool) {
        let mut flags = self.flags();
        flags.set(EntryFlags::AVAIL_1, value);
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
    pub fn addr(&self, _level: usize) -> PhysAddr {
        PhysAddr::new(self.0 & 0x000f_ffff_ffff_f000).unwrap()
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
        *self = Entry::new_internal(self.addr(0), flags);
    }

    /// Get the base address of the next page table.
    pub fn table_addr(&self) -> PhysAddr {
        self.addr(0)
    }
}

/// IA32_PAT, identical on every CPU. Entries 0-5 are what Limine hands the BSP (0-3 are also the
/// power-on values, 5 maps its framebuffer); 6 is the kernel's write-combining entry.
pub const PAT: u64 = 0x0001_0105_0007_0406;

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
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
        /// The PAT bit of a 4 KiB entry. Larger entries have it at bit 12, which is not
        /// representable here: write-combining is only encodable in 4 KiB entries.
        const PAT_4K = 1 << 7;
        const GLOBAL = 1 << 8;
        const AVAIL_1 = 1 << 9;
        const OBJECT_TABLE = 1 << 10;
        const WIRED = 1 << 11;
        /// Software bit. Bits 52-58 are available in every entry type on x86-64; 59-62 are
        /// protection keys, so this is the low end of the safe range.
        const PROBED = 1 << 52;
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
        if self.contains(EntryFlags::PROBED) {
            flags.insert(MappingFlags::PROBE);
        }
        flags
    }

    /// Get the represented permissions as a [Protections].
    pub fn perms(&self) -> Protections {
        let rw = if self.contains(Self::WRITE) {
            Protections::WRITE | Protections::READ
        } else {
            Protections::READ
        };
        let ex = if self.contains(Self::NO_EXECUTE) {
            Protections::empty()
        } else {
            Protections::EXEC
        };
        rw | ex
    }

    pub fn apply_perms(&mut self, perms: Protections) {
        self.set(Self::WRITE, perms.contains(Protections::WRITE));
        self.set(Self::NO_EXECUTE, !perms.contains(Protections::EXEC));
    }

    /// Retrieve the [CacheType].
    pub fn cache_type(&self) -> CacheType {
        if self.contains(Self::CACHE_DISABLE) {
            // No level needed: PCD without PWT is never written in a large entry.
            if self.contains(Self::PAT_4K) && !self.contains(Self::WRITE_THROUGH) {
                CacheType::WriteCombining
            } else {
                CacheType::Uncacheable
            }
        } else if self.contains(Self::WRITE_THROUGH) {
            CacheType::WriteThrough
        } else {
            CacheType::WriteBack
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

    /// Get the flags needed to indacate a leaf (i.e. page)
    pub fn leaf() -> Self {
        EntryFlags::empty()
    }
}

impl From<&MappingSettings> for EntryFlags {
    fn from(settings: &MappingSettings) -> Self {
        let c = match settings.cache() {
            CacheType::WriteBack => EntryFlags::empty(),
            CacheType::WriteThrough => EntryFlags::WRITE_THROUGH,
            // PAT entry 6. In a large entry bit 7 is PS instead, selecting entry 2 (UC-).
            CacheType::WriteCombining => EntryFlags::PAT_4K | EntryFlags::CACHE_DISABLE,
            // PCD|PWT is strong UC; PCD alone is only UC-.
            CacheType::Uncacheable | CacheType::MemoryMappedIO => {
                EntryFlags::CACHE_DISABLE | EntryFlags::WRITE_THROUGH
            }
        };
        let mut p = EntryFlags::empty();
        if settings.perms().contains(Protections::WRITE) {
            p |= EntryFlags::WRITE;
        }
        if !settings.perms().contains(Protections::EXEC) {
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
        let w = if settings.flags().contains(MappingFlags::WIRED) {
            EntryFlags::WIRED
        } else {
            EntryFlags::empty()
        };
        let pr = if settings.flags().contains(MappingFlags::PROBE) {
            EntryFlags::PROBED
        } else {
            EntryFlags::empty()
        };
        p | c | f | u | w | pr
    }
}

#[cfg(test)]
mod test {
    use alloc::boxed::Box;
    use core::sync::atomic::{AtomicUsize, Ordering};

    use twizzler_abi::device::CacheType;
    use twizzler_kernel_macros::kernel_test;

    use super::{EntryFlags, PAT};
    use crate::{
        interrupt::Destination,
        memory::pagetables::MappingSettings,
        processor::{ipi::ipi_exec, mp::with_each_active_processor},
    };

    #[kernel_test]
    fn test_cache_type_encoding() {
        use CacheType::*;
        // (type, PAT index of a 4 KiB entry, decoded, PAT index of a large entry, decoded)
        for (ct, idx, decoded, large_idx, large_decoded) in [
            (WriteBack, 0, WriteBack, 0, WriteBack),
            (WriteThrough, 1, WriteThrough, 1, WriteThrough),
            (WriteCombining, 6, WriteCombining, 2, WriteCombining),
            (Uncacheable, 3, Uncacheable, 3, Uncacheable),
            (MemoryMappedIO, 3, Uncacheable, 3, Uncacheable),
        ] {
            let pat_idx = |bits: u64, pat_bit: u32| {
                (bits >> pat_bit & 1) << 2 | (bits >> 4 & 1) << 1 | (bits >> 3 & 1)
            };
            let flags = EntryFlags::from(&MappingSettings::default_cachetype(ct));
            assert_eq!(pat_idx(flags.bits(), 7), idx);
            assert_eq!(flags.cache_type(), decoded);

            // What Table::map and Table::change build above the last level.
            let large = flags | EntryFlags::huge();
            assert_eq!(pat_idx(large.bits(), 12), large_idx);
            assert_eq!(large.settings().cache(), large_decoded);
        }
        let memtype = |idx: u64| PAT >> (idx * 8) & 0xff;
        assert_eq!(
            [0, 1, 2, 3, 6].map(memtype),
            [0x06, 0x04, 0x07, 0x00, 0x01],
            "WB, WT, UC-, UC, WC"
        );
    }

    #[kernel_test]
    fn test_pat_all_cpus() {
        static GOOD: AtomicUsize = AtomicUsize::new(0);
        GOOD.store(0, Ordering::SeqCst);
        ipi_exec(
            Destination::All,
            Box::new(|| {
                if unsafe { x86::msr::rdmsr(x86::msr::IA32_PAT) } == PAT {
                    GOOD.fetch_add(1, Ordering::SeqCst);
                }
            }),
            true,
        );
        let mut cpus = 0;
        with_each_active_processor(|_| cpus += 1);
        assert_eq!(GOOD.load(Ordering::SeqCst), cpus);
    }
}
