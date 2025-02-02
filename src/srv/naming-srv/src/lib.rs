#![feature(naked_functions)]
#![feature(linkage)]
#[warn(unused_variables)]
use std::sync::Mutex;

use lazy_static::lazy_static;
use lazy_init::LazyTransform;
use naming_core::{Entry, ErrorKind, NameSession, NameStore, Result};
use secgate::{
    secure_gate,
    util::{Descriptor, HandleMgr, SimpleBuffer},
};
use twizzler_abi::{
    aux::KernelInitInfo,
    object::{MAX_SIZE, NULLPAGE_SIZE},
    syscall::{sys_object_create, BackingType, LifetimeType, ObjectCreate, ObjectCreateFlags},
};
use twizzler_rt_abi::object::{MapFlags, ObjID};


struct NamespaceClient<'a> {
    session: NameSession<'a>,
    buffer: SimpleBuffer,
}

impl<'a> NamespaceClient<'a> {
    fn new(session: NameSession<'a>) -> Option<Self> {
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
        Some(Self { session, buffer })
    }

    fn sbid(&self) -> ObjID {
        self.buffer.handle().id()
    }
}

unsafe impl Send for Namer<'_> {}
unsafe impl Sync for Namer<'_> {}

struct Namer<'a> {
    handles: Mutex<HandleMgr<NamespaceClient<'a>>>,
    names: NameStore,
}

impl Namer<'_> {
    fn new() -> Self {
        Self {
            handles: Mutex::new(HandleMgr::new(None)),
            names: NameStore::new(),
        }
    }

    fn new_in(id: ObjID) -> Result<Self> {
        let names = NameStore::new_in(id)?;
        Ok(Self {
            handles: Mutex::new(HandleMgr::new(None)),
            names: names
        })
    }
}

lazy_static! {
    static ref NAMINGSERVICE: LazyTransform<(), Namer<'static>> = LazyTransform::new(());
}

// How would this work if I changed the root while handles were open?
#[secure_gate(options(info))]
pub fn namer_start(_info: &secgate::GateCallInfo, bootstrap: ObjID) {
    NAMINGSERVICE.get_or_create(|_| {
        Namer::new_in(bootstrap).or::<ErrorKind>(Ok(Namer::new())).unwrap()
    });
}

#[secure_gate(options(info))]
pub fn open_handle(info: &secgate::GateCallInfo) -> Option<(Descriptor, ObjID)> {
    let service = NAMINGSERVICE.get()?;
    let mut binding = service.handles.lock().unwrap();

    let session = service.names.root_session();
    let client = NamespaceClient::new(session)?;
    let id = client.sbid();

    let desc = binding.insert(info.source_context().unwrap_or(0.into()), client)?;

    Some((desc, id))
}

#[secure_gate(options(info))]
pub fn close_handle(info: &secgate::GateCallInfo, desc: Descriptor) {
    let service = NAMINGSERVICE.get().unwrap();

    let mut binding = service.handles.lock().unwrap();

    binding.remove(info.source_context().unwrap_or(0.into()), desc);
}

#[secure_gate(options(info))]
pub fn put(info: &secgate::GateCallInfo, desc: Descriptor) -> Result<()> {
    let service = NAMINGSERVICE.get().unwrap();
    let mut binding = service.handles.lock().unwrap();
    let client = binding
        .lookup_mut(info.source_context().unwrap_or(0.into()), desc)
        .ok_or(ErrorKind::Other)?;

    let mut buf = [0u8; std::mem::size_of::<Entry>()];
    client.buffer.read(&mut buf);
    let provided = unsafe { std::mem::transmute::<[u8; std::mem::size_of::<Entry>()], Entry>(buf) };

    client.session.put(provided.name, provided.entry_type)
}

#[secure_gate(options(info))]
pub fn get(info: &secgate::GateCallInfo, desc: Descriptor) -> Result<Entry> {
    let service = NAMINGSERVICE.get().unwrap();
    let mut binding = service.handles.lock().unwrap();
    let client = binding
        .lookup_mut(info.source_context().unwrap_or(0.into()), desc)
        .ok_or(ErrorKind::Other)?;

    let mut buf = [0u8; std::mem::size_of::<Entry>()];
    client.buffer.read(&mut buf);
    let provided = unsafe { std::mem::transmute::<[u8; std::mem::size_of::<Entry>()], Entry>(buf) };

    let entry = client.session.get(provided.name)?;

    Ok(entry)
}

#[secure_gate(options(info))]
pub fn remove(info: &secgate::GateCallInfo, desc: Descriptor, recursive: bool) -> Result<()> {
    let service = NAMINGSERVICE.get().unwrap();
    let mut binding = service.handles.lock().unwrap();
    let client = binding
        .lookup_mut(info.source_context().unwrap_or(0.into()), desc)
        .ok_or(ErrorKind::Other)?;

    let mut buf = [0u8; std::mem::size_of::<Entry>()];
    client.buffer.read(&mut buf);
    let provided = unsafe { std::mem::transmute::<[u8; std::mem::size_of::<Entry>()], Entry>(buf) };

    client.session.remove(provided.name, recursive)?;

    Ok(())
}

#[secure_gate(options(info))]
pub fn enumerate_names(info: &secgate::GateCallInfo, desc: Descriptor) -> Result<usize> {
    let service = NAMINGSERVICE.get().unwrap();
    let mut binding = service.handles.lock().unwrap();
    let client = binding
        .lookup_mut(info.source_context().unwrap_or(0.into()), desc)
        .ok_or(ErrorKind::Other)?;

    let mut buf = [0u8; std::mem::size_of::<Entry>()];
    client.buffer.read(&mut buf);
    let provided = unsafe { std::mem::transmute::<[u8; std::mem::size_of::<Entry>()], Entry>(buf) };

    // TODO: make not bad
    let vec1 = client.session.enumerate_namespace(provided.name)?;
    let len = vec1.len();

    let mut buffer = SimpleBuffer::new(client.buffer.handle().clone());
    let slice = unsafe {
        std::slice::from_raw_parts(
            vec1.as_ptr() as *const u8,
            len * std::mem::size_of::<Entry>(),
        )
    };
    buffer.write(slice);

    Ok(len)
}

#[secure_gate(options(info))]
pub fn change_namespace(info: &secgate::GateCallInfo, desc: Descriptor) -> Result<()> {
    let service = NAMINGSERVICE.get().unwrap();
    let mut binding = service.handles.lock().unwrap();
    let client = binding
        .lookup_mut(info.source_context().unwrap_or(0.into()), desc)
        .ok_or(ErrorKind::Other)?;

    let mut buf = [0u8; std::mem::size_of::<Entry>()];
    client.buffer.read(&mut buf);
    let provided = unsafe { std::mem::transmute::<[u8; std::mem::size_of::<Entry>()], Entry>(buf) };

    client.session.change_namespace(provided.name)
}
