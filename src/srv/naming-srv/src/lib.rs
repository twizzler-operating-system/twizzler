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
pub struct Schema {
    pub key: [u8; MAX_KEY_SIZE],
    pub val: u128,
}

struct Namer {
    handles: HandleMgr<NamespaceClient>,
    names: Vec<Schema>,
    count: usize,
}

impl Namer {
    const fn new() -> Self {
        Self {
            handles: HandleMgr::new(None),
            names: Vec::<Schema>::new(),
            count: 0,
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
                key: [0u8; 256],
                val: 0,
            };
            let bytes = n.name().as_bytes();
            s.key[..bytes.len()].copy_from_slice(&bytes[..bytes.len()]);
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
