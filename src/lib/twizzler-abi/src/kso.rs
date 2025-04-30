//! Functions to deal with Kernel State Objects (KSOs). These are objects created by the kernel to
//! describe the running state of the system and expose device memory to userspace.

use core::fmt::Display;

use twizzler_rt_abi::{
    error::{ArgumentError, TwzError},
    Result,
};

use crate::object::ObjID;

/// Maximum name length for a KSO.
pub const KSO_NAME_MAX_LEN: usize = 512;
/// The base struct for any kernel state object.
#[repr(C)]
pub struct KsoHdr {
    version: u32,
    flags: u16,
    name_len: u16,
    name: [u8; KSO_NAME_MAX_LEN],
}

impl Display for KsoHdr {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "{}",
            core::str::from_utf8(&self.name[0..self.name_len as usize])
                .map_err(|_| core::fmt::Error)?
        )
    }
}

impl KsoHdr {
    /// Construct a new kernel state object header.
    pub fn new(name: &str) -> Self {
        let b = name.as_bytes();
        let mut ret = Self {
            version: 0,
            flags: 0,
            name_len: b.len() as u16,
            name: [0; KSO_NAME_MAX_LEN],
        };
        for (i, v) in b.iter().take(KSO_NAME_MAX_LEN).enumerate() {
            ret.name[i] = *v;
        }
        ret
    }
}

/// A value to pass for a KAction.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[repr(C)]
pub enum KactionValue {
    U64(u64),
    ObjID(ObjID),
}

impl From<(u64, u64)> for KactionValue {
    fn from(x: (u64, u64)) -> Self {
        if x.0 == 0xFFFFFFFFFFFFFFFF {
            Self::U64(x.1)
        } else {
            Self::ObjID(ObjID::from_parts([x.0, x.1]))
        }
    }
}

impl From<KactionValue> for (u64, u64) {
    fn from(x: KactionValue) -> Self {
        let parts = match x {
            KactionValue::U64(x) => [0xffffffffffffffff, x],
            KactionValue::ObjID(id) => id.parts(),
        };
        (parts[0], parts[1])
    }
}

impl KactionValue {
    /// If the value is an object ID, return it, otherwise panic.
    pub fn unwrap_objid(self) -> ObjID {
        match self {
            KactionValue::U64(_) => panic!("failed to unwrap ObjID"),
            KactionValue::ObjID(o) => o,
        }
    }

    /// If the value is an object ID, return it, otherwise return None.
    pub fn objid(self) -> Option<ObjID> {
        match self {
            KactionValue::U64(_) => None,
            KactionValue::ObjID(o) => Some(o),
        }
    }

    /// If the value is a u64, return it, otherwise panic.
    pub fn unwrap_u64(self) -> u64 {
        match self {
            KactionValue::ObjID(_) => panic!("failed to unwrap ObjID"),
            KactionValue::U64(o) => o,
        }
    }

    /// If the value is a u64, return it, otherwise return None.
    pub fn u64(self) -> Option<u64> {
        match self {
            KactionValue::U64(x) => Some(x),
            KactionValue::ObjID(_) => None,
        }
    }
}

bitflags::bitflags! {
    /// Possible flags for kaction.
    pub struct KactionFlags: u64 {
    }
}

/// A generic kaction command, applies to all KSOs.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[repr(C)]
pub enum KactionGenericCmd {
    /// Get the root of the KSO tree.
    GetKsoRoot,
    /// Get a child object.
    GetChild(u16),
    /// Get a sub-object.
    GetSubObject(u8, u8),
    /// Pin pages of object memory.
    PinPages(u16),
    /// Release Pin
    ReleasePin,
}

impl From<KactionGenericCmd> for u32 {
    fn from(x: KactionGenericCmd) -> Self {
        let (h, l) = match x {
            KactionGenericCmd::GetKsoRoot => (0, 0),
            KactionGenericCmd::GetChild(v) => (1, v),
            KactionGenericCmd::GetSubObject(t, v) => (2, ((t as u16) << 8) | (v as u16)),
            KactionGenericCmd::PinPages(v) => (3, v),
            KactionGenericCmd::ReleasePin => (4, 0),
        };
        ((h as u32) << 16) | l as u32
    }
}

impl TryFrom<u32> for KactionGenericCmd {
    type Error = TwzError;
    fn try_from(x: u32) -> Result<KactionGenericCmd> {
        let (h, l) = ((x >> 16) as u16, (x & 0xffff) as u16);
        let v = match h {
            0 => KactionGenericCmd::GetKsoRoot,
            1 => KactionGenericCmd::GetChild(l),
            2 => KactionGenericCmd::GetSubObject((l >> 8) as u8, l as u8),
            3 => KactionGenericCmd::PinPages(l),
            4 => KactionGenericCmd::ReleasePin,
            _ => return Err(ArgumentError::InvalidArgument.into()),
        };
        Ok(v)
    }
}

/// A KAction command, either generic or KSO-specific.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[repr(C)]
pub enum KactionCmd {
    Generic(KactionGenericCmd),
    Specific(u32),
}

impl From<KactionCmd> for u64 {
    fn from(x: KactionCmd) -> Self {
        let (h, l) = match x {
            KactionCmd::Generic(x) => (0, x.into()),
            KactionCmd::Specific(x) => (1, x),
        };
        ((h as u64) << 32) | l as u64
    }
}

impl TryFrom<u64> for KactionCmd {
    type Error = TwzError;
    fn try_from(x: u64) -> Result<KactionCmd> {
        let (h, l) = ((x >> 32) as u32, (x & 0xffffffff) as u32);
        let v = match h {
            0 => KactionCmd::Generic(KactionGenericCmd::try_from(l)?),
            1 => KactionCmd::Specific(l),
            _ => return Err(ArgumentError::InvalidArgument.into()),
        };
        Ok(v)
    }
}

const KACTION_PACK_MASK: u64 = 0xffffffff;
const KACTION_PACK_BITS: u64 = 32;
pub fn pack_kaction_pin_start_and_len(start: u64, len: usize) -> Option<u64> {
    let len: u64 = len.try_into().ok()?;
    if len > KACTION_PACK_MASK || start > KACTION_PACK_MASK {
        return None;
    }
    Some(len << KACTION_PACK_BITS | start)
}

pub fn unpack_kaction_pin_start_and_len(val: u64) -> Option<(u64, usize)> {
    Some((
        val & KACTION_PACK_MASK,
        (val >> KACTION_PACK_BITS).try_into().ok()?,
    ))
}

pub fn pack_kaction_pin_token_and_len(token: u32, len: usize) -> Option<u64> {
    let len: u64 = len.try_into().ok()?;
    let token: u64 = token.into();
    if len > KACTION_PACK_MASK {
        return None;
    }
    Some(len << KACTION_PACK_BITS | token)
}

pub fn unpack_kaction_pin_token_and_len(val: u64) -> Option<(u32, usize)> {
    Some((
        (val & KACTION_PACK_MASK) as u32,
        (val >> KACTION_PACK_BITS).try_into().ok()?,
    ))
}

#[derive(Debug)]
#[repr(u32)]
pub enum InterruptPriority {
    High,
    Normal,
    Low,
}

bitflags::bitflags! {
    #[derive(Debug)]
    pub struct InterruptAllocateOptions:u32 {
        const UNIQUE = 0x1;
    }
}

pub fn pack_kaction_int_pri_and_opts(
    pri: InterruptPriority,
    opts: InterruptAllocateOptions,
) -> u64 {
    ((pri as u64) << KACTION_PACK_BITS) | opts.bits() as u64
}

pub fn unpack_kaction_int_pri_and_opts(
    val: u64,
) -> Option<(InterruptPriority, InterruptAllocateOptions)> {
    let pri = match val >> KACTION_PACK_BITS {
        1 => InterruptPriority::Low,
        2 => InterruptPriority::High,
        _ => InterruptPriority::Normal,
    };
    let opts = InterruptAllocateOptions::from_bits(val as u32)?;
    Some((pri, opts))
}
