#![feature(naked_functions)]
#![feature(linkage)]
#[warn(unused_variables)]
use std::{path::Path, sync::Mutex};

use lazy_static::lazy_static;
use naming_core::{Entry, EntryType, ErrorKind, NameSession, NameStore, Result};
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

fn get_kernel_init_info() -> &'static KernelInitInfo {
    unsafe {
        (((twizzler_abi::slot::RESERVED_KERNEL_INIT * MAX_SIZE) + NULLPAGE_SIZE)
            as *const KernelInitInfo)
            .as_ref()
            .unwrap()
    }
}

struct NamespaceClient<'a> {
    session: NameSession<'a>,
    buffer: SimpleBuffer,
}

impl<'a> NamespaceClient<'a> {
    fn new(session: NameSession<'a>) -> Option<Self> {
        twizzler_abi::klog_println!("0");
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
        twizzler_abi::klog_println!("1");
        let handle =
            twizzler_rt_abi::object::twz_rt_map_object(id, MapFlags::WRITE | MapFlags::READ)
                .ok()?;
        twizzler_abi::klog_println!("2");
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
}

lazy_static! {
    static ref NAMINGSERVICE: Namer<'static> = Namer::new();
}

// How would this work if I changed the root while handles were open?
// Maybe the secure gates don't provide names until set_root is performed.
#[secure_gate(options(info))]
pub fn namer_start(_info: &secgate::GateCallInfo, _bootstrap: ObjID) {
    // Assume some data structure that's inside _bootstrap to initalize names
    let session = NAMINGSERVICE.names.root_session();
    session
        .put(Path::new("/initrd"), EntryType::Namespace)
        .unwrap();
    let init_info = get_kernel_init_info();

    for n in init_info.names() {
        session
            .put(
                "/initrd/".to_owned() + n.name(),
                EntryType::Object(n.id().raw()),
            )
            .unwrap();
    }
}

#[secure_gate(options(info))]
pub fn open_handle(info: &secgate::GateCallInfo) -> Option<(Descriptor, ObjID)> {
    twizzler_abi::klog_println!("naming open handle");
    let mut binding = NAMINGSERVICE.handles.lock().unwrap();
    twizzler_abi::klog_println!("X");

    let session = NAMINGSERVICE.names.root_session();
    twizzler_abi::klog_println!("Y");
    let client = NamespaceClient::new(session)?;
    twizzler_abi::klog_println!("Y2");
    let id = client.sbid();

    twizzler_abi::klog_println!("Z");
    let desc = binding.insert(info.source_context().unwrap_or(0.into()), client)?;

    twizzler_abi::klog_println!("done");
    Some((desc, id))
}

#[secure_gate(options(info))]
pub fn close_handle(info: &secgate::GateCallInfo, desc: Descriptor) {
    let mut binding = NAMINGSERVICE.handles.lock().unwrap();

    binding.remove(info.source_context().unwrap_or(0.into()), desc);
}

#[secure_gate(options(info))]
pub fn put(info: &secgate::GateCallInfo, desc: Descriptor) -> Result<()> {
    let mut binding = NAMINGSERVICE.handles.lock().unwrap();
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
    let mut binding = NAMINGSERVICE.handles.lock().unwrap();
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
pub fn remove(_info: &secgate::GateCallInfo, _desc: Descriptor) -> Result<()> {
    todo!()
}

#[secure_gate(options(info))]
pub fn enumerate_names(info: &secgate::GateCallInfo, desc: Descriptor) -> Result<usize> {
    let mut binding = NAMINGSERVICE.handles.lock().unwrap();
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
    let mut binding = NAMINGSERVICE.handles.lock().unwrap();
    let client = binding
        .lookup_mut(info.source_context().unwrap_or(0.into()), desc)
        .ok_or(ErrorKind::Other)?;

    let mut buf = [0u8; std::mem::size_of::<Entry>()];
    client.buffer.read(&mut buf);
    let provided = unsafe { std::mem::transmute::<[u8; std::mem::size_of::<Entry>()], Entry>(buf) };

    client.session.change_namespace(provided.name)
}
