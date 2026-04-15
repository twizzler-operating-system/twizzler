use std::sync::Mutex;

use object_store::{ExternalFile, ExternalFileSbHdr, ExternalFileStore, ExternalOpenFlags};
use secgate::util::{Descriptor, SimpleBuffer};
use twizzler::object::{ObjID, ObjectHandle};
use twizzler_abi::{
    object::Protections,
    syscall::{sys_object_create, BackingType, LifetimeType, ObjectCreate, ObjectCreateFlags},
};
use twizzler_rt_abi::{bindings::NAME_DATA_MAX, error::TwzError, object::MapFlags};

use crate::{threads::run_async, PAGER_CTX};

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

pub fn get_sb_object() -> Result<ObjectHandle, TwzError> {
    let mut sbo = SB_OBJECTS.lock().unwrap();
    if sbo.objs.len() == 0 {
        drop(sbo);
        // Create and map a handle for the simple buffer.
        let id = sys_object_create(
            ObjectCreate::new(
                BackingType::Normal,
                LifetimeType::Volatile,
                None,
                ObjectCreateFlags::empty(),
                Protections::all(),
            ),
            &[],
            &[],
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
    pub fn new() -> Result<Self, TwzError> {
        let handle = get_sb_object()?;
        let buffer = SimpleBuffer::new(handle);
        Ok(Self { buffer })
    }
}

#[secgate::entry(lib = "pager")]
pub fn pager_open_handle() -> Result<(Descriptor, ObjID), TwzError> {
    let info = secgate::get_caller().ok_or(TwzError::INVALID_ARGUMENT)?;
    let comp = info.source_context().unwrap_or(0.into());
    let pager = &PAGER_CTX.get().unwrap().data;
    let handle = pager.new_handle(comp)?;
    let id = pager.with_handle(comp, handle, |pc| pc.sbid())?;

    Ok((handle, id))
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
pub fn pager_enumerate_external(desc: Descriptor, id: ObjID) -> Result<usize, TwzError> {
    let info = secgate::get_caller().ok_or(TwzError::INVALID_ARGUMENT)?;
    let comp = info.source_context().unwrap_or(0.into());
    let pager = &PAGER_CTX.get().unwrap();

    let mut entries: Vec<ExternalFile> = Vec::new();
    run_async(
        pager
            .paged_ostore(None)?
            .readdir_external(id.raw(), &mut entries),
    )?;

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
    let info = secgate::get_caller().ok_or(TwzError::INVALID_ARGUMENT)?;
    let comp = info.source_context().unwrap_or(0.into());
    let pager = &PAGER_CTX.get().unwrap();

    let mut namebuf = [0u8; NAME_DATA_MAX];
    let namelen = pager
        .data
        .with_handle(comp, desc, |pc| pc.buffer.read(&mut namebuf[0..namelen]))?;
    let name =
        str::from_utf8(namebuf[..namelen].as_ref()).map_err(|_| TwzError::INVALID_ARGUMENT)?;

    let file = run_async(pager.paged_ostore(None)?.open_external(
        Some(id.raw()),
        name,
        ExternalOpenFlags::empty(),
        0,
    ))?;

    pager
        .data
        .with_handle_mut(comp, desc, |pc| {
            write_external_file_to_sb(&mut pc.buffer, &file, 0)
        })
        .ok_or(TwzError::INVALID_ARGUMENT)
}

#[secgate::entry(lib = "pager")]
pub fn pager_create_external(
    desc: Descriptor,
    dir: ObjID,
    mode: libc::mode_t,
    namelen: usize,
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

    let file = run_async(pager.paged_ostore(None)?.open_external(
        Some(dir.raw()),
        name,
        ExternalOpenFlags::CREATE,
        mode,
    ))?;

    pager
        .data
        .with_handle_mut(comp, desc, |pc| {
            write_external_file_to_sb(&mut pc.buffer, &file, 0)
        })
        .ok_or(TwzError::INVALID_ARGUMENT)
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

    run_async(
        pager
            .paged_ostore(None)?
            .unlink_external(Some(dir.raw()), name),
    )?;

    Ok(())
}
