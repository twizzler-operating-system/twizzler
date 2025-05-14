use core::sync::atomic::Ordering;

use twizzler_abi::{meta::MetaFlags, object::ObjID};

use super::{Object, ObjectRef};

#[repr(C)]
struct Ids {
    nonce: u128,
    kuid: ObjID,
    flags: MetaFlags,
}

static OID: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(1);

pub(super) fn backup_id_gen() -> ObjID {
    ((OID.fetch_add(1, Ordering::SeqCst) as u128) | (1u128 << 64)).into()
}

fn gen_id(nonce: u128, kuid: ObjID, flags: MetaFlags) -> ObjID {
    let mut ids = Ids { nonce, kuid, flags };
    let ptr = core::ptr::addr_of_mut!(ids).cast::<u8>();
    let slice = unsafe { core::slice::from_raw_parts_mut(ptr, size_of::<Ids>()) };
    let hash = crate::crypto::sha256(slice);
    let mut id_buf = [0u8; 16];
    id_buf.copy_from_slice(&hash[0..16]);
    for i in 0..16 {
        id_buf[i] ^= hash[i + 16];
    }
    u128::from_ne_bytes(id_buf).into()
}

pub fn calculate_new_id(kuid: ObjID, flags: MetaFlags, nonce: u128) -> ObjID {
    gen_id(nonce, kuid, flags)
}

fn verify_id(id: ObjID, nonce: u128, kuid: ObjID, flags: MetaFlags) -> bool {
    let generated = gen_id(nonce, kuid, flags);
    id == generated
}

impl Object {
    pub fn check_id(self: &ObjectRef) -> bool {
        *self.verified_id.call_once(|| loop {
            let meta = self.read_meta(true);
            if let Some(meta) = meta {
                break verify_id(self.id, meta.nonce.0, meta.kuid, meta.flags);
            } else {
                logln!("failed to read metadata");
            }
        })
    }
}
