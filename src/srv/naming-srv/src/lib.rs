#![feature(naked_functions)]
#![feature(linkage)]

use lazy_static::lazy_static;
use std::any::Any;
use std::{collections::HashMap, rc::Rc};
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};
use arrayvec::ArrayString;
use twizzler_abi::{
    aux::KernelInitInfo,
    slot::RESERVED_KERNEL_INIT,
    object::{ObjID, MAX_SIZE, NULLPAGE_SIZE},
    syscall::{LifetimeType, ObjectCreateFlags, BackingType, ObjectCreate, sys_object_create}
};
use twizzler_rt_abi::object::{MapFlags, ObjectHandle};
use secgate::util::SimpleBuffer;

fn get_kernel_init_info() -> &'static KernelInitInfo {
    unsafe {
        (((twizzler_abi::slot::RESERVED_KERNEL_INIT * MAX_SIZE) + NULLPAGE_SIZE)
            as *const KernelInitInfo)
            .as_ref()
            .unwrap()
    }
}

type Val = ObjID;

lazy_static! {
    static ref HASHMAP: Mutex<HashMap<String, Val>> = {
        let mut m: Mutex<HashMap<String, Val>> = Mutex::new(HashMap::new());
        let mut h = m.lock().unwrap();

        let init_info = get_kernel_init_info();
        let mut initrd_namespace: HashMap<String, Val> = HashMap::new();
        for n in init_info.names() {
            let path = n.name();
            initrd_namespace.insert(path.to_owned(), n.id());
        }

        drop(h);
        m 
    };
    static ref OBJ_MAP: Mutex<HashMap<ObjID, SimpleBuffer>> = {
        Mutex::new(HashMap::new())
    };
}

pub struct NamespaceHandle {
    buf: SimpleBuffer,
    id: ObjID
}

impl NamespaceHandle {
    pub fn new() -> NamespaceHandle {
        let id = buffer_request().unwrap();
        let handle = twizzler_rt_abi::object::twz_rt_map_object(id, MapFlags::READ | MapFlags::WRITE).unwrap();

        NamespaceHandle {
            buf: SimpleBuffer::new(handle),
            id: id
        }
    }

    pub fn put(&mut self, key: &str, val: ObjID) {
        let bytes_written = self.buf.write(key.as_bytes());
        put(self.id, bytes_written, val.into());
    }

    pub fn get(&mut self, key: &str) -> Option<ObjID> {
        let bytes_written = self.buf.write(key.as_bytes());
        get(self.id, bytes_written).unwrap()
    }
}

// Creates an shared buffer between the service and caller
#[secgate::secure_gate]
pub fn buffer_request() -> ObjID {
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
    .unwrap();
    let handle = twizzler_rt_abi::object::twz_rt_map_object(id, MapFlags::READ | MapFlags::WRITE).unwrap();

    let mut obj_map = OBJ_MAP.lock().unwrap();
    obj_map.insert(id, SimpleBuffer::new(handle));
    id 
}

#[secgate::secure_gate]
pub fn buffer_free(id: ObjID) {

}

#[secgate::secure_gate]
pub fn put(buf: ObjID, len_key: usize, val: ObjID) {
    let mut h = HASHMAP.lock().unwrap();
    let mut o = OBJ_MAP.lock().unwrap();

    let handle = o.get(&buf).unwrap();
    let mut buf = vec![0u8; len_key];
    handle.read(&mut buf);

    h.insert(String::from_utf8(buf).unwrap(), val);
}

#[secgate::secure_gate]
pub fn get(buf: ObjID, len_key: usize) -> Option<ObjID> {
    let mut h = HASHMAP.lock().unwrap();
    let mut o = OBJ_MAP.lock().unwrap();

    let handle = o.get(&buf).unwrap();
    let mut buf = vec![0u8; len_key];
    handle.read(&mut buf);

    h.get(&String::from_utf8(buf).unwrap()).copied()
}

#[secgate::secure_gate]
pub fn reload() {
}
