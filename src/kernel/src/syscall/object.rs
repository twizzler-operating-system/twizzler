use alloc::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
use twizzler_abi::{
    object::{ObjID, Protections},
    syscall::{
        CreateTieSpec, HandleType, MapFlags, MapInfo, NewHandleError, ObjectCreate,
        ObjectCreateError, ObjectMapError, ObjectReadMapError, ObjectSource,
    },
};

use crate::{
    memory::context::{Context, ContextRef},
    mutex::Mutex,
    obj::{LookupFlags, Object, ObjectRef},
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
        if src.id.as_u128() == 0 {
            crate::obj::copy::zero_ranges(&obj, src.dest_start as usize, src.len)
        } else {
            let so = crate::obj::lookup_object(src.id, LookupFlags::empty())
                .ok_or(ObjectCreateError::ObjectNotFound)?;
            crate::obj::copy::copy_ranges(
                &so,
                src.src_start as usize,
                &obj,
                src.dest_start as usize,
                src.len,
            )
        }
    }
    crate::obj::register_object(obj.clone());
    Ok(obj.id())
}

pub fn sys_object_map(
    id: ObjID,
    slot: usize,
    prot: Protections,
    handle: Option<ObjID>,
) -> Result<usize, ObjectMapError> {
    let vm = if let Some(handle) = handle {
        get_vmcontext_from_handle(handle).ok_or(ObjectMapError::ObjectNotFound)?
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
    Ok(slot)
}

pub fn sys_object_readmap(handle: ObjID, slot: usize) -> Result<MapInfo, ObjectReadMapError> {
    let vm = if handle.as_u128() == 0 {
        current_memory_context().unwrap()
    } else {
        get_vmcontext_from_handle(handle).ok_or(ObjectReadMapError::InvalidArgument)?
    };
    let info = vm
        .lookup_slot(slot)
        .ok_or(ObjectReadMapError::InvalidSlot)?;
    Ok(MapInfo {
        id: info.object().id(),
        prot: info.mapping_settings(false, false).perms(),
        slot,
        flags: MapFlags::empty(),
    })
}

pub trait ObjectHandle {
    type HandleType;
    fn create_with_handle<NewFn>(obj: ObjectRef, new: NewFn) -> Arc<Self::HandleType>
    where
        NewFn: FnOnce(ObjectRef) -> Self::HandleType,
        Self: Sized,
    {
        Arc::new(new(obj))
    }
}

struct Handle<T: ObjectHandle> {
    obj: ObjectRef,
    item: Arc<T::HandleType>,
}

impl<T: ObjectHandle + Clone> Handle<T> {
    fn new<NewFn>(id: ObjID, new: NewFn) -> Result<Self, NewHandleError>
    where
        NewFn: FnOnce(ObjectRef) -> T::HandleType,
    {
        let obj = crate::obj::lookup_object(id, LookupFlags::empty());
        let obj = match obj {
            crate::obj::LookupResult::Found(obj) => obj,
            _ => return Err(NewHandleError::NotFound),
        };
        Ok(Handle {
            obj: obj.clone(),
            item: T::create_with_handle(obj, new),
        })
    }
}

struct AllHandles {
    all: BTreeSet<ObjID>,
    pager_q_count: u8,
    vm_contexts: BTreeMap<ObjID, Handle<ContextRef>>,
}

static ALL_HANDLES: Once<Mutex<AllHandles>> = Once::new();

fn get_all_handles() -> &'static Mutex<AllHandles> {
    ALL_HANDLES.call_once(|| {
        Mutex::new(AllHandles {
            all: BTreeSet::new(),
            vm_contexts: BTreeMap::new(),
            pager_q_count: 0,
        })
    })
}

pub fn get_vmcontext_from_handle(id: ObjID) -> Option<ContextRef> {
    let ah = get_all_handles();
    ah.lock().vm_contexts.get(&id).map(|x| x.item.clone())
}

pub fn sys_new_handle(id: ObjID, handle_type: HandleType) -> Result<u64, NewHandleError> {
    let mut ah = get_all_handles().lock();
    if ah.all.contains(&id) {
        return Err(NewHandleError::AlreadyHandle);
    }
    match handle_type {
        HandleType::VmContext => ah
            .vm_contexts
            .insert(id, Handle::new(id, |_obj| Context::new())?),
        HandleType::PagerQueue => {
            if ah.pager_q_count == 2 {
                return Err(NewHandleError::HandleSaturated);
            }
            ah.pager_q_count += 1;
            crate::pager::init_pager_queue(id, ah.pager_q_count == 1);
            return Ok(0);
        }
    };
    ah.all.insert(id);
    Ok(0)
}

pub fn sys_unbind_handle(id: ObjID) {
    let mut ah = get_all_handles().lock();
    if !ah.all.contains(&id) {
        return;
    }
    // TODO: we'll need to fix this for having many kinds of handles.
    ah.all.remove(&id);
    ah.vm_contexts.remove(&id).unwrap();
}

// Note: placeholder types
pub fn sys_sctx_attach(id: ObjID) -> Result<u32, u32> {
    todo!()
}
