#[repr(transparent)]
struct MetaFlags(u32);

#[repr(transparent)]
struct Nonce(u128);

#[repr(C)]
pub struct MetaInfo {
    nonce: Nonce,
    kuid: ObjID,
    flags: MetaFlags,
    fotcount: u16,
    extcount: u16,
}

#[repr(C)]
struct MetaExt {
    tag: MetaExtTag,
    value: u64,
}
