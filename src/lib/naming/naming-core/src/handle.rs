use std::{
    path::Path,
    sync::{
        atomic::{AtomicU32, Ordering},
        OnceLock,
    },
};

use secgate::util::{Handle, SimpleBuffer};
use twizzler::object::ObjID;
use twizzler_rt_abi::{
    error::{ArgumentError, TwzError},
    object::MapFlags,
};

use crate::{
    api::NamerAPI, GetFlags, InlinePath, NsNode, Result, BUFFER_NSLOTS, BUFFER_SLOT_SIZE, PATH_MAX,
};

/// One handle to the naming server, shared by any number of threads.
///
/// Every method takes `&self`: a path short enough to inline crosses in the gate arguments and
/// touches no shared state at all, and longer paths (plus enumerate replies) go through disjoint
/// slots of the one shared buffer. This is what lets a runtime keep a single handle per process
/// instead of a pool -- the pool existed only because the buffer used to be written at offset 0 by
/// every call.
pub struct NamingHandle<'a, API: NamerAPI> {
    desc: u32,
    /// Created on first spill/enumerate. A handle that only ever does inline calls never makes
    /// one, which is what keeps a handle cheap: just a descriptor.
    buffer: OnceLock<SimpleBuffer>,
    /// Busy bit per buffer slot.
    slots: AtomicU32,
    api: &'a API,
}

/// A claimed slot of the shared buffer, freed on drop. Held across the gate call that names its
/// offset, which is what keeps concurrent spills disjoint.
struct SlotGuard<'h> {
    slots: &'h AtomicU32,
    idx: usize,
}

impl SlotGuard<'_> {
    fn offset(&self) -> usize {
        self.idx * BUFFER_SLOT_SIZE
    }
}

impl Drop for SlotGuard<'_> {
    fn drop(&mut self) {
        self.slots.fetch_and(!(1 << self.idx), Ordering::Release);
    }
}

impl<'a, API: NamerAPI> Drop for NamingHandle<'a, API> {
    fn drop(&mut self) {
        self.release();
    }
}

impl<'a, API: NamerAPI> NamingHandle<'a, API> {
    /// The handle's shared buffer, asking the server to create it the first time.
    fn buffer(&self) -> Result<&SimpleBuffer> {
        if let Some(b) = self.buffer.get() {
            return Ok(b);
        }
        let id = self.api.get_buffer(self.desc)?;
        let handle =
            twizzler_rt_abi::object::twz_rt_map_object(id, MapFlags::READ | MapFlags::WRITE)?;
        // Racing initializers both map the same object (the server creates it once per
        // descriptor); the loser just drops its extra mapping.
        let _ = self.buffer.set(SimpleBuffer::new(handle));
        // Unwrap-Ok: just set (by us or the winner).
        Ok(self.buffer.get().unwrap())
    }

    /// Claim a free slot, waiting if all are in flight. The wait is bounded by a gate call's
    /// duration, and hitting it at all takes `BUFFER_NSLOTS` concurrent spilled calls on one
    /// handle.
    fn take_slot(&self) -> SlotGuard<'_> {
        loop {
            let cur = self.slots.load(Ordering::Relaxed);
            let free = !cur & ((1u32 << BUFFER_NSLOTS) - 1);
            if free == 0 {
                std::thread::yield_now();
                continue;
            }
            let idx = free.trailing_zeros() as usize;
            if self
                .slots
                .compare_exchange_weak(cur, cur | (1 << idx), Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                return SlotGuard {
                    slots: &self.slots,
                    idx,
                };
            }
        }
    }

    fn path_bytes<P: AsRef<Path>>(path: &P) -> Result<&[u8]> {
        let bytes = path.as_ref().as_os_str().as_encoded_bytes();
        if bytes.len() > PATH_MAX {
            Err(ArgumentError::InvalidArgument.into())
        } else {
            Ok(bytes)
        }
    }

    /// Write one path into a fresh slot; the guard keeps the slot until the gate call returns.
    fn spill<P: AsRef<Path>>(&self, path: &P) -> Result<(SlotGuard<'_>, usize)> {
        let bytes = Self::path_bytes(path)?;
        let buffer = self.buffer()?;
        let slot = self.take_slot();
        buffer.write_offset(bytes, slot.offset());
        Ok((slot, bytes.len()))
    }

    /// Two paths packed into one slot, the second right after the first (a slot holds
    /// `2 * PATH_MAX`).
    fn spill2<P: AsRef<Path>, Q: AsRef<Path>>(
        &self,
        a: &P,
        b: &Q,
    ) -> Result<(SlotGuard<'_>, usize, usize)> {
        let ab = Self::path_bytes(a)?;
        let bb = Self::path_bytes(b)?;
        let buffer = self.buffer()?;
        let slot = self.take_slot();
        buffer.write_offset(ab, slot.offset());
        buffer.write_offset(bb, slot.offset() + ab.len());
        Ok((slot, ab.len(), bb.len()))
    }

    fn read_nodes(buffer: &SimpleBuffer, offset: usize, n: usize) -> Vec<NsNode> {
        let mut bytes = vec![0u8; n * size_of::<NsNode>()];
        buffer.read_offset(&mut bytes, offset);
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            unsafe { out.push(*(bytes.as_ptr().add(i * size_of::<NsNode>()) as *const NsNode)) };
        }
        out
    }

    /// Open a new naming handle.
    pub fn new(api: &'a API) -> Option<Self> {
        NamingHandle::open(api).ok()
    }

    pub fn put<P: AsRef<Path>>(&self, path: P, id: ObjID) -> Result<()> {
        if let Some(p) = InlinePath::new(&path) {
            return self.api.put_inline(self.desc, p, id);
        }
        let (slot, len) = self.spill(&path)?;
        self.api.put(self.desc, slot.offset(), len, id)
    }

    pub fn get(&self, path: &str, flags: GetFlags) -> Result<NsNode> {
        if let Some(inline) = InlinePath::new(path) {
            return self.api.get_inline(self.desc, inline, flags);
        }
        let (slot, len) = self.spill(&path)?;
        self.api.get(self.desc, slot.offset(), len, flags)
    }

    pub fn remove(&self, path: &str) -> Result<()> {
        if let Some(inline) = InlinePath::new(path) {
            return self.api.remove_inline(self.desc, inline);
        }
        let (slot, len) = self.spill(&path)?;
        self.api.remove(self.desc, slot.offset(), len)
    }

    pub fn change_namespace<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        if let Some(p) = InlinePath::new(&path) {
            return self.api.change_namespace_inline(self.desc, p);
        }
        let (slot, len) = self.spill(&path)?;
        self.api.change_namespace(self.desc, slot.offset(), len)
    }

    pub fn put_namespace<P: AsRef<Path>>(&self, path: P, persist: bool) -> Result<()> {
        if let Some(p) = InlinePath::new(&path) {
            return self.api.mkns_inline(self.desc, p, persist);
        }
        let (slot, len) = self.spill(&path)?;
        self.api.mkns(self.desc, slot.offset(), len, persist)
    }

    pub fn rename<P: AsRef<Path>, Q: AsRef<Path>>(&self, old: P, new: Q) -> Result<()> {
        if let (Some(o), Some(n)) = (InlinePath::new(&old), InlinePath::new(&new)) {
            return self.api.rename_inline(self.desc, o, n);
        }
        let (slot, old_len, new_len) = self.spill2(&old, &new)?;
        self.api.rename(self.desc, slot.offset(), old_len, new_len)
    }

    pub fn symlink<P: AsRef<Path>, L: AsRef<Path>>(&self, path: P, link: L) -> Result<()> {
        if let (Some(p), Some(l)) = (InlinePath::new(&path), InlinePath::new(&link)) {
            return self.api.link_inline(self.desc, p, l);
        }
        let (slot, name_len, link_len) = self.spill2(&path, &link)?;
        self.api.link(self.desc, slot.offset(), name_len, link_len)
    }

    /// A slot bounds one enumerate call's reply, so large requests page through the slot; the
    /// `skip`/`count` protocol underneath is unchanged.
    pub fn enumerate_names_nsid(
        &self,
        nsid: ObjID,
        skip: usize,
        count: usize,
    ) -> Result<Vec<NsNode>> {
        tracing::trace!(
            "enumerating namespace {} (skip {}, count {})",
            nsid,
            skip,
            count
        );
        let per = BUFFER_SLOT_SIZE / size_of::<NsNode>();
        let buffer = self.buffer()?;
        let slot = self.take_slot();
        let mut out = Vec::new();
        loop {
            let want = (count - out.len()).min(per);
            if want == 0 {
                break;
            }
            let n = self
                .api
                .enumerate_names_nsid(self.desc, nsid, slot.offset(), skip + out.len(), want)?
                .min(want);
            out.extend(Self::read_nodes(buffer, slot.offset(), n));
            if n < want {
                break;
            }
        }
        Ok(out)
    }

    pub fn enumerate_names_relative<P: AsRef<Path>>(
        &self,
        path: P,
        skip: usize,
        count: usize,
    ) -> Result<Vec<NsNode>> {
        let bytes = Self::path_bytes(&path)?;
        let per = BUFFER_SLOT_SIZE / size_of::<NsNode>();
        let buffer = self.buffer()?;
        let slot = self.take_slot();
        let mut out = Vec::new();
        loop {
            let want = (count - out.len()).min(per);
            if want == 0 {
                break;
            }
            // The reply lands where the path was, so rewrite the path each round.
            buffer.write_offset(bytes, slot.offset());
            let n = self
                .api
                .enumerate_names(
                    self.desc,
                    slot.offset(),
                    bytes.len(),
                    skip + out.len(),
                    want,
                )?
                .min(want);
            out.extend(Self::read_nodes(buffer, slot.offset(), n));
            if n < want {
                break;
            }
        }
        Ok(out)
    }

    pub fn enumerate_names(&self, skip: usize, count: usize) -> Result<Vec<NsNode>> {
        self.enumerate_names_relative(&".", skip, count)
    }
}

impl<'a, API: NamerAPI> Handle for NamingHandle<'a, API> {
    type OpenError = TwzError;

    type OpenInfo = &'a API;

    fn open(info: Self::OpenInfo) -> std::result::Result<Self, Self::OpenError>
    where
        Self: Sized,
    {
        let desc = info.open_handle()?;
        Ok(Self {
            desc,
            buffer: OnceLock::new(),
            slots: AtomicU32::new(0),
            api: info,
        })
    }

    fn release(&mut self) {
        let _ = self
            .api
            .close_handle(self.desc)
            .inspect_err(|e| tracing::warn!("{}", e));
    }
}
