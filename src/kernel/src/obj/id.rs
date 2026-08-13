use core::sync::atomic::Ordering;

use twizzler_abi::{
    meta::{MetaFlags, MetaInfo},
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

    if id != generated && id.parts()[0] != 0x8000000000000000 && id.parts()[0] != 1 {
        logln!(
            "verify_id: {} {:?} {:?} {:?} => {:?} :: {:?}",
            nonce,
            kuid,
            flags,
            def_prot,
            generated,
            id
        );
    }
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

// Temporary (pagerperf.md): `check_id` was 67% of `insert_object`, and 46x more expensive per map
// once four threads map cold objects at once. The result is memoized in `verified_id`, so the
// suspicion is that the whole cost is the first call's `read_meta()` -- a meta-page read that for a
// cold object is a page-in, i.e. paging charged to the mapping path rather than crypto.
mod checkidstats {
    use core::sync::atomic::{AtomicU64, Ordering};

    static HITS: AtomicU64 = AtomicU64::new(0);
    static MISSES: AtomicU64 = AtomicU64::new(0);
    static RETRIES: AtomicU64 = AtomicU64::new(0);
    static READ_NS: AtomicU64 = AtomicU64::new(0);
    static VERIFY_NS: AtomicU64 = AtomicU64::new(0);
    static PRESET: AtomicU64 = AtomicU64::new(0);

    /// A memo installed by someone who already knew the answer, rather than by a `check_id` miss.
    /// Every one of these turns what would have been a miss into a hit; the two counters together
    /// say whether the presetting sites cover the objects that actually get mapped.
    pub fn preset() {
        PRESET.fetch_add(1, Ordering::Relaxed);
    }

    pub fn hit() {
        let h = HITS.fetch_add(1, Ordering::Relaxed) + 1;
        maybe_report(h + MISSES.load(Ordering::Relaxed));
    }

    pub fn miss(read_ns: u64, verify_ns: u64, retries: u64) {
        let m = MISSES.fetch_add(1, Ordering::Relaxed) + 1;
        RETRIES.fetch_add(retries, Ordering::Relaxed);
        READ_NS.fetch_add(read_ns, Ordering::Relaxed);
        VERIFY_NS.fetch_add(verify_ns, Ordering::Relaxed);
        maybe_report(m + HITS.load(Ordering::Relaxed));
    }

    fn maybe_report(n: u64) {
        if !n.is_power_of_two() {
            return;
        }
        crate::logln!(
            "CHECKIDSTATS {} check_id: {} hits, {} misses ({} retries), {} preset, read_meta {} us, verify {} us",
            n,
            HITS.load(Ordering::Relaxed),
            MISSES.load(Ordering::Relaxed),
            RETRIES.load(Ordering::Relaxed),
            PRESET.load(Ordering::Relaxed),
            READ_NS.load(Ordering::Relaxed) / 1000,
            VERIFY_NS.load(Ordering::Relaxed) / 1000,
        );
    }
}

impl Object {
    pub fn has_checked_id(&self) -> bool {
        self.verified_id.poll().is_some()
    }

    /// Record the result of an ID check made somewhere other than [Object::check_id].
    ///
    /// Two callers know the answer without reading anything: `sys_object_create`, which derived the
    /// ID from these very fields moments ago, and the pager completion path, for a pager that says
    /// it validated the object. Both run before the object is registered, so no `check_id` can
    /// race this. Calling it on an object that has already been checked is a no-op, by `Once`.
    pub fn set_verified_id(&self, verified: bool, default_prot: Protections) {
        if self.verified_id.poll().is_some() {
            return;
        }
        checkidstats::preset();
        self.verified_id.call_once(|| (verified, default_prot));
    }

    /// Record the ID check for metadata the kernel is writing itself.
    ///
    /// [Object::check_id] would read these very bytes back and run `verify_id` over them, so
    /// running it here against the struct being written is the same answer without the read --
    /// which for a pager-backed object is a page-in charged to whoever maps the object first.
    ///
    /// Only the first call takes effect (`set_verified_id` is a `Once`), so an object whose meta
    /// the kernel rewrites keeps the answer for the first write. `initrd.rs` is the only site that
    /// rewrites, and it now preserves the fields this verifies against.
    pub fn note_written_meta(&self, meta: &MetaInfo) {
        // `verified_id` is an `OnceWait`, and its `call_once` ends in `CondVar::signal`, which
        // unwraps the current thread. `initrd::init` writes object metadata from `kernel_main`,
        // before threading exists, so there the memo is skipped rather than recorded -- those
        // objects are DRAM-resident and their `read_meta` is the cheap kind. Same hazard `once.rs`
        // describes for `Once` vs `OnceWait`.
        if crate::thread::current_thread_ref().is_none() {
            return;
        }
        self.set_verified_id(
            verify_id(
                self.id,
                meta.nonce.0,
                meta.kuid,
                meta.flags,
                meta.default_prot,
            ),
            meta.default_prot,
        );
    }

    pub fn check_id(self: &ObjectRef) -> (bool, Protections) {
        if self.verified_id.poll().is_none() {
            let mut read_ns = 0u64;
            let mut verify_ns = 0u64;
            let mut retries = 0u64;
            let id = loop {
                let t_read = crate::instant::Instant::now();
                let meta = self.read_meta();
                read_ns += (crate::instant::Instant::now() - t_read).as_nanos() as u64;
                if let Some(meta) = meta {
                    let t_verify = crate::instant::Instant::now();
                    let v = (
                        verify_id(
                            self.id,
                            meta.nonce.0,
                            meta.kuid,
                            meta.flags,
                            meta.default_prot,
                        ),
                        meta.default_prot,
                    );
                    verify_ns = (crate::instant::Instant::now() - t_verify).as_nanos() as u64;
                    break v;
                } else {
                    retries += 1;
                    logln!("failed to read metadata");
                }
            };
            checkidstats::miss(read_ns, verify_ns, retries);
            *self.verified_id.call_once(|| id)
        } else {
            checkidstats::hit();
            *self.verified_id.poll().unwrap()
        }
    }
}
