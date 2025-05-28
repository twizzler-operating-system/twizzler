use core::fmt::Display;

use heapless::{FnvIndexMap, Vec};
use twizzler_abi::object::{ObjID, Protections, NULLPAGE_SIZE};

use crate::{Cap, Del};

/// completely arbitrary amount of mask entries in a security context
pub const MASKS_MAX: usize = 16;
/// completely arbitrary amount of capabilites and delegations in a security context
pub const SEC_CTX_MAP_LEN: usize = 16;
/// arbitrary number of map items per target object
pub const MAP_ITEMS_PER_OBJ: usize = 16;

#[derive(Debug)]
pub struct Mask {
    /// object whose permissions will be masked.
    pub target: ObjID,
    /// Specifies a mask on the permissions granted by capabilties and delegations in this security
    /// context.
    pub permmask: Protections,
    /// an override mask on the context's global mask.
    pub ovrmask: Protections,
    // not sure what these flags are for?
    // flags: BitField
}

impl Mask {
    pub fn new(target: ObjID, permmask: Protections, ovrmask: Protections) -> Self {
        Mask {
            target,
            permmask,
            ovrmask,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub enum CtxMapItemType {
    #[default]
    Cap,
    Del,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct CtxMapItem {
    /// Type of the Map Item
    pub item_type: CtxMapItemType,
    /// The offset into the object
    pub offset: usize,
}

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct SecCtxFlags: u16 {
        // a security context should have an undetachable bit,
        const UNDETACHABLE = 1;
    }
}

/// The base of a Security Context, holding a map to the capabilities and delegations stored inside,
/// masks on targets
#[derive(Debug)]
pub struct SecCtxBase {
    /// A map that holds the mapping from an ObjID to the capabilities and delegations stored
    /// inside this context that apply to that target ObjID.
    pub map: FnvIndexMap<ObjID, Vec<CtxMapItem, MAP_ITEMS_PER_OBJ>, SEC_CTX_MAP_LEN>,
    /// A map holding masks that apply for a target ObjID.
    pub masks: FnvIndexMap<ObjID, Mask, MASKS_MAX>,
    /// The global mask that applies to all protections gratned by this Security Context.
    pub global_mask: Protections,
    /// The running offset into the object where a new entry can be inserted.
    offset: usize,
    /// Flags specific to this security context.
    pub flags: SecCtxFlags,
}

pub const OBJECT_ROOT_OFFSET: usize = size_of::<SecCtxBase>() + NULLPAGE_SIZE;

pub enum InsertType {
    Cap(Cap),
    Del(Del),
}

impl SecCtxBase {
    pub fn new(global_mask: Protections, flags: SecCtxFlags) -> Self {
        Self {
            map: FnvIndexMap::<ObjID, Vec<CtxMapItem, MAP_ITEMS_PER_OBJ>, SEC_CTX_MAP_LEN>::new(),
            masks: FnvIndexMap::<ObjID, Mask, MASKS_MAX>::new(),
            global_mask,
            offset: 0,
            flags,
        }
    }
}

#[cfg(feature = "user")]
use twizzler::marker::BaseType;

#[cfg(feature = "user")]
impl BaseType for SecCtxBase {
    //NOTE: unsure if this is what the fingerprint "should" be, just chose a random number.
    fn fingerprint() -> u64 {
        16
    }
}

impl Display for CtxMapItem {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Item Type: {:?}\n", self.item_type)?;
        write!(f, "Offset: {:#X}\n", self.offset)?;
        Ok(())
    }
}

impl Default for SecCtxBase {
    fn default() -> Self {
        Self::new(Protections::all(), SecCtxFlags::empty())
    }
}
