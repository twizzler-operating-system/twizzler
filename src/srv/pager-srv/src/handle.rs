use std::sync::Mutex;

use object_store::{
    ExternalFile, ExternalFileSbHdr, ExternalFileStore, ExternalOpenFlags, PagedObjectStore,
};
use secgate::util::{Descriptor, SimpleBuffer};
use twizzler::object::{ObjID, ObjectHandle};
use twizzler_abi::{
    object::Protections,
    syscall::{
        sys_object_create, BackingType, CreateTieFlags, CreateTieSpec, LifetimeType, ObjectCreate,
        ObjectCreateFlags,
    },
};
use twizzler_rt_abi::{bindings::NAME_DATA_MAX, error::TwzError, object::MapFlags};

use crate::PAGER_CTX;

// Per-client metadata.
pub(crate) struct PagerClient {
    buffer: SimpleBuffer,
}

impl PagerClient {
    fn sbid(&self) -> ObjID {
        self.buffer.handle().id()
    }

    pub fn into_handle(self) -> ObjectHandle {
        self.buffer.into_handle()
    }
}

struct SbObjects {
    objs: Vec<ObjectHandle>,
}

static SB_OBJECTS: Mutex<SbObjects> = Mutex::new(SbObjects { objs: Vec::new() });

pub fn get_sb_object(instance: ObjID) -> Result<ObjectHandle, TwzError> {
    let mut sbo = SB_OBJECTS.lock().unwrap();
    if sbo.objs.len() == 0 {
        drop(sbo);
        // Create and map a handle for the simple buffer.
        let id = sys_object_create(
            ObjectCreate::new(
                BackingType::Normal,
                LifetimeType::Volatile,
                None,
                ObjectCreateFlags::DELETE,
                Protections::all(),
            ),
            &[],
            &[CreateTieSpec::new(instance, CreateTieFlags::empty()).into()],
        )?;
        let handle =
            twizzler_rt_abi::object::twz_rt_map_object(id, MapFlags::WRITE | MapFlags::READ)?;
        return Ok(handle);
    }

    let next = sbo.objs.pop().unwrap();
    // TODO: discard all object pages.
    Ok(next)
}

pub fn release_sb_object(obj: ObjectHandle) {
    let mut sbo = SB_OBJECTS.lock().unwrap();
    sbo.objs.push(obj);
}

impl PagerClient {
    pub fn new(instance: ObjID) -> Result<Self, TwzError> {
        let handle = get_sb_object(instance)?;
        let buffer = SimpleBuffer::new(handle);
        Ok(Self { buffer })
    }
}

#[secgate::entry(lib = "pager")]
pub fn pager_open_handle() -> Result<(Descriptor, ObjID), TwzError> {
    let t_body = std::time::Instant::now();
    let info = secgate::get_caller().ok_or(TwzError::INVALID_ARGUMENT)?;
    let comp = info.source_context().unwrap_or(0.into());
    let pager = &PAGER_CTX.get().unwrap().data;
    let handle = pager.new_handle(comp)?;
    let id = pager.with_handle(comp, handle, |pc| pc.sbid())?;
    bodystats::record(t_body.elapsed().as_nanos() as u64);

    Ok((handle, id))
}

// Temporary instrumentation for the File::open latency hunt (pagerperf.md).
mod bodystats {
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNT: AtomicU64 = AtomicU64::new(0);
    static BODY: AtomicU64 = AtomicU64::new(0);

    pub fn record(body: u64) {
        let n = COUNT.fetch_add(1, Ordering::Relaxed) + 1;
        let b = BODY.fetch_add(body, Ordering::Relaxed) + body;
        if n.is_power_of_two() && crate::watchdog::diag_enabled() {
            twizzler_abi::klog_println!(
                "OPENHANDLESTATS {} open-handle bodies: {} us total",
                n,
                b / 1000,
            );
        }
    }
}

#[secgate::entry(lib = "pager")]
pub fn pager_close_handle(desc: Descriptor) -> Result<(), TwzError> {
    let info = secgate::get_caller().ok_or(TwzError::INVALID_ARGUMENT)?;
    let comp = info.source_context().unwrap_or(0.into());
    let pager = &PAGER_CTX.get().unwrap().data;
    if let Some(oh) = pager.drop_handle(comp, desc) {
        release_sb_object(oh);
    }
    Ok(())
}

fn write_external_file_to_sb(sb: &mut SimpleBuffer, file: &ExternalFile, off: usize) -> usize {
    let ext_file_hdr = ExternalFileSbHdr {
        pathlen: file.path.as_os_str().as_encoded_bytes().len() as u32,
        kind: file.kind,
        id: file.id,
    };
    let ptr = &ext_file_hdr as *const ExternalFileSbHdr as *const u8;
    let bytes = unsafe { core::slice::from_raw_parts(ptr, size_of::<ExternalFileSbHdr>()) };
    let thislen = sb.write_offset(bytes, off);
    let pathlen = sb.write_offset(file.path.as_os_str().as_encoded_bytes(), off + thislen);
    thislen + pathlen
}

#[secgate::entry(lib = "pager")]
pub fn pager_enumerate_external(
    desc: Descriptor,
    id: ObjID,
    skip: usize,
    count: usize,
) -> Result<usize, TwzError> {
    let info = secgate::get_caller().ok_or(TwzError::INVALID_ARGUMENT)?;
    let comp = info.source_context().unwrap_or(0.into());
    let pager = &PAGER_CTX.get().unwrap();

    let mut entries: Vec<ExternalFile> = Vec::new();
    pager
        .paged_ostore(None)?
        .readdir_external(id.raw(), skip, count, &mut entries)?;

    pager
        .data
        .with_handle_mut(comp, desc, |pc| {
            let mut len = 0;
            for item in entries.iter() {
                len += write_external_file_to_sb(&mut pc.buffer, item, len);
            }
            len
        })
        .ok_or(TwzError::INVALID_ARGUMENT)
}

#[secgate::entry(lib = "pager")]
pub fn pager_lookup_external(
    desc: Descriptor,
    id: ObjID,
    namelen: usize,
) -> Result<usize, TwzError> {
    // Roughly one call per compartment load, which is the axis the census needs to resolve.
    crate::heapdiag::tick();
    tracing::trace!(
        "looking up name in external namespace {} (namelen {})",
        id,
        namelen
    );
    let info = secgate::get_caller().ok_or(TwzError::INVALID_ARGUMENT)?;
    let comp = info.source_context().unwrap_or(0.into());
    let pager = &PAGER_CTX.get().unwrap();

    let mut namebuf = [0u8; NAME_DATA_MAX];
    let namelen = pager
        .data
        .with_handle(comp, desc, |pc| pc.buffer.read(&mut namebuf[0..namelen]))?;
    let name =
        str::from_utf8(namebuf[..namelen].as_ref()).map_err(|_| TwzError::INVALID_ARGUMENT)?;

    let t_store = std::time::Instant::now();
    let file = pager.paged_ostore(None)?.open_external(
        Some(id.raw()),
        name,
        ExternalOpenFlags::READ,
        0,
        None,
    )?;
    lookupstats::record(t_store.elapsed().as_nanos() as u64);

    pager
        .data
        .with_handle_mut(comp, desc, |pc| {
            write_external_file_to_sb(&mut pc.buffer, &file, 0)
        })
        .ok_or(TwzError::INVALID_ARGUMENT)
}

// Temporary instrumentation for the File::open latency hunt (pagerperf.md).
mod lookupstats {
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNT: AtomicU64 = AtomicU64::new(0);
    static STORE: AtomicU64 = AtomicU64::new(0);

    pub fn record(store: u64) {
        let n = COUNT.fetch_add(1, Ordering::Relaxed) + 1;
        let s = STORE.fetch_add(store, Ordering::Relaxed) + store;
        if n.is_power_of_two() && crate::watchdog::diag_enabled() {
            twizzler_abi::klog_println!(
                "LOOKUPSTATS {} external lookups: store {} us (per lookup: {} us)",
                n,
                s / 1000,
                s / (n * 1000),
            );
        }
    }
}

#[secgate::entry(lib = "pager")]
pub fn pager_create_external(
    desc: Descriptor,
    dir: ObjID,
    mode: libc::mode_t,
    namelen: usize,
    link_to: Option<ObjID>,
) -> Result<usize, TwzError> {
    let info = secgate::get_caller().ok_or(TwzError::INVALID_ARGUMENT)?;
    let comp = info.source_context().unwrap_or(0.into());
    let pager = &PAGER_CTX.get().unwrap();

    let mut namebuf = [0u8; NAME_DATA_MAX];
    let namelen = pager
        .data
        .with_handle(comp, desc, |pc| pc.buffer.read(&mut namebuf[0..namelen]))?;
    let name =
        str::from_utf8(namebuf[..namelen].as_ref()).map_err(|_| TwzError::INVALID_ARGUMENT)?;

    let file = pager.paged_ostore(None)?.open_external(
        Some(dir.raw()),
        name,
        ExternalOpenFlags::CREATE,
        mode,
        link_to.map(|x| x.raw()),
    )?;

    pager
        .data
        .with_handle_mut(comp, desc, |pc| {
            write_external_file_to_sb(&mut pc.buffer, &file, 0)
        })
        .ok_or(TwzError::INVALID_ARGUMENT)
}

#[secgate::entry(lib = "pager")]
pub fn pager_set_mtime_external(id: ObjID, mtime: u64) -> Result<(), TwzError> {
    let pager = &PAGER_CTX.get().unwrap();
    pager
        .paged_ostore(None)?
        .set_mtime(id.raw(), mtime as u32)?;
    Ok(())
}

/// The store's link count for external object `id`, which is the authority the synthesized meta
/// page's `MEXT_NLINK` is a cache of.
#[secgate::entry(lib = "pager")]
pub fn pager_nlink_external(id: ObjID) -> Result<u32, TwzError> {
    let pager = &PAGER_CTX.get().unwrap();
    Ok(pager.paged_ostore(None)?.nlink(id.raw())?)
}

#[secgate::entry(lib = "pager")]
pub fn pager_unlink_external(desc: Descriptor, dir: ObjID, namelen: usize) -> Result<(), TwzError> {
    let info = secgate::get_caller().ok_or(TwzError::INVALID_ARGUMENT)?;
    let comp = info.source_context().unwrap_or(0.into());
    let pager = &PAGER_CTX.get().unwrap();

    let mut namebuf = [0u8; NAME_DATA_MAX];
    let namelen = pager
        .data
        .with_handle(comp, desc, |pc| pc.buffer.read(&mut namebuf[0..namelen]))?;
    let name =
        str::from_utf8(namebuf[..namelen].as_ref()).map_err(|_| TwzError::INVALID_ARGUMENT)?;

    pager
        .paged_ostore(None)?
        .unlink_external(Some(dir.raw()), name)?;

    Ok(())
}

#[secgate::entry(lib = "pager")]
pub fn pager_readlink_external(desc: Descriptor, id: ObjID) -> Result<usize, TwzError> {
    let info = secgate::get_caller().ok_or(TwzError::INVALID_ARGUMENT)?;
    let comp = info.source_context().unwrap_or(0.into());
    let pager = &PAGER_CTX.get().unwrap();

    let name = pager.paged_ostore(None)?.readlink_external(id.raw())?;
    let namelen = pager
        .data
        .with_handle_mut(comp, desc, |pc| pc.buffer.write(name.as_bytes()))
        .ok_or(TwzError::INVALID_ARGUMENT)?;

    Ok(namelen)
}
