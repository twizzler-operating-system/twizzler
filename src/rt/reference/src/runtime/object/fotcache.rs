use std::collections::BTreeMap;

use twizzler_abi::simple_mutex::Mutex;
use twizzler_rt_abi::{
    bindings::map_flags,
    object::{MapFlags, ObjectHandle},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(transparent)]
struct Key(u64);

impl Key {
    pub fn new(idx: u32, flags: map_flags) -> Self {
        Key((idx as u64) << 32 | flags as u64)
    }
}

pub(crate) struct FotCache {
    cache: Mutex<BTreeMap<Key, ObjectHandle>>,
}

impl FotCache {
    pub fn new() -> Self {
        FotCache {
            cache: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn insert(&self, idx: u64, flags: MapFlags, handle: ObjectHandle) -> Option<()> {
        tracing::trace!(
            "Inserting FOT cache entry for idx: {}, flags: {:?} as {}",
            idx,
            flags,
            handle.id()
        );
        let key = Key::new(idx.try_into().ok()?, flags.bits());
        let mut cache = self.cache.lock();
        cache.insert(key, handle).map(|_| ())
    }

    pub fn with_entry<R>(
        &self,
        idx: u64,
        flags: MapFlags,
        f: impl FnOnce(&ObjectHandle) -> R,
    ) -> Option<R> {
        let key = Key::new(idx.try_into().ok()?, flags.bits());
        let cache = self.cache.lock();
        cache.get(&key).map(|handle| f(handle))
    }

    pub unsafe fn resolve_cached_ptr(&self, idx: u64, flags: MapFlags) -> Option<*mut u8> {
        self.with_entry(idx, flags, |h| {
            tracing::trace!(
                "Resolved cached pointer for idx: {}, flags: {:?}: {}",
                idx,
                flags,
                h.id(),
            );
            h.start()
        })
    }

    pub fn clear(&self) {
        tracing::trace!("Clearing FOT cache");
        self.cache.lock().clear();
    }
}
