use core::sync::atomic::Ordering;

use twizzler_abi::{
    meta::MetaFlags,
    object::{ObjID, Protections},
};

use super::{Object, ObjectRef};

#[repr(C)]
struct Ids {
    nonce: u128,
    kuid: ObjID,
    flags: MetaFlags,
    def_prot: Protections,
    _resv2: u32,
    _resv3: u64,
}

static OID: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(1);

pub(super) fn backup_id_gen() -> ObjID {
    ((OID.fetch_add(1, Ordering::SeqCst) as u128) | (1u128 << 64)).into()
}

fn gen_id(nonce: u128, kuid: ObjID, flags: MetaFlags, def_prot: Protections) -> ObjID {
    assert_eq!(size_of::<Ids>(), 48);
    let ids = Ids {
        nonce,
        kuid,
        flags,
        def_prot,
        _resv2: 0,
        _resv3: 0,
    };
    let ptr = core::ptr::addr_of!(ids).cast::<u8>();
    let slice = unsafe { core::slice::from_raw_parts(ptr, size_of::<Ids>()) };
    let hash = crate::crypto::sha256(slice);
    let mut id_buf = [0u8; 16];
    id_buf.copy_from_slice(&hash[0..16]);
    for i in 0..16 {
        id_buf[i] ^= hash[i + 16];
    }
    u128::from_ne_bytes(id_buf).into()
}

pub fn calculate_new_id(
    kuid: ObjID,
    flags: MetaFlags,
    nonce: u128,
    def_prot: Protections,
) -> ObjID {
    let id = gen_id(nonce, kuid, flags, def_prot);
    debug_assert!(verify_id(id, nonce, kuid, flags, def_prot));
    /*
    logln!(
        "calc_new_id: {} {:?} {:?} {:?} => {:?}",
        nonce,
        kuid,
        flags,
        def_prot,
        id
    );
    */
    id
}

fn verify_id(id: ObjID, nonce: u128, kuid: ObjID, flags: MetaFlags, def_prot: Protections) -> bool {
    let generated = gen_id(nonce, kuid, flags, def_prot);

    // logln!(
    //     "verify: {} {:?} {:?} {:?} => {:?} :: {:?}",
    //     nonce,
    //     kuid,
    //     flags,
    //     def_prot,
    //     generated,
    //     id
    // );

    id == generated
}

impl Object {
    pub fn check_id(self: &ObjectRef) -> (bool, Protections) {
        *self.verified_id.call_once(|| loop {
            let meta = self.read_meta(true);
            if let Some(meta) = meta {
                break (
                    verify_id(
                        self.id,
                        meta.nonce.0,
                        meta.kuid,
                        meta.flags,
                        meta.default_prot,
                    ),
                    meta.default_prot,
                );
            } else {
                logln!("failed to read metadata");
            }
        })
    }
}
