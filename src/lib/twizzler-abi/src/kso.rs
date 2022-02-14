use core::fmt::Display;

use crate::object::ObjID;

pub const KSO_NAME_MAX_LEN: usize = 512;
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
            Self::ObjID(ObjID::new_from_parts(x.0, x.1))
        }
    }
}

impl From<KactionValue> for (u64, u64) {
    fn from(x: KactionValue) -> Self {
        match x {
            KactionValue::U64(x) => (0xffffffffffffffff, x),
            KactionValue::ObjID(id) => id.split(),
        }
    }
}

impl KactionValue {
    pub fn unwrap_objid(self) -> ObjID {
        match self {
            KactionValue::U64(_) => panic!("failed to unwrap ObjID"),
            KactionValue::ObjID(o) => o,
        }
    }

    pub fn objid(self) -> Option<ObjID> {
        match self {
            KactionValue::U64(_) => None,
            KactionValue::ObjID(o) => Some(o),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[repr(C)]
pub enum KactionError {
    Unknown = 0,
    InvalidArgument = 1,
    NotFound = 2,
}

impl From<u64> for KactionError {
    fn from(x: u64) -> Self {
        match x {
            1 => KactionError::InvalidArgument,
            2 => KactionError::NotFound,
            _ => KactionError::Unknown,
        }
    }
}

impl From<KactionError> for u64 {
    fn from(x: KactionError) -> Self {
        match x {
            KactionError::Unknown => 0,
            KactionError::InvalidArgument => 1,
            KactionError::NotFound => 2,
        }
    }
}

bitflags::bitflags! {
    pub struct KactionFlags: u64 {

    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[repr(C)]
pub enum KactionGenericCmd {
    GetKsoRoot,
    GetChild(u16),
    GetSubObject(u8, u8),
}

impl From<KactionGenericCmd> for u32 {
    fn from(x: KactionGenericCmd) -> Self {
        let (h, l) = match x {
            KactionGenericCmd::GetKsoRoot => (0, 0),
            KactionGenericCmd::GetChild(v) => (1, v),
            KactionGenericCmd::GetSubObject(t, v) => (2, ((t as u16) << 8) | (v as u16)),
        };
        ((h as u32) << 16) | l as u32
    }
}

impl TryFrom<u32> for KactionGenericCmd {
    type Error = KactionError;
    fn try_from(x: u32) -> Result<KactionGenericCmd, KactionError> {
        let (h, l) = ((x >> 16) as u16, (x & 0xffff) as u16);
        let v = match h {
            0 => KactionGenericCmd::GetKsoRoot,
            1 => KactionGenericCmd::GetChild(l),
            2 => KactionGenericCmd::GetSubObject((l >> 8) as u8, l as u8),
            _ => Err(KactionError::InvalidArgument)?,
        };
        Ok(v)
    }
}

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
    type Error = KactionError;
    fn try_from(x: u64) -> Result<KactionCmd, KactionError> {
        let (h, l) = ((x >> 32) as u32, (x & 0xffffffff) as u32);
        let v = match h {
            0 => KactionCmd::Generic(KactionGenericCmd::try_from(l)?),
            1 => KactionCmd::Specific(l),
            _ => Err(KactionError::InvalidArgument)?,
        };
        Ok(v)
    }
}
