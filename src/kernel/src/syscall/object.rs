use alloc::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
use twizzler_abi::{
    object::{ObjID, Protections},
    syscall::{
        CreateTieSpec, HandleType, NewHandleError, ObjectCreate, ObjectCreateError, ObjectMapError,
        ObjectSource,
    },
};
use x86_64::VirtAddr;

use crate::{
    memory::context::MemoryContextRef,
    mutex::Mutex,
    obj::{copy::CopySpec, LookupFlags, Object, ObjectRef, PageNumber},
    once::Once,
    thread::current_memory_context,
};

pub fn sys_object_create(
    _create: &ObjectCreate,
    srcs: &[ObjectSource],
    _ties: &[CreateTieSpec],
) -> Result<ObjID, ObjectCreateError> {
    let obj = Arc::new(Object::new());
    for src in srcs {
        let so = crate::obj::lookup_object(src.id, LookupFlags::empty())
            .ok_or(ObjectCreateError::ObjectNotFound)?;
        let cs = CopySpec::new(
            so,
            PageNumber::from_address(VirtAddr::new(src.src_start)),
            PageNumber::from_address(VirtAddr::new(src.dest_start)),
            src.len,
        );
        crate::obj::copy::copy_ranges(&cs.src, cs.src_start, &obj, cs.dest_start, cs.length)
    }
    crate::obj::register_object(obj.clone());
    Ok(obj.id())
}

pub fn sys_object_map(
    id: ObjID,
    slot: usize,
    prot: Protections,
    handle: Option<&ObjID>,
) -> Result<usize, ObjectMapError> {
    let vm = if let Some(handle) = handle {
        get_vmcontext_from_handle(*handle).ok_or(ObjectMapError::ObjectNotFound)?
    } else {
        current_memory_context().unwrap()
    };
    let obj = crate::obj::lookup_object(id, LookupFlags::empty());
    let obj = match obj {
        crate::obj::LookupResult::NotFound => return Err(ObjectMapError::ObjectNotFound),
        crate::obj::LookupResult::WasDeleted => return Err(ObjectMapError::ObjectNotFound),
        crate::obj::LookupResult::Pending => return Err(ObjectMapError::ObjectNotFound),
        crate::obj::LookupResult::Found(obj) => obj,
    };
    // TODO
    let _res = crate::operations::map_object_into_context(slot, obj, vm, prot.into());
    logln!(
        "mapping obj {} to {} with {:?} in {:?}",
        id,
        slot,
        prot,
        handle
    );
    Ok(slot)
}

pub trait ObjectHandle {
    fn create_with_handle(obj: ObjectRef) -> Self;
}

struct Handle<T: ObjectHandle> {
    obj: ObjectRef,
    item: T,
}

impl<T: ObjectHandle + Clone> Handle<T> {
    fn new(id: ObjID) -> Result<Self, NewHandleError> {
        let obj = crate::obj::lookup_object(id, LookupFlags::empty());
        let obj = match obj {
            crate::obj::LookupResult::Found(obj) => obj,
            _ => Err(NewHandleError::NotFound)?,
        };
        Ok(Handle {
            obj: obj.clone(),
            item: T::create_with_handle(obj),
        })
    }
}

struct AllHandles {
    all: BTreeSet<ObjID>,
    vm_contexts: BTreeMap<ObjID, Handle<MemoryContextRef>>,
}

static ALL_HANDLES: Once<Mutex<AllHandles>> = Once::new();

fn get_all_handles() -> &'static Mutex<AllHandles> {
    ALL_HANDLES.call_once(|| {
        Mutex::new(AllHandles {
            all: BTreeSet::new(),
            vm_contexts: BTreeMap::new(),
        })
    })
}

pub fn get_vmcontext_from_handle(id: ObjID) -> Option<MemoryContextRef> {
    let ah = get_all_handles();
    ah.lock().vm_contexts.get(&id).map(|x| x.item.clone())
}

pub fn sys_new_handle(id: ObjID, handle_type: HandleType) -> Result<u64, NewHandleError> {
    let mut ah = get_all_handles().lock();
    if ah.all.contains(&id) {
        return Err(NewHandleError::AlreadyHandle);
    }
    match handle_type {
        HandleType::VmContext => ah.vm_contexts.insert(id, Handle::new(id)?),
    };
    ah.all.insert(id);
    Ok(0)
}
