use crate::object::ObjID;

pub const KSO_NAME_MAX_LEN: usize = 512;
#[repr(C)]
pub struct KsoHdr {
    version: u32,
    flags: u16,
    name_len: u16,
    name: [u8; KSO_NAME_MAX_LEN],
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

#[repr(C)]
pub enum KactionValue {
    U64(u64),
    ObjID(ObjID),
}

#[repr(C)]
pub enum KactionError {
    Unknown = 0,
}
