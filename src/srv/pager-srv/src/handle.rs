use std::{io::ErrorKind, path::PathBuf};

use secgate::{
    secure_gate,
    util::{Descriptor, SimpleBuffer},
};
use twizzler::object::ObjID;
use twizzler_abi::syscall::{
    sys_object_create, BackingType, LifetimeType, ObjectCreate, ObjectCreateFlags,
};
use twizzler_rt_abi::object::MapFlags;

use crate::PAGER_CTX;

// Per-client metadata.
pub(crate) struct PagerClient {
    buffer: SimpleBuffer,
}

impl PagerClient {
    fn sbid(&self) -> ObjID {
        self.buffer.handle().id()
    }
}

impl PagerClient {
    pub fn new() -> Option<Self> {
        // Create and map a handle for the simple buffer.
        let id = sys_object_create(
            ObjectCreate::new(
                BackingType::Normal,
                LifetimeType::Volatile,
                None,
                ObjectCreateFlags::empty(),
            ),
            &[],
            &[],
        )
        .ok()?;
        let handle =
            twizzler_rt_abi::object::twz_rt_map_object(id, MapFlags::WRITE | MapFlags::READ)
                .ok()?;
        let buffer = SimpleBuffer::new(handle);
        Some(Self { buffer })
    }

    fn read_buffer(&self, name_len: usize) -> Result<PathBuf, ErrorKind> {
        let mut buf = vec![0; name_len];
        self.buffer.read(&mut buf);
        Ok(PathBuf::from(
            String::from_utf8(buf).map_err(|_| ErrorKind::InvalidFilename)?,
        ))
    }
}

#[secure_gate(options(info))]
pub fn pager_open_handle(info: &secgate::GateCallInfo) -> Option<(Descriptor, ObjID)> {
    let comp = info.source_context().unwrap_or(0.into());
    let pager = &PAGER_CTX.get().unwrap().data;
    let handle = pager.new_handle(comp)?;
    let id = pager.with_handle(comp, handle, |pc| pc.sbid())?;

    Some((handle, id))
}

#[secure_gate(options(info))]
pub fn pager_close_handle(info: &secgate::GateCallInfo, desc: Descriptor) {
    let comp = info.source_context().unwrap_or(0.into());
    let pager = &PAGER_CTX.get().unwrap().data;
    pager.drop_handle(comp, desc);
}

pub const PATH_EXTERNAL_MAX: usize = 4096;
#[secure_gate(options(info))]
pub fn pager_enumerate_external(
    info: &secgate::GateCallInfo,
    desc: Descriptor,
    name_len: usize,
) -> Result<usize, ErrorKind> {
    let comp = info.source_context().unwrap_or(0.into());
    let pager = &PAGER_CTX.get().unwrap();
    let path = pager
        .data
        .with_handle(comp, desc, |pc| pc.read_buffer(name_len))
        .ok_or(ErrorKind::InvalidInput)??;

    let items = pager.enumerate_external(path).map_err(|e| e.kind())?;

    pager
        .data
        .with_handle_mut(comp, desc, |pc| {
            let mut len = 0;
            for (idx, item) in items.iter().enumerate() {
                let bytes = &(**item);
                len += pc.buffer.write_offset(bytes, idx * PATH_EXTERNAL_MAX);
            }
            len
        })
        .ok_or(ErrorKind::InvalidInput)
}

#[secure_gate(options(info))]
pub fn pager_stat_external(
    info: &secgate::GateCallInfo,
    desc: Descriptor,
    name_len: usize,
) -> Result<(ObjID, bool), ErrorKind> {
    let comp = info.source_context().unwrap_or(0.into());
    let pager = &PAGER_CTX.get().unwrap();
    let path = pager
        .data
        .with_handle(comp, desc, |pc| pc.read_buffer(name_len))
        .ok_or(ErrorKind::InvalidInput)??;

    pager
        .paged_ostore
        .open_external(path.as_path())
        .map(|x| (x.0.into(), x.1))
        .map_err(|e| e.kind())
}
