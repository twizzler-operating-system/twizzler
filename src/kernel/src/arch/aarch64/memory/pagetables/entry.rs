use twizzler_abi::{device::CacheType, object::Protections};

use crate::{
    arch::address::PhysAddr,
    memory::pagetables::{MappingFlags, MappingSettings},
};

use super::mair::{memory_attr_manager, AttributeIndex};

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Ord, Eq)]
#[repr(transparent)]
/// The type of a single entry in a page table.
///
/// Page table entries in aarch64 nomenclature are known
/// as translation table descriptors. The descriptors themselves
/// can be different types, and mean different things depending
/// on what level we are in. It can also vary depending on
/// the size of the physical address space used (e.g., 48-bit)
pub struct Entry(u64);

impl Entry {
    fn new_internal(addr: PhysAddr, flags: EntryFlags) -> Self {
        Self(addr.raw() | flags.bits())
    }

    /// Construct a new _present_ [Entry] out of an address and flags.
    pub fn new(addr: PhysAddr, flags: EntryFlags) -> Self {
        // Table descriptor bits [11:2] are ignored
        // so it is safe to always add these flags
        Self::new_internal(
            addr,
            flags
            | EntryFlags::PRESENT
            // NOTE: device memory will ignore these sharability
            // attributes, so it is safe to always enable them
            | EntryFlags::SH1_SHAREABLE
            | EntryFlags::SH0_INNER_OR_OUTER
            // TODO: access flag is managed by software on base
            // ARMv8-A. This only works since these entries were
            // already present before when switching from bootstrap
            // mappings.
            | EntryFlags::ACCESS,
        )
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
        todo!("get_avail_bit")
    }

    pub(super) fn set_avail_bit(&mut self, _value: bool) {
        todo!("set_avail_bit")
    }

    /// Is this a huge page, or a page table?
    pub fn is_huge(&self) -> bool {
        // The meaning of this bit is only valid at levels != 3
        // If this bit is set then this entry points to another
        // page table. If this bit is set at level 3, then we are
        // looking at a page
        !self.flags().contains(EntryFlags::TABLE_OR_HUGE_PAGE)
    }

    /// Is the entry mapped Present?
    pub fn is_present(&self) -> bool {
        // if the last bit is 0, then this entry is invalid
        self.flags().contains(EntryFlags::PRESENT)
    }

    // bits [47:30]
    const LVL1_BLK_ADDR_MASK: u64 = 0x0000_FFFF_C000_0000;
    // bits [47:21]
    const LVL2_BLK_ADDR_MASK: u64 = 0x0000_FFFF_FFE0_0000;
    // bits [47:12]
    const LVL3_PAGE_ADDR_MASK: u64 = 0x0000_FFFF_FFFF_F000;

    /// Address contained in the [Entry].
    pub fn addr(&self, level: usize) -> PhysAddr {
        // The bits that indicate the address depends on
        // the translation granule used and the descriptor
        // type which depends on the level. For now we are
        // assuming a 4KiB translation granule.
        //
        // we assume the user wants the address of a page given
        // the level the entry is currently in
        match level {
            1 => PhysAddr::new(self.0 & Self::LVL1_BLK_ADDR_MASK).unwrap(),
            2 => PhysAddr::new(self.0 & Self::LVL2_BLK_ADDR_MASK).unwrap(),
            3 => PhysAddr::new(self.0 & Self::LVL3_PAGE_ADDR_MASK).unwrap(),
            // this is used when changing/unmapping entries
            0 => self.table_addr(),
            _ => todo!("getting the address from this level: {}", level),
        }
    }

    /// Set the address.
    pub fn set_addr(&mut self, _addr: PhysAddr) {
        todo!("setting the address on aarch64 depends on the level")
    }

    /// Clear the entry.
    pub fn clear(&mut self) {
        todo!("clear")
    }

    /// Get the flags.
    pub fn flags(&self) -> EntryFlags {
        EntryFlags::from_bits_truncate(self.0)
    }

    /// Set the flags.
    pub fn set_flags(&mut self, flags: EntryFlags) {
        // todo!("setting the flags on aarch64 depends on the level")
        // does it tho? I think for some yes, but not all ...
        // depends on the level, since this creates a new address
        // TODO: setting the address depends on the level.
        // For now we assume that we are setting the address for a leaf

        // for now assume that we do a leaf
        *self = Entry::new_internal(self.addr(super::Table::last_level()), flags);
    }

    const NEXT_LVL_TABLE_ADDR_MASK: u64 = 0x0000_FFFF_FFFF_F000;

    /// Get the base address of the next page table.
    pub fn table_addr(&self) -> PhysAddr {
        // aarch64 next level table address bits [47:12]
        // bits [47:12] map to table address [47:12]
        // this is true for a 4 KiB translation granule
        // with 48-bit addressing
        PhysAddr::new(self.0 & Self::NEXT_LVL_TABLE_ADDR_MASK).unwrap()
    }
}

bitflags::bitflags! {
    /// The possible flags in an AArch64 page table entry.
    pub struct EntryFlags: u64 {
        /// Indicates if the entry is valid
        const PRESENT = 1 << 0;
        /// Indicates if this entry is a Table/Huge Page at a given level.
        const TABLE_OR_HUGE_PAGE = 1 << 1;

        // Here we are assuming bit flags that corrspond to the upper/lower
        // attributes found in a block/page descriptor in a stage 1 translation.

        // Lower Attributes

        // AttrIndx[2:0] (deals with cache type)
        //
        // Since the bitflags type only maps to a single flag
        // and not a range (bits [4:2]), we encode each bit from
        // AttrIndex so that its value is saved when calling
        // `from_bits_truncate`

        /// AttrIndx bit 0.
        const ATTR_INDX_0 =  1 << 2;
        /// AttrIndx bit 1.
        const ATTR_INDX_1 =  1 << 3;
        /// AttrIndx bit 2.
        const ATTR_INDX_2 =  1 << 4;

        /// The output address of a descriptor is to non-secure memory.
        const NS = 1 << 5;

        // [7:6] => AP[2:1]
        //   - data Access Permissions bits (AP[2:1]).
        //   - AP[2]: read only / read/write access
        //   - AP[1]: EL0/app control or priviledged exception level
        //   - AP[1]=0, no data access at EL0; AP[1]=1, EL0 access with AP[2] permissions

        /// Access permission bit 1: User accessible/kernel only.
        const AP1_USER_OR_KERNEL = 1 << 6;
        /// Access permission bit 2: Read only or read-write permission.
        const AP2_READ_OR_RW = 1 << 7;

        // [9:8] =>
        //   - Shareability Field(SH[1:0]): if the Effective value of TCR_Elx.DS is 0.
        //   - Output Address(OA[51:50]): if the Effective value of TCR_Elx.DS is 1.

        /// Shareability attribute bit 0: Inner or Outer shareable
        const SH0_INNER_OR_OUTER = 1 << 8;
        /// Shareability attribute bit 1: Shareable or non-shareable/reserved
        const SH1_SHAREABLE = 1 << 9;

        // [10] => AF
        //   - access flag, memory region accessed since last set to 0
        //   - descriptors with AF set to 0 cannot be cached in TLB
        //   - either managed by hw or sw, depending on FEAT_HAFDBS

        /// Indicates if memory has been accessed since last set to 0.
        /// The flag might be managed by either hardware or software.
        const ACCESS = 1 << 10;

        // [11] => nG
        //   - not global bit (nG).
        //   - for translations that use ASID
        /// Indicates if the mapping is not global.
        const NOT_GLOBAL = 1 << 11;

        // [15:12] => OA (block descriptor bits)
        //   - RES0 if FEAT_LPA is not implemented
        // [16] => nT
        //   - If FEAT_BBM is not implemented
        //   - when changing block size accesses do not break coherency

        // Upper Attributes

        // [50] => GP
        //   - If FEAT_BTI is implemented, then Gaurd page for stage 1
        // [51] => DBM
        //   - RES0 if FEAT_HAFDBS is not implemented.
        //   - Dirty Bit Modifier. Hw managed dirty state
        // [52] => Contiguous
        //   - descr. belongs to group of adj entries that point to contig OA

        // [53] => PXN
        //   - RES0 for a translation regime that cannot apply to execution at EL0.
        //   - priviledged execute never
        /// PXN bit: Priviledged execute-never.
        const KERNEL_NO_EXECUTE = 1 << 53;
        // [54] => UXN or XN
        //   - UXN for a translation regime that can apply to execution at EL0, otherwise XN.
        /// UXN bit: Unpriviledged execute-never.
        const USER_NO_EXECUTE = 1 << 54;

        // [58:55] => Ignored/Reserved for software use
        // [62:59] => PBHA
        //   - IGNORED if FEAT_HPDS2 is not implemented
        //   - Page based hardware attributes
        // [63] => Ignored
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
        // TODO: do we need to check if we are using ASIDs?
        if !self.contains(EntryFlags::NOT_GLOBAL) {
            flags.insert(MappingFlags::GLOBAL);
        }
        if self.contains(EntryFlags::AP1_USER_OR_KERNEL) {
            flags.insert(MappingFlags::USER);
        }
        flags
    }

    /// Get the represented permissions as a [Protections].
    pub fn perms(&self) -> Protections {
        let rw = if self.contains(Self::AP2_READ_OR_RW) {
            Protections::READ
        } else {
            Protections::WRITE | Protections::READ
        };
        // TODO: decide on more sophisitcated execution permissions
        let ex = if self.contains(Self::KERNEL_NO_EXECUTE) || self.contains(Self::USER_NO_EXECUTE) {
            Protections::empty()
        } else {
            Protections::EXEC
        };
        rw | ex
    }

    /// Retrieve the [CacheType].
    pub fn cache_type(&self) -> CacheType {
        // The cache type depends on the type of memory
        // assigned to this entry which is indicated by
        // the AttrIndex field. This is used to index into the
        // MAIR_EL1 register

        // Get the attribute index of the entry
        // bits [4:2] => AttrIndex[2:0]
        let index: AttributeIndex = ((self.bits() >> 2) & 0b111) as AttributeIndex;

        // get the attribute based on the index
        let attr = memory_attr_manager()
            .read_entry(index)
            .expect("invalid attribute index");

        // match the attribute to the cache type
        CacheType::from(attr)
    }

    /// Get the set of flags to use for an intermediate (page table) entry.
    pub fn intermediate() -> Self {
        // we want the table to be: readable, writeable, valid,
        // marked as a table descriptor, and be kernel-only
        //
        // NOTE: not setting AP1_USER_OR_KERNEL/AP2_READ_OR_RW
        // means that AP[2:1] = 0, so the mapping is kernel only with
        // read/write access
        Self::PRESENT | Self::TABLE_OR_HUGE_PAGE
    }

    /// Get the flags needed to indicate a huge page.
    pub fn huge() -> Self {
        // huge pages are indicated by the absence of
        // the TABLE_OR_HUGE_PAGE bit flag
        EntryFlags::empty()
    }

    /// Get the flags needed to indacate a leaf (i.e. page)
    pub fn leaf() -> Self {
        // If this bit is set at level 3, then we are
        // looking at a page.
        Self::TABLE_OR_HUGE_PAGE
    }
}

impl From<CacheType> for EntryFlags {
    fn from(cache: CacheType) -> Self {
        // unsupported cache types result in `EntryFlags::empty()`
        // in this, it defaults to normal cacheble memory (index 0)
        let attr_idx = memory_attr_manager().attribute_index(cache).unwrap_or(0);

        // TODO: should we try to update the requested mapping in MAIR?

        // convert the numerical index to a set of flags
        // AttrIndx[2:0] is mapped to EntryFlags bits [4:2]
        EntryFlags::from_bits_truncate((attr_idx << 2) as u64)
    }
}

impl From<&MappingSettings> for EntryFlags {
    fn from(settings: &MappingSettings) -> Self {
        // here 0/EntryFlags::empty() is a valid memory type
        // so even if we do not support a certain type of memory,
        // it gets set as the default (WriteBack)
        let c = EntryFlags::from(settings.cache());

        let mut p = EntryFlags::empty();
        if !settings.perms().contains(Protections::WRITE) {
            // set this flag if we only want read-only permissions
            // in other words, do not set if we desire write permissions
            p |= EntryFlags::AP2_READ_OR_RW;
        }
        if !settings.perms().contains(Protections::EXEC) {
            p |= EntryFlags::KERNEL_NO_EXECUTE | EntryFlags::USER_NO_EXECUTE;
        }
        let f = if settings.flags().contains(MappingFlags::GLOBAL) {
            // pages are global if we do not set this flag
            EntryFlags::empty()
        } else {
            EntryFlags::NOT_GLOBAL
        };
        let u = if settings.flags().contains(MappingFlags::USER) {
            EntryFlags::AP1_USER_OR_KERNEL
        } else {
            EntryFlags::empty()
        };
        p | c | f | u
    }
}
