#![feature(naked_functions)]
#![feature(linkage)]

use std::sync::Mutex;

use arrayvec::ArrayString;
use lazy_static::lazy_static;
use naming_core::definitions::Schema;
use secgate::{
    secure_gate,
    util::{Descriptor, HandleMgr, SimpleBuffer},
};
use twizzler::{
    collections::vec::{VecObject, VecObjectAlloc},
    object::ObjectBuilder,
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

struct Namer {
    handles: HandleMgr<NamespaceClient>,
    names: VecObject<Schema, VecObjectAlloc>,
}

unsafe impl Send for Namer {}
unsafe impl Sync for Namer {}

impl Namer {
    fn new() -> Self {
        Self {
            handles: HandleMgr::new(None),
            names: VecObject::new(ObjectBuilder::default()).unwrap(),
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
            unsafe { foo.mutable().val = provided.val }
        }
    }

    // TODO: handle error
    let _ = namer.names.push(provided);
}

#[secure_gate(options(info))]
pub fn get(info: &secgate::GateCallInfo, desc: Descriptor) -> Option<u128> {
    let namer = NAMINGSERVICE.lock().unwrap();
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
    let namer = NAMINGSERVICE.lock().unwrap();
    let Some(client) = namer
        .handles
        .lookup(info.source_context().unwrap_or(0.into()), desc)
    else {
        return None;
    };

    let mut vec = std::vec::Vec::<u8>::new();
    for i in 0..namer.names.len() {
        let foo = namer.names.get(i).unwrap();
        vec.extend_from_slice(unsafe {
            &std::mem::transmute::<Schema, [u8; std::mem::size_of::<Schema>()]>(*foo.raw())
        });
    }
    let mut buffer = SimpleBuffer::new(client.buffer.handle().clone());
    buffer.write(&vec);

    Some(namer.names.len())
}

#[secure_gate(options(info))]
pub fn remove(_info: &secgate::GateCallInfo, _desc: Descriptor) {
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
