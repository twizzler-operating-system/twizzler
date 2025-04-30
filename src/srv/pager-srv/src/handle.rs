
use object_store::ExternalFile;
use secgate::{
    secure_gate,
    util::{Descriptor, SimpleBuffer},
};
use twizzler::object::ObjID;
use twizzler_abi::syscall::{
    sys_object_create, BackingType, LifetimeType, ObjectCreate, ObjectCreateFlags,
};
use twizzler_rt_abi::{error::TwzError, object::MapFlags};

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
    pub fn new() -> Result<Self, TwzError> {
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
        )?;
        let handle =
            twizzler_rt_abi::object::twz_rt_map_object(id, MapFlags::WRITE | MapFlags::READ)?;
        let buffer = SimpleBuffer::new(handle);
        Ok(Self { buffer })
    }
}

#[secure_gate(options(info))]
pub fn pager_open_handle(info: &secgate::GateCallInfo) -> Result<(Descriptor, ObjID), TwzError> {
    let comp = info.source_context().unwrap_or(0.into());
    let pager = &PAGER_CTX.get().unwrap().data;
    let handle = pager.new_handle(comp)?;
    let id = pager.with_handle(comp, handle, |pc| pc.sbid())?;

    Ok((handle, id))
}

#[secure_gate(options(info))]
pub fn pager_close_handle(info: &secgate::GateCallInfo, desc: Descriptor) -> Result<(), TwzError> {
    let comp = info.source_context().unwrap_or(0.into());
    let pager = &PAGER_CTX.get().unwrap().data;
    pager.drop_handle(comp, desc);
    Ok(())
}

#[secure_gate(options(info))]
pub fn pager_enumerate_external(
    info: &secgate::GateCallInfo,
    desc: Descriptor,
    id: ObjID,
) -> Result<usize, TwzError> {
    let comp = info.source_context().unwrap_or(0.into());
    let pager = &PAGER_CTX.get().unwrap();

    let items = pager.enumerate_external(id)?;

    pager
        .data
        .with_handle_mut(comp, desc, |pc| {
            let mut len = 0;
            for (idx, item) in items.iter().enumerate() {
                let ptr = item as *const ExternalFile;
                let bytes = unsafe {
                    core::slice::from_raw_parts(ptr.cast::<u8>(), size_of::<ExternalFile>())
                };
                len += pc
                    .buffer
                    .write_offset(bytes, idx * size_of::<ExternalFile>());
            }
            len
        })
        .ok_or(TwzError::INVALID_ARGUMENT)
}
