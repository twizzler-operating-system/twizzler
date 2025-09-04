#![feature(naked_functions)]
#![feature(linkage)]
#![feature(io_error_more)]
#[warn(unused_variables)]
use std::sync::Mutex;
use std::{io::ErrorKind, path::PathBuf};

use lazy_init::LazyTransform;
use lazy_static::lazy_static;
use naming_core::{GetFlags, NameSession, NameStore, NsNode, Result, PATH_MAX};
use secgate::{
    secure_gate,
    util::{Descriptor, HandleMgr, SimpleBuffer},
};
use tracing::Level;
use twizzler::object::ObjectHandle;
use twizzler_abi::{
    aux::KernelInitInfo,
    object::{Protections, MAX_SIZE, NULLPAGE_SIZE},
    syscall::{sys_object_create, BackingType, LifetimeType, ObjectCreate, ObjectCreateFlags},
};
use twizzler_rt_abi::{
    error::{ArgumentError, ResourceError},
    object::{MapFlags, ObjID},
};

struct SbObjects {
    objs: Vec<ObjectHandle>,
}

static SB_OBJECTS: Mutex<SbObjects> = Mutex::new(SbObjects { objs: Vec::new() });

pub fn get_sb_object() -> Result<ObjectHandle> {
    let mut sbo = SB_OBJECTS.lock().unwrap();
    if sbo.objs.len() == 0 {
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

struct NamespaceClient<'a> {
    session: NameSession<'a>,
    buffer: SimpleBuffer,
}

impl<'a> NamespaceClient<'a> {
    fn new(session: NameSession<'a>) -> Option<Self> {
        // Create and map a handle for the simple buffer.
        let handle = get_sb_object().ok()?;
        let buffer = SimpleBuffer::new(handle);
        Some(Self { session, buffer })
    }

    fn sbid(&self) -> ObjID {
        self.buffer.handle().id()
    }

    fn read_buffer(&self, name_len: usize) -> Result<PathBuf> {
        if name_len >= PATH_MAX {
            return Err(ArgumentError::InvalidArgument.into());
        }
        let mut buf = vec![0; name_len];
        self.buffer.read(&mut buf);
        Ok(PathBuf::from(
            String::from_utf8(buf).map_err(|_| ErrorKind::InvalidFilename)?,
        ))
    }

    fn read_buffer_at(&self, name_len: usize, off: usize) -> Result<PathBuf> {
        if name_len >= PATH_MAX {
            return Err(ArgumentError::InvalidArgument.into());
        }
        let mut buf = vec![0; name_len];
        self.buffer.read_offset(&mut buf, off);
        Ok(PathBuf::from(
            String::from_utf8(buf).map_err(|_| ArgumentError::InvalidArgument)?,
        ))
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

    fn new_with(id: ObjID) -> Result<Self> {
        let names = NameStore::new_with(id)?;
        Ok(Self {
            handles: Mutex::new(HandleMgr::new(None)),
            names,
        })
    }
}

lazy_static! {
    static ref NAMINGSERVICE: LazyTransform<(), Namer<'static>> = LazyTransform::new(());
}

fn get_kernel_init_info() -> &'static KernelInitInfo {
    unsafe {
        (((twizzler_abi::slot::RESERVED_KERNEL_INIT * MAX_SIZE) + NULLPAGE_SIZE)
            as *const KernelInitInfo)
            .as_ref()
            .unwrap()
    }
}

// How would this work if I changed the root while handles were open?
#[secure_gate(options(info))]
pub fn namer_start(_info: &secgate::GateCallInfo, bootstrap: ObjID) -> Result<ObjID> {
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_max_level(Level::INFO)
            .without_time()
            .finish(),
    )
    .unwrap();

    Ok(NAMINGSERVICE
        .get_or_create(|_| {
            let namer = Namer::new_with(bootstrap)
                .or::<ErrorKind>(Ok(Namer::new()))
                .unwrap();
            namer.names.root_session().mkns("/initrd", false).unwrap();
            for n in get_kernel_init_info().names() {
                namer
                    .names
                    .root_session()
                    .put(&format!("/initrd/{}", n.name()), n.id())
                    .unwrap();
            }

            namer
        })
        .names
        .id())
}

#[secure_gate(options(info))]
pub fn open_handle(info: &secgate::GateCallInfo) -> Result<(Descriptor, ObjID)> {
    let service = NAMINGSERVICE.get().ok_or(ResourceError::Unavailable)?;
    let mut binding = service.handles.lock().unwrap();

    let session = service.names.root_session();
    let client = NamespaceClient::new(session).ok_or(ResourceError::Unavailable)?;
    let id = client.sbid();

    let desc = binding
        .insert(info.source_context().unwrap_or(0.into()), client)
        .ok_or(ResourceError::OutOfResources)?;

    Ok((desc, id))
}

#[secure_gate(options(info))]
pub fn close_handle(info: &secgate::GateCallInfo, desc: Descriptor) -> Result<()> {
    let service = NAMINGSERVICE.get().unwrap();

    let mut binding = service.handles.lock().unwrap();

    if let Some(client) = binding.remove(info.source_context().unwrap_or(0.into()), desc) {
        release_sb_object(client.buffer.into_handle());
    }

    Ok(())
}

#[secure_gate(options(info))]
pub fn put(
    info: &secgate::GateCallInfo,
    desc: Descriptor,
    name_len: usize,
    id: ObjID,
) -> Result<()> {
    let service = NAMINGSERVICE.get().unwrap();
    let mut binding = service.handles.lock().unwrap();
    let client = binding
        .lookup_mut(info.source_context().unwrap_or(0.into()), desc)
        .ok_or(ArgumentError::BadHandle)?;

    let path = client.read_buffer(name_len)?;

    client.session.put(path, id)
}

#[secure_gate(options(info))]
pub fn mkns(
    info: &secgate::GateCallInfo,
    desc: Descriptor,
    name_len: usize,
    persist: bool,
) -> Result<()> {
    let service = NAMINGSERVICE.get().unwrap();
    let mut binding = service.handles.lock().unwrap();
    let client = binding
        .lookup_mut(info.source_context().unwrap_or(0.into()), desc)
        .ok_or(ArgumentError::BadHandle)?;

    let path = client.read_buffer(name_len)?;

    client.session.mkns(path, persist)
}

#[secure_gate(options(info))]
pub fn link(
    info: &secgate::GateCallInfo,
    desc: Descriptor,
    name_len: usize,
    link_len: usize,
) -> Result<()> {
    let service = NAMINGSERVICE.get().unwrap();
    let mut binding = service.handles.lock().unwrap();
    let client = binding
        .lookup_mut(info.source_context().unwrap_or(0.into()), desc)
        .ok_or(ErrorKind::Other)?;

    let path = client.read_buffer(name_len)?;
    let link = client.read_buffer_at(link_len, name_len)?;

    client.session.link(path, link)
}

#[secure_gate(options(info))]
pub fn get(
    info: &secgate::GateCallInfo,
    desc: Descriptor,
    name_len: usize,
    flags: GetFlags,
) -> Result<NsNode> {
    let service = NAMINGSERVICE.get().unwrap();
    let mut binding = service.handles.lock().unwrap();
    let client = binding
        .lookup_mut(info.source_context().unwrap_or(0.into()), desc)
        .ok_or(ErrorKind::Other)?;

    let path = client.read_buffer(name_len)?;

    client.session.get(path, flags)
}

#[secure_gate(options(info))]
pub fn remove(info: &secgate::GateCallInfo, desc: Descriptor, name_len: usize) -> Result<()> {
    let service = NAMINGSERVICE.get().unwrap();
    let mut binding = service.handles.lock().unwrap();
    let client = binding
        .lookup_mut(info.source_context().unwrap_or(0.into()), desc)
        .ok_or(ErrorKind::Other)?;

    let path = client.read_buffer(name_len)?;

    client.session.remove(path)?;

    Ok(())
}

#[secure_gate(options(info))]
pub fn enumerate_names(
    info: &secgate::GateCallInfo,
    desc: Descriptor,
    name_len: usize,
) -> Result<usize> {
    let service = NAMINGSERVICE.get().unwrap();
    let mut binding = service.handles.lock().unwrap();
    let client = binding
        .lookup_mut(info.source_context().unwrap_or(0.into()), desc)
        .ok_or(ErrorKind::Other)?;

    let path = client.read_buffer(name_len)?;

    // TODO: make not bad
    let vec1 = client.session.enumerate_namespace(path)?;
    let len = vec1.len();

    let mut buffer = SimpleBuffer::new(client.buffer.handle().clone());
    let slice = unsafe {
        std::slice::from_raw_parts(
            vec1.as_ptr() as *const u8,
            len * std::mem::size_of::<NsNode>(),
        )
    };
    buffer.write(slice);

    Ok(len)
}

#[secure_gate(options(info))]
pub fn enumerate_names_nsid(
    info: &secgate::GateCallInfo,
    desc: Descriptor,
    id: ObjID,
) -> Result<usize> {
    let service = NAMINGSERVICE.get().unwrap();
    let mut binding = service.handles.lock().unwrap();
    let client = binding
        .lookup_mut(info.source_context().unwrap_or(0.into()), desc)
        .ok_or(ErrorKind::Other)?;

    // TODO: make not bad
    let vec1 = client.session.enumerate_namespace_nsid(id)?;
    let len = vec1.len();

    let mut buffer = SimpleBuffer::new(client.buffer.handle().clone());
    let slice = unsafe {
        std::slice::from_raw_parts(
            vec1.as_ptr() as *const u8,
            len * std::mem::size_of::<NsNode>(),
        )
    };
    buffer.write(slice);

    Ok(len)
}

#[secure_gate(options(info))]
pub fn change_namespace(
    info: &secgate::GateCallInfo,
    desc: Descriptor,
    name_len: usize,
) -> Result<()> {
    let service = NAMINGSERVICE.get().unwrap();
    let mut binding = service.handles.lock().unwrap();
    let client = binding
        .lookup_mut(info.source_context().unwrap_or(0.into()), desc)
        .ok_or(ErrorKind::Other)?;

    let path = client.read_buffer(name_len)?;

    client.session.change_namespace(path)
}
