/// An abstraction to manage state in the MAIR_EL1 system register.
///
/// The MAIR_EL1 register is responsible for storing the memory
/// attributes used by the page tables (cache type, device vs normal, etc.)
///
/// A description of the MAIR_EL1 register can be found in section
/// D17.2.97 of the "Arm Architecture Reference Manual"
use arm64::registers::MAIR_EL1;
use registers::{
    LocalRegisterCopy,
    interfaces::{Readable, Writeable},
    register_bitfields,
};
use twizzler_abi::device::CacheType;

use crate::once::Once;

// TODO: check the bounds of this
pub type AttributeIndex = u8;

#[derive(Copy, Clone, Debug)]
pub struct MemoryAttribute {
    attr: LocalRegisterCopy<u8, MEM_ATTR::Register>,
}

impl MemoryAttribute {
    fn new(attr: u8) -> Self {
        Self {
            attr: LocalRegisterCopy::new(attr),
        }
    }

    fn is_valid(&self) -> bool {
        let (outer, inner) = (self.raw() >> 4, self.raw() & 0xf);
        if outer == 0 {
            // Device memory: bits [1:0] must be zero.
            inner & 0b11 == 0
        } else {
            // Normal memory: an inner 0 is UNPREDICTABLE (or FEAT_XS-specific).
            inner != 0
        }
    }

    fn raw(&self) -> u8 {
        self.attr.get()
    }
}

#[derive(Debug)]
pub enum AttributeError {
    NoEntry,    // might not need
    Exists(u8), // could get around this
    Full,       // is needed
}

pub struct MemoryAttributeManager {
    mair: [MemoryAttribute; 8],
}

// TODO: in the future we might want a replace entry method

impl MemoryAttributeManager {
    /// Keeps the bootloader's attributes, which its tables (and so the kernel's copies of them)
    /// index, and adds each [`CacheType`]'s attribute that is missing in a spare slot: one
    /// repeating an earlier slot's value, which no table needs. Runs on the BSP before any
    /// secondary copies MAIR_EL1.
    fn new() -> Self {
        let mut mair = MAIR_EL1.get().to_le_bytes();
        for cache in [
            CacheType::WriteBack,
            CacheType::Uncacheable,
            CacheType::WriteThrough,
            CacheType::MemoryMappedIO,
        ] {
            let want = MemoryAttribute::from(cache).raw();
            if mair.contains(&want) {
                continue;
            }
            if let Some(spare) = (1..mair.len()).find(|&i| mair[..i].contains(&mair[i])) {
                mair[spare] = want;
            }
        }
        MAIR_EL1.set(u64::from_le_bytes(mair));
        unsafe { core::arch::asm!("isb") };
        Self {
            mair: mair.map(MemoryAttribute::new),
        }
    }

    // read entries of the register state
    pub fn read_entry(&self, index: AttributeIndex) -> Option<MemoryAttribute> {
        // we assume we keep an up to date copy of the MAIR register
        // we assume (for now) that the index is in bounds
        let attr = self.mair[index as usize];
        if attr.is_valid() { Some(attr) } else { None }
    }

    // map a requested memory type to an index
    fn map_to_entry(&mut self, memory: CacheType) -> Result<AttributeIndex, AttributeError> {
        // check if it exists
        let index = self.attribute_index(memory);
        match index {
            // it exists
            Some(x) => Ok(x),
            // it does not exist
            None => {
                // apply memory type mapping -> mair entry
                let entry = MemoryAttribute::from(memory);
                // find a empty slot in mair
                let void = self.find_invalid_index()?;
                // set the index to the desired entry
                self.mair[void as usize] = entry;
                // write out state
                MAIR_EL1.set(u64::from_le_bytes(
                    // assumes we are on a le machine
                    unsafe {
                        // TODO: test this ...
                        // don't know if transmute works now with local reg copy???
                        core::mem::transmute::<[MemoryAttribute; 8], [u8; 8]>(self.mair)
                    },
                ));
                Ok(void)
            }
        }
    }

    // retrieve an index corresponding to mapping (if any)
    pub fn attribute_index(&self, memory: CacheType) -> Option<AttributeIndex> {
        // convert cache type to entry type
        let attr = MemoryAttribute::from(memory);
        for (index, entry) in self.mair.iter().enumerate() {
            if entry.is_valid() {
                // check for equality
                if entry.raw() == attr.raw() {
                    return Some(index as u8);
                }
            }
        }
        None
    }

    // returns the index of an invalid entry (if any)
    fn find_invalid_index(&self) -> Result<AttributeIndex, AttributeError> {
        // could be option
        for (index, entry) in self.mair.iter().enumerate() {
            if !entry.is_valid() {
                return Ok(index as AttributeIndex);
            }
        }
        Err(AttributeError::Full)
    }
}

impl From<CacheType> for MemoryAttribute {
    fn from(memory: CacheType) -> Self {
        // Both nibbles: the field values are unshifted, and a normal attribute with inner 0 is
        // UNPREDICTABLE.
        let normal = |v: u8| MemoryAttribute::new(v << 4 | v);
        match memory {
            // we map all device mmio as strict device memory
            CacheType::MemoryMappedIO => MemoryAttribute::new(
                MEM_ATTR::Device::Value::nonGathering_nonReordering_noEarlyWriteAck as u8,
            ),
            // Normal non-cacheable is also ARM's write-combining memory.
            CacheType::Uncacheable | CacheType::WriteCombining => {
                normal(MEM_ATTR::Normal_Outer::Value::NonCacheable as u8)
            }
            CacheType::WriteThrough => normal(
                MEM_ATTR::Normal_Outer::Value::WriteThrough_NonTransient_ReadWriteAlloc as u8,
            ),
            CacheType::WriteBack => {
                normal(MEM_ATTR::Normal_Outer::Value::WriteBack_NonTransient_ReadWriteAlloc as u8)
            }
        }
    }
}

impl From<MemoryAttribute> for CacheType {
    fn from(attr: MemoryAttribute) -> Self {
        // NOTE: transient is basically a hint to the cacheing system
        // so we can place it in the same class as other non-transient
        // memory in this case
        match attr.attr.read_as_enum(MEM_ATTR::Normal_Outer) {
            // is this device memory or uncacheable memory?
            Some(MEM_ATTR::Normal_Outer::Value::Device) => CacheType::MemoryMappedIO,
            // is this memory not cacheable?
            Some(MEM_ATTR::Normal_Outer::Value::NonCacheable) => CacheType::Uncacheable,
            // is this memory write-through?
            Some(MEM_ATTR::Normal_Outer::Value::WriteThrough_Transient_WriteAlloc)
            | Some(MEM_ATTR::Normal_Outer::Value::WriteThrough_Transient_ReadAlloc)
            | Some(MEM_ATTR::Normal_Outer::Value::WriteThrough_Transient_ReadWriteAlloc)
            | Some(MEM_ATTR::Normal_Outer::Value::WriteThrough_NonTransient)
            | Some(MEM_ATTR::Normal_Outer::Value::WriteThrough_NonTransient_WriteAlloc)
            | Some(MEM_ATTR::Normal_Outer::Value::WriteThrough_NonTransient_ReadAlloc)
            | Some(MEM_ATTR::Normal_Outer::Value::WriteThrough_NonTransient_ReadWriteAlloc) => {
                CacheType::WriteThrough
            }
            // is this memory write-back?
            Some(MEM_ATTR::Normal_Outer::Value::WriteBack_Transient_WriteAlloc)
            | Some(MEM_ATTR::Normal_Outer::Value::WriteBack_Transient_ReadAlloc)
            | Some(MEM_ATTR::Normal_Outer::Value::WriteBack_Transient_ReadWriteAlloc)
            | Some(MEM_ATTR::Normal_Outer::Value::WriteBack_NonTransient)
            | Some(MEM_ATTR::Normal_Outer::Value::WriteBack_NonTransient_WriteAlloc)
            | Some(MEM_ATTR::Normal_Outer::Value::WriteBack_NonTransient_ReadAlloc)
            | Some(MEM_ATTR::Normal_Outer::Value::WriteBack_NonTransient_ReadWriteAlloc) => {
                CacheType::WriteBack
            }
            None => todo!("unrecognized cache type"),
        }
    }
}

// TODO: maybe this resource should have a lock? However, it rarely changes
static MEMORY_ATTR_MANAGER: Once<MemoryAttributeManager> = Once::new();

pub fn memory_attr_manager() -> &'static MemoryAttributeManager {
    MEMORY_ATTR_MANAGER.call_once(|| MemoryAttributeManager::new())
}

// unpredictable states
// 0b0000dd1x	UNPREDICTABLE.
// 0b01000000   If FEAT_XS is implemented: Normal Inner Non-cacheable, Outer Non-cacheable memory
// with the XS attribute set to 0. Otherwise, UNPREDICTABLE. 0b10100000   If FEAT_XS is implemented:
// Normal Inner Write-through Cacheable, Outer Write-through Cacheable, Read-Allocate, No-Write
// Allocate, Non-transient memory with the XS attribute set to 0. Otherwise, UNPREDICTABLE.
// 0b11110000   If FEAT_MTE2 is implemented: Tagged Normal Inner Write-Back, Outer Write-Back,
// Read-Allocate, Write-Allocate Non-transient memory. Otherwise, UNPREDICTABLE. 0bxxxx0000, (xxxx
// != 0000, xxxx != 0100, xxxx != 1010, xxxx != 1111)	UNPREDICTABLE.

register_bitfields! {u8,
    pub MEM_ATTR [
        Device OFFSET(0) NUMBITS(8) [
            nonGathering_nonReordering_noEarlyWriteAck = 0b0000_0000,
            nonGathering_nonReordering_EarlyWriteAck = 0b0000_0100,
            nonGathering_Reordering_EarlyWriteAck = 0b0000_1000,
            Gathering_Reordering_EarlyWriteAck = 0b0000_1100,
            // unpredictable if bit 1 is set
        ],
        Normal_Outer OFFSET(4) NUMBITS(4) [
            Device = 0b0000,

            WriteThrough_Transient_WriteAlloc = 0b0001,
            WriteThrough_Transient_ReadAlloc = 0b0010,
            WriteThrough_Transient_ReadWriteAlloc = 0b0011,

            NonCacheable = 0b0100,
            WriteBack_Transient_WriteAlloc = 0b0101,
            WriteBack_Transient_ReadAlloc = 0b0110,
            WriteBack_Transient_ReadWriteAlloc = 0b0111,

            WriteThrough_NonTransient = 0b1000,
            WriteThrough_NonTransient_WriteAlloc = 0b1001,
            WriteThrough_NonTransient_ReadAlloc = 0b1010,
            WriteThrough_NonTransient_ReadWriteAlloc = 0b1011,

            WriteBack_NonTransient = 0b1100,
            WriteBack_NonTransient_WriteAlloc = 0b1101,
            WriteBack_NonTransient_ReadAlloc = 0b1110,
            WriteBack_NonTransient_ReadWriteAlloc = 0b1111
            // unpredictable if bits are not (no match)
            // 0000
            // 0100
            // 1010
            // 1111
            // when lower bits are 0
        ],
        Normal_Inner OFFSET(0) NUMBITS(4) [
            WriteThrough_Transient = 0x0000,
            WriteThrough_Transient_WriteAlloc = 0x0001,
            WriteThrough_Transient_ReadAlloc = 0x0010,
            WriteThrough_Transient_ReadWriteAlloc = 0x0011,

            NonCacheable = 0b0100,
            WriteBack_Transient_WriteAlloc = 0b0101,
            WriteBack_Transient_ReadAlloc = 0b0110,
            WriteBack_Transient_ReadWriteAlloc = 0b0111,

            WriteThrough_NonTransient = 0b1000,
            WriteThrough_NonTransient_WriteAlloc = 0b1001,
            WriteThrough_NonTransient_ReadAlloc = 0b1010,
            WriteThrough_NonTransient_ReadWriteAlloc = 0b1011,

            WriteBack_NonTransient = 0b1100,
            WriteBack_NonTransient_WriteAlloc = 0b1101,
            WriteBack_NonTransient_ReadAlloc = 0b1110,
            WriteBack_NonTransient_ReadWriteAlloc = 0b1111
        ],
    ]
}
