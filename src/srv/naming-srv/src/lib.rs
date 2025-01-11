#![feature(naked_functions)]
#![feature(linkage)]

use std::{default, sync::Mutex};

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

pub const MAX_KEY_SIZE: usize = 256;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Schema {
    pub key: ArrayString<MAX_KEY_SIZE>,
    pub val: u128,
}

struct Namer {
    handles: HandleMgr<NamespaceClient>,
    names: Vec<Schema>,
}

impl Namer {
    const fn new() -> Self {
        Self {
            handles: HandleMgr::new(None),
            names: Vec::<Schema>::new(),
        }
    }
}

struct NamerSrv {
    inner: Mutex<Namer>,
}

lazy_static! {
    static ref NAMINGSERVICE: NamerSrv = {
        let mut namer = Namer::new();

        let init_info = get_kernel_init_info();

        for n in init_info.names() {
            let mut s = Schema {
                key: ArrayString::new(),
                val: 0,
            };
            s.key = ArrayString::from(n.name()).unwrap();
            s.val = n.id().raw();
            namer.names.push(s);
        }

        NamerSrv {
            inner: Mutex::new(namer),
        }
    };
}

#[secure_gate(options(info))]
pub fn put(info: &secgate::GateCallInfo, desc: Descriptor) {
    let mut namer = NAMINGSERVICE.inner.lock().unwrap();
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

    let foo = namer
        .names
        .iter_mut()
        .find(|search| search.key == provided.key);
    match foo {
        Some(found) => found.val = provided.val,
        None => namer.names.push(provided),
    };
}

#[secure_gate(options(info))]
pub fn get(info: &secgate::GateCallInfo, desc: Descriptor) -> Option<u128> {
    let mut namer = NAMINGSERVICE.inner.lock().unwrap();
    let Some(client) = namer
        .handles
        .lookup(info.source_context().unwrap_or(0.into()), desc)
    else {
        return None;
    };

    let mut buf = [0u8; std::mem::size_of::<Schema>()];
    client.buffer.read(&mut buf);
    let provided =
        unsafe { std::mem::transmute::<[u8; std::mem::size_of::<Schema>()], Schema>(buf) };

    let foo: Option<&Schema> = namer.names.iter().find(|search| search.key == provided.key);
    match foo {
        Some(found) => Some(found.val),
        None => None,
    }
}

#[secure_gate(options(info))]
pub fn open_handle(info: &secgate::GateCallInfo) -> Option<(Descriptor, ObjID)> {
    let mut namer = NAMINGSERVICE.inner.lock().ok()?;
    let client = NamespaceClient::new()?;
    let id = client.sbid();
    let desc = namer
        .handles
        .insert(info.source_context().unwrap_or(0.into()), client)?;

    Some((desc, id))
}

#[secure_gate(options(info))]
pub fn close_handle(info: &secgate::GateCallInfo, desc: Descriptor) {
    let mut namer = NAMINGSERVICE.inner.lock().unwrap();
    namer
        .handles
        .remove(info.source_context().unwrap_or(0.into()), desc);
}

#[secure_gate(options(info))]
pub fn enumerate_names(info: &secgate::GateCallInfo, desc: Descriptor) -> Option<usize> {
    let mut namer = NAMINGSERVICE.inner.lock().unwrap();
    let Some(client) = namer
        .handles
        .lookup(info.source_context().unwrap_or(0.into()), desc)
    else {
        return None;
    };

    let mut vec = Vec::<u8>::new();
    for s in namer.names.clone() {
        vec.extend_from_slice(
            unsafe {
                &std::mem::transmute::<Schema, [u8; std::mem::size_of::<Schema>()]>(s)
            }
        );
    }
    let mut buffer = SimpleBuffer::new(client.buffer.handle().clone());
    buffer.write(&vec);

    Some(namer.names.len())
}

#[secure_gate(options(info))]
pub fn remove(info: &secgate::GateCallInfo, desc: Descriptor) {
    let mut namer = NAMINGSERVICE.inner.lock().unwrap();
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
        .retain(|x| x.key != provided.key);
}
