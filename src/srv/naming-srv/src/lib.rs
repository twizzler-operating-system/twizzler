#![feature(naked_functions)]
#![feature(linkage)]

use std::{default, sync::{Arc, Mutex}};

use lazy_static::lazy_static;
use secgate::{
    secure_gate,
    util::{Descriptor, HandleMgr, SimpleBuffer},
};
use twizzler_abi::{
    aux::KernelInitInfo,
    object::{ObjID, MAX_SIZE, NULLPAGE_SIZE},
    syscall::{sys_object_create, BackingType, LifetimeType, ObjectCreate, ObjectCreateFlags},
};
use twizzler_rt_abi::object::MapFlags;
use arrayvec::ArrayString;
use twizzler::{collections::vec::VecObject, marker::Invariant, object::{ObjectBuilder, TypedObject}};
use twizzler::collections::vec::Vec;
use twizzler::object::Object;
use twizzler::collections::vec::VecObjectAlloc;

fn get_kernel_init_info() -> &'static KernelInitInfo {
    unsafe {
        (((twizzler_abi::slot::RESERVED_KERNEL_INIT * MAX_SIZE) + NULLPAGE_SIZE)
            as *const KernelInitInfo)
            .as_ref()
            .unwrap()
    }
}

pub struct NamespaceClient {
    buffer: SimpleBuffer,
}

impl NamespaceClient {
    fn new() -> Option<Self> {
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

    fn sbid(&self) -> ObjID {
        self.buffer.handle().id()
    }
}

pub const MAX_KEY_SIZE: usize = 255;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Schema {
    pub key: ArrayString<MAX_KEY_SIZE>,
    pub val: u128,
}

unsafe impl Invariant for Schema {}

struct Namer {
    handles: HandleMgr<NamespaceClient>,
    names: VecObject<Schema, VecObjectAlloc>
}

unsafe impl Send for Namer {}
unsafe impl Sync for Namer {}

impl Namer {
    fn new() -> Self {
        Self {
            handles: HandleMgr::new(None),
            names: VecObject::new(ObjectBuilder::default()).unwrap()
        }
    }
}

lazy_static! {
    static ref NAMINGSERVICE: Mutex<Namer> = {

        let mut namer = Namer::new();

        let init_info = get_kernel_init_info();

        for n in init_info.names() {
            let mut s = Schema {
                key: ArrayString::new(),
                val: 0,
            };
            s.key = ArrayString::from(n.name()).unwrap();
            s.val = n.id().raw();
            namer.names.push(s).unwrap();
        }

        Mutex::new(namer)
    };
}

#[secure_gate(options(info))]
pub fn put(info: &secgate::GateCallInfo, desc: Descriptor) {
    let mut namer = NAMINGSERVICE.lock().unwrap();
    let Some(client) = namer
        .handles
        .lookup(info.source_context().unwrap_or(0.into()), desc)
    else {
        return;
    };

    // should use buffer rather than copying
    let mut buf = [0u8; std::mem::size_of::<Schema>()];
    client.buffer.read(&mut buf);
    let provided =
        unsafe { std::mem::transmute::<[u8; std::mem::size_of::<Schema>()], Schema>(buf) };
    
    for i in 0..namer.names.len() {
        let foo = namer.names.get(i).unwrap();
        if foo.key == provided.key {
            unsafe {foo.mutable().val = provided.val}
        }
    }

    namer.names.push(provided);
}

#[secure_gate(options(info))]
pub fn get(info: &secgate::GateCallInfo, desc: Descriptor) -> Option<u128> {
    let mut namer = NAMINGSERVICE.lock().unwrap();
    let client = namer
        .handles
        .lookup(info.source_context().unwrap_or(0.into()), desc)?;

    // should use buffer rather than copying
    let mut buf = [0u8; std::mem::size_of::<Schema>()];
    client.buffer.read(&mut buf);
    let provided =
        unsafe { std::mem::transmute::<[u8; std::mem::size_of::<Schema>()], Schema>(buf) };

    for i in 0..namer.names.len() {
        let foo = namer.names.get(i).unwrap();
        if foo.key == provided.key {
            return Some(foo.val);
        }
    }
    None
}

#[secure_gate(options(info))]
pub fn open_handle(info: &secgate::GateCallInfo) -> Option<(Descriptor, ObjID)> {
    let mut namer = NAMINGSERVICE.lock().unwrap();
    let client = NamespaceClient::new()?;
    let id = client.sbid();
    let desc = namer
        .handles
        .insert(info.source_context().unwrap_or(0.into()), client)?;

    Some((desc, id))
}

#[secure_gate(options(info))]
pub fn close_handle(info: &secgate::GateCallInfo, desc: Descriptor) {
    let mut namer = NAMINGSERVICE.lock().unwrap();
    namer
        .handles
        .remove(info.source_context().unwrap_or(0.into()), desc);
}

#[secure_gate(options(info))]
pub fn enumerate_names(info: &secgate::GateCallInfo, desc: Descriptor) -> Option<usize> {
    let mut namer = NAMINGSERVICE.lock().unwrap();
    let Some(client) = namer
        .handles
        .lookup(info.source_context().unwrap_or(0.into()), desc)
    else {
        return None;
    };

    let mut vec = std::vec::Vec::<u8>::new();
    for i in 0..namer.names.len() {
        let foo = namer.names.get(i).unwrap();
        vec.extend_from_slice(
            unsafe {
                &std::mem::transmute::<Schema, [u8; std::mem::size_of::<Schema>()]>(*foo.raw())
            }
        );
    }
    let mut buffer = SimpleBuffer::new(client.buffer.handle().clone());
    buffer.write(&vec);

    Some(namer.names.len())
}

#[secure_gate(options(info))]
pub fn remove(info: &secgate::GateCallInfo, desc: Descriptor) {
    todo!()
    /*let mut namer = NAMINGSERVICE.inner.lock().unwrap();
    let Some(client) = namer
        .handles
        .lookup(info.source_context().unwrap_or(0.into()), desc)
    else {
        return;
    };

    let mut buf = [0u8; std::mem::size_of::<Schema>()];
    client.buffer.read(&mut buf);
    let provided =
        unsafe { std::mem::transmute::<[u8; std::mem::size_of::<Schema>()], Schema>(buf) };

    let foo = namer
        .names
        .retain(|x| x.key != provided.key);*/
}
