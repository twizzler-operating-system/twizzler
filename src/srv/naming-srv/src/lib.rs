#![feature(naked_functions)]
#![feature(linkage)]

use std::{
    default, fs::OpenOptions, ops::DerefMut, path::Path, sync::{Arc, Mutex}
};

use arrayvec::ArrayString;
use lazy_static::lazy_static;
use naming_core::{handle::Schema, store::{NameStore, NameSession, EntryType}, MAX_KEY_SIZE};
use secgate::{
    secure_gate,
    util::{Descriptor, HandleMgr, SimpleBuffer},
};
use twizzler::{
    alloc::invbox::InvBox, collections::vec::{Vec, VecObject, VecObjectAlloc}, marker::Invariant, object::{Object, ObjectBuilder, TypedObject}, ptr::{GlobalPtr, InvPtr}
};
use twizzler_abi::{
    aux::KernelInitInfo,
    object::{MAX_SIZE, NULLPAGE_SIZE},
    syscall::{sys_object_create, BackingType, LifetimeType, ObjectCreate, ObjectCreateFlags},
};
use twizzler_rt_abi::{bindings, object::{MapFlags, ObjID}};

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
        Some(Self { session: session, buffer })
    }

    fn sbid(&self) -> ObjID {
        self.buffer.handle().id()
    }
}

unsafe impl Send for Namer<'_> {}
unsafe impl Sync for Namer<'_> {}

struct Namer<'a> {
    handles: Mutex<HandleMgr<NamespaceClient<'a>>>,
    names: NameStore
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
    static ref NAMINGSERVICE: Namer<'static> = {
        let mut namer = Namer::new();
        {
            let session = namer.names.root_session();
            session.put(Path::new("/initrd"), EntryType::Namespace);
            let init_info = get_kernel_init_info();
    
            for n in init_info.names() {
                session.put("/initrd/".to_owned() + n.name(), EntryType::Name(n.id().raw()));
            }
        }
        namer
    };
}

#[secure_gate(options(info))]
pub fn open_handle(info: &secgate::GateCallInfo) -> Option<(Descriptor, ObjID)> {
    let mut binding = NAMINGSERVICE.handles.lock().unwrap();

    let session = NAMINGSERVICE.names.root_session();
    let client = NamespaceClient::new(session)?;
    let id = client.sbid();
    
    let desc = binding
        .insert(info.source_context().unwrap_or(0.into()), client)?;

    Some((desc, id))
}

#[secure_gate(options(info))]
pub fn close_handle(info: &secgate::GateCallInfo, desc: Descriptor) {
    let mut binding = NAMINGSERVICE.handles.lock().unwrap();

    binding.remove(info.source_context().unwrap_or(0.into()), desc);
}

#[secure_gate(options(info))]
pub fn put(info: &secgate::GateCallInfo, desc: Descriptor) {
    let binding = NAMINGSERVICE.handles.lock().unwrap();
    let Some(client) = binding.lookup(info.source_context().unwrap_or(0.into()), desc) 
    else {
        return;
    };

    let mut buf = [0u8; std::mem::size_of::<Schema>()];
    client.buffer.read(&mut buf);
    let provided =
        unsafe { std::mem::transmute::<[u8; std::mem::size_of::<Schema>()], Schema>(buf) };

    client.session.put(provided.key.as_str(), EntryType::Name(provided.val));
}

#[secure_gate(options(info))]
pub fn get(info: &secgate::GateCallInfo, desc: Descriptor) -> Option<u128> {
    let binding = NAMINGSERVICE.handles.lock().unwrap();
    let Some(client) = binding.lookup(info.source_context().unwrap_or(0.into()), desc) 
    else {
        return None;
    };

    let mut buf = [0u8; std::mem::size_of::<Schema>()];
    client.buffer.read(&mut buf);
    let provided =
        unsafe { std::mem::transmute::<[u8; std::mem::size_of::<Schema>()], Schema>(buf) };
    
    client.session.get(provided.key.as_str())
}

#[secure_gate(options(info))]
pub fn enumerate_names(info: &secgate::GateCallInfo, desc: Descriptor) -> Option<usize> {
    let binding = NAMINGSERVICE.handles.lock().unwrap();
    let Some(client) = binding.lookup(info.source_context().unwrap_or(0.into()), desc) 
    else {
        return None;
    };

    let mut buf = [0u8; std::mem::size_of::<Schema>()];
    client.buffer.read(&mut buf);
    let provided =
        unsafe { std::mem::transmute::<[u8; std::mem::size_of::<Schema>()], Schema>(buf) };

    // TODO: make not bad
    let mut vec1 = client.session.enumerate_namespace(provided.key);
    let len = vec1.len();
    let mut vec = std::vec::Vec::<u8>::new();
    for i in vec1 {
        vec.extend_from_slice(unsafe {
            &std::mem::transmute::<Schema, [u8; std::mem::size_of::<Schema>()]>(i)
        });
    }
    let mut buffer = SimpleBuffer::new(client.buffer.handle().clone());
    buffer.write(&vec);

    Some(len)
}

#[secure_gate(options(info))]
pub fn remove(info: &secgate::GateCallInfo, desc: Descriptor) {
    todo!()
}

#[secure_gate(options(info))]
pub fn change_namespace(info: &secgate::GateCallInfo, desc: Descriptor) {
    let mut binding = NAMINGSERVICE.handles.lock().unwrap();
    let Some(mut client) = binding.lookup_mut(info.source_context().unwrap_or(0.into()), desc) 
    else {
        return;
    };

    let mut buf = [0u8; std::mem::size_of::<Schema>()];
    client.buffer.read(&mut buf);
    let provided =
        unsafe { std::mem::transmute::<[u8; std::mem::size_of::<Schema>()], Schema>(buf) };

    client.session.change_namespace(provided.key.as_str());
}



#[secure_gate(options(info))]
pub fn put_namespace(info: &secgate::GateCallInfo, desc: Descriptor) {
    let binding = NAMINGSERVICE.handles.lock().unwrap();
    let Some(client) = binding.lookup(info.source_context().unwrap_or(0.into()), desc) 
    else {
        return;
    };

    let mut buf = [0u8; std::mem::size_of::<Schema>()];
    client.buffer.read(&mut buf);
    let provided =
        unsafe { std::mem::transmute::<[u8; std::mem::size_of::<Schema>()], Schema>(buf) };

    client.session.put(provided.key.as_str(), EntryType::Namespace);
}

#[secure_gate(options(info))]
pub fn delete_namespace(info: &secgate::GateCallInfo, desc: Descriptor) {
    todo!()
}

