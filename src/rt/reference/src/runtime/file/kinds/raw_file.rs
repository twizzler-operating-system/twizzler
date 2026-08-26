use std::{
    io::{ErrorKind, SeekFrom},
    ptr::null_mut,
    sync::{
        atomic::{AtomicPtr, AtomicU64, Ordering},
        Arc,
    },
};

use libc::{S_IFREG, S_IRWXG, S_IRWXO, S_IRWXU};
use secgate::TwzError;
use twizzler_abi::{
    meta::MetaExt,
    object::{ObjID, MAX_SIZE, NULLPAGE_SIZE},
};
use twizzler_rt_abi::{
    bindings::{sync_info, SYNC_FLAG_ASYNC_DURABLE, SYNC_FLAG_DURABLE},
    error::ArgumentError,
    fd::FdInfo,
    object::{MapFlags, ObjectCmd, ObjectHandle, MEXT_MTIME, MEXT_SIZED},
    Result,
};

use crate::{
    runtime::file::{Fd, WaitpointResult},
    OUR_RUNTIME,
};

// Temporary instrumentation for the File::open latency hunt (pagerperf.md): splits the `obj` phase
// of an open into the mapping and the first touch of the meta page.
mod objstats {
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNT: AtomicU64 = AtomicU64::new(0);
    static MAP: AtomicU64 = AtomicU64::new(0);
    static META: AtomicU64 = AtomicU64::new(0);

    pub fn record(map: u64, meta: u64) {
        let n = COUNT.fetch_add(1, Ordering::Relaxed) + 1;
        let m = MAP.fetch_add(map, Ordering::Relaxed) + map;
        let e = META.fetch_add(meta, Ordering::Relaxed) + meta;
        // Every call, in ns; see OPENSTATS. On the open path's own switch, not the global one:
        // this splits `obj`, which the open measurement showed is ~80-90% of an open, so it has to
        // come along with that measurement rather than needing STATS_ON turned on for everything.
        secgate::statlog::record_on(
            super::super::openstats::OPEN_STATS,
            "OBJSTATS",
            n,
            &[map, meta],
        );
        let _ = (m, e);
    }
}

const RFI_NEEDS_SYNC: u64 = 1 << 0;
const RFI_HAS_KSYNC: u64 = 1 << 1;
struct RawFileInner {
    pos: AtomicU64,
    len: AtomicU64,
    flags: AtomicU64,
    /// The `MEXT_SIZED` extension, resolved once instead of searched for on every call.
    ///
    /// `update_len` runs at the top of every read, and `find_meta_ext` is a `SeqCst` scan of the
    /// meta table -- which sits at the far end of the object, so each call reaches a page a long
    /// way from the data about to be copied and pays a TLB and cache miss before any bytes move.
    /// An extension's slot does not move once allocated, so the pointer stays good for the life of
    /// the mapping and the length can be read with a single load.
    ///
    /// Null until resolved: a fresh object has no `MEXT_SIZED` until the first write creates one,
    /// so this is filled in lazily and re-checked while it is still null.
    sized: AtomicPtr<MetaExt>,
}

#[derive(Clone)]
pub struct RawFile {
    inner: Arc<RawFileInner>,
    handle: ObjectHandle,
}

impl RawFile {
    fn maybe_set_needs_sync(&self) {
        if self.handle.map_flags().contains(MapFlags::WRITE) {
            let old = self
                .inner
                .flags
                .fetch_or(RFI_NEEDS_SYNC | RFI_HAS_KSYNC, Ordering::SeqCst);
            if old & RFI_HAS_KSYNC == 0 {
                let mut sync_info = sync_info {
                    release_compare: 0,
                    release_set: 0,
                    release_ptr: core::ptr::null_mut(),
                    durable_ptr: core::ptr::null_mut(),
                    flags: SYNC_FLAG_ASYNC_DURABLE,
                    __resv: 0,
                };
                if let Err(e) = self.handle.cmd(ObjectCmd::Sync, &mut sync_info) {
                    tracing::error!(
                        "failed to set async durable sync on object {}: {:?}",
                        self.handle.id(),
                        e
                    );
                }
            }
        }
    }

    /// The `MEXT_SIZED` extension, from the cache when it has been resolved.
    ///
    /// Safety: the pointer is into the object's meta page, which stays mapped for as long as this
    /// `RawFile` holds its handle, and an extension slot does not move once allocated.
    fn sized_ext(&self) -> Option<&MetaExt> {
        let cached = self.inner.sized.load(Ordering::Relaxed);
        if !cached.is_null() {
            return Some(unsafe { &*cached });
        }
        let me = self.handle.find_meta_ext(MEXT_SIZED)?;
        self.inner
            .sized
            .store(me as *const MetaExt as *mut _, Ordering::Relaxed);
        Some(me)
    }

    fn update_len(&self) {
        if let Some(me) = self.sized_ext() {
            self.inner
                .len
                .store(me.value.load(Ordering::SeqCst), Ordering::SeqCst);
        }
    }

    pub fn open(obj_id: ObjID, flags: MapFlags) -> Result<Self> {
        let t_map = std::time::Instant::now();
        let handle = OUR_RUNTIME.map_object(obj_id, flags)?;
        let map_ns = t_map.elapsed().as_nanos() as u64;
        // First touch of the meta page: a fault, and on a cold object a pager round trip.
        let t_meta = std::time::Instant::now();
        let mut sized = handle
            .find_meta_ext(MEXT_SIZED)
            .map_or(null_mut(), |me| me as *const MetaExt as *mut MetaExt);
        let len = if let Some(me) = unsafe { sized.as_ref() } {
            me.value.load(Ordering::SeqCst)
        } else {
            if flags.contains(MapFlags::WRITE) {
                unsafe { handle.set_meta_ext(MetaExt::new(MEXT_SIZED, 0))? };
                // Created just now, so resolve it here and skip the lazy path on the first read.
                sized = handle
                    .find_meta_ext(MEXT_SIZED)
                    .map_or(null_mut(), |me| me as *const MetaExt as *mut MetaExt);
            }
            0
        };
        objstats::record(map_ns, t_meta.elapsed().as_nanos() as u64);
        Ok(Self {
            inner: Arc::new(RawFileInner {
                pos: AtomicU64::new(0),
                len: AtomicU64::new(len),
                flags: AtomicU64::new(0),
                sized: AtomicPtr::new(sized),
            }),
            handle,
        })
    }

    pub fn truncate(&self, new_len: u64) -> Result<()> {
        if new_len > (MAX_SIZE - NULLPAGE_SIZE) as u64 {
            return Err(ArgumentError::InvalidArgument.into());
        }
        self.inner.len.store(new_len, Ordering::SeqCst);
        let me = MetaExt::new(MEXT_SIZED, new_len);
        unsafe { self.handle.set_meta_ext(me)? };
        self.maybe_set_needs_sync();
        Ok(())
    }
}

impl Fd for RawFile {
    fn read(
        &self,
        buf: &mut [u8],
        _flags: twizzler_rt_abi::io::IoFlags,
        a_offset: Option<u64>,
        _ep: Option<&mut twizzler_rt_abi::io::Endpoint>,
    ) -> Result<usize> {
        self.update_len();
        let offset = a_offset.unwrap_or(self.inner.pos.load(Ordering::SeqCst));
        let len = self.inner.len.load(Ordering::SeqCst);
        if offset >= len {
            return Ok(0);
        }
        let copy_len = buf.len().min((len - offset) as usize);
        let data = unsafe {
            core::slice::from_raw_parts(
                self.handle.start().add(NULLPAGE_SIZE + offset as usize),
                copy_len,
            )
        };
        buf[0..copy_len].copy_from_slice(data);
        if a_offset.is_none() {
            self.inner
                .pos
                .store(offset + copy_len as u64, Ordering::SeqCst);
        }
        Ok(copy_len)
    }

    fn write(
        &self,
        buf: &[u8],
        _flags: twizzler_rt_abi::io::IoFlags,
        a_offset: Option<u64>,
        _to: Option<&twizzler_rt_abi::io::Endpoint>,
    ) -> Result<usize> {
        let offset = a_offset.unwrap_or(self.inner.pos.load(Ordering::SeqCst));
        let write_len = buf.len();
        let end_pos = offset + write_len as u64;
        if end_pos > (MAX_SIZE - NULLPAGE_SIZE) as u64 {
            return Err(TwzError::INVALID_ARGUMENT);
        }
        let len = self.inner.len.load(Ordering::SeqCst);
        if end_pos > len {
            self.inner.len.store(end_pos, Ordering::SeqCst);
            let me = twizzler_rt_abi::object::MetaExt::new(MEXT_SIZED, end_pos);
            unsafe { self.handle.set_meta_ext(me)? };
        }
        unsafe {
            let dest = self.handle.start().add(NULLPAGE_SIZE + offset as usize);
            core::ptr::copy_nonoverlapping(buf.as_ptr(), dest, write_len);
        }
        if a_offset.is_none() {
            self.inner
                .pos
                .store(offset + write_len as u64, Ordering::SeqCst);
        }
        self.maybe_set_needs_sync();
        Ok(write_len)
    }

    fn stat(&self) -> Result<FdInfo> {
        self.update_len();
        // Externally-backed objects carry the store's mtime in a meta ext (pager-synthesized);
        // native objects have none and report zero.
        let modified = self
            .handle
            .find_meta_ext(MEXT_MTIME)
            .map(|me| std::time::Duration::from_secs(me.value.load(Ordering::SeqCst)))
            .unwrap_or(std::time::Duration::ZERO);
        Ok(FdInfo {
            kind: twizzler_rt_abi::fd::FdKind::Regular,
            size: self.inner.len.load(Ordering::SeqCst),
            flags: twizzler_rt_abi::fd::FdFlags::empty(),
            id: self.handle.id().raw(),
            unix_mode: S_IFREG | S_IRWXG | S_IRWXO | S_IRWXU,
            accessed: std::time::Duration::ZERO,
            modified,
            created: std::time::Duration::ZERO,
        })
    }

    fn seek(&self, pos: SeekFrom) -> Result<usize> {
        self.update_len();
        let new_pos: i64 = match pos {
            SeekFrom::Start(x) => x as i64,
            SeekFrom::End(x) => (self.inner.len.load(Ordering::SeqCst) as i64) - x,
            SeekFrom::Current(x) => (self.inner.pos.load(Ordering::SeqCst) as i64) + x,
        };

        if new_pos < 0 {
            Err(ArgumentError::InvalidArgument.into())
        } else {
            self.inner.pos.store(new_pos as u64, Ordering::SeqCst);
            Ok(new_pos as usize)
        }
    }

    fn flush(&self) -> Result<()> {
        if self
            .inner
            .flags
            .fetch_add(!RFI_NEEDS_SYNC, Ordering::SeqCst)
            & RFI_NEEDS_SYNC
            != 0
        {
            self.handle.cmd(ObjectCmd::Sync, null_mut::<()>())?;
        }
        Ok(())
    }

    fn fd_cmd(&self, cmd: u32, arg: *const u8, _ret: *mut u8) -> Result<()> {
        match cmd {
            twizzler_rt_abi::bindings::FD_CMD_TRUNCATE => {
                let new_len = unsafe { *(arg as *const u64) };
                self.truncate(new_len)?;
                Ok(())
            }
            _ => Err(ArgumentError::InvalidArgument.into()),
        }
    }

    fn get_config(&self, _reg: u32, _val: *mut std::ffi::c_void, _val_len: usize) -> Result<()> {
        Err(ErrorKind::Unsupported.into())
    }

    fn set_config(&self, _reg: u32, _val: *const std::ffi::c_void, _val_len: usize) -> Result<()> {
        Err(ErrorKind::Unsupported.into())
    }

    fn waitpoint(&self, _kind: twizzler_rt_abi::bindings::wait_kind) -> Result<WaitpointResult> {
        if let Some(me) = self.handle.find_meta_ext(MEXT_SIZED) {
            let ready = self.inner.pos.load(Ordering::SeqCst) < me.value.load(Ordering::SeqCst);
            Ok(WaitpointResult {
                sleep: (&me.value, self.inner.pos.load(Ordering::SeqCst)).into(),
                also: None,
                ready,
                keepalive: None,
            })
        } else {
            Err(ErrorKind::Unsupported.into())
        }
    }

    fn shutdown(&self, _sh: std::net::Shutdown) -> Result<()> {
        if self.inner.flags.load(Ordering::SeqCst) & RFI_NEEDS_SYNC != 0 {
            let mut sync_info = sync_info {
                release_compare: 0,
                release_set: 0,
                release_ptr: core::ptr::null_mut(),
                durable_ptr: core::ptr::null_mut(),
                flags: SYNC_FLAG_ASYNC_DURABLE | SYNC_FLAG_DURABLE,
                __resv: 0,
            };
            if let Err(e) = self.handle.cmd(ObjectCmd::Sync, &mut sync_info) {
                tracing::error!(
                    "failed to set async durable sync on object {}: {:?}",
                    self.handle.id(),
                    e
                );
            }
        }
        Ok(())
    }
}
