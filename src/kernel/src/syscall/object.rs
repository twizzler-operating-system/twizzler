use alloc::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use twizzler_abi::{
    meta::MetaFlags,
    object::{ObjID, Protections},
    syscall::{
        CreateTieSpec, DeleteFlags, HandleType, MapFlags, MapInfo, ObjectControlCmd, ObjectCreate,
        ObjectCreateFlags, ObjectSource,
    },
};
use twizzler_rt_abi::{
    error::{ArgumentError, NamingError, ObjectError, ResourceError},
    Result,
};

use crate::{
    arch::context::ArchContext,
    memory::context::{virtmem::Slot, Context, ContextRef, UserContext},
    mutex::Mutex,
    obj::{calculate_new_id, lookup_object, LookupFlags, Object, ObjectRef},
    once::Once,
    security::get_sctx,
    thread::{current_memory_context, current_thread_ref},
};

pub fn sys_object_create(
    create: &ObjectCreate,
    srcs: &[ObjectSource],
    ties: &[CreateTieSpec],
) -> Result<ObjID> {
    let id = calculate_new_id(create.kuid, MetaFlags::default());
    let obj = Arc::new(Object::new(id, create.lt, ties));
    if obj.use_pager() {
        crate::pager::create_object(id);
        if create.flags.contains(ObjectCreateFlags::DELETE) {
            object_ctrl(id, ObjectControlCmd::Delete(DeleteFlags::empty()));
        }
        return Ok(obj.id());
    }
    for src in srcs {
        if src.id.raw() == 0 {
            crate::obj::copy::zero_ranges(&obj, src.dest_start as usize, src.len)
        } else {
            let so = crate::obj::lookup_object(src.id, LookupFlags::empty())
                .ok_or(ObjectError::NoSuchObject)?;
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
    if create.flags.contains(ObjectCreateFlags::DELETE) {
        object_ctrl(id, ObjectControlCmd::Delete(DeleteFlags::empty()));
    }
    Ok(obj.id())
}

pub fn sys_object_map(
    id: ObjID,
    slot: usize,
    prot: Protections,
    handle: Option<ObjID>,
) -> Result<usize> {
    let vm = if let Some(handle) = handle {
        get_vmcontext_from_handle(handle).ok_or(ObjectError::NoSuchObject)?
    } else {
        current_memory_context().unwrap()
    };
    let obj = crate::obj::lookup_object(id, LookupFlags::empty());
    let obj = match obj {
        crate::obj::LookupResult::WasDeleted => return Err(ObjectError::NoSuchObject.into()),
        crate::obj::LookupResult::Found(obj) => obj,
        _ => match crate::pager::lookup_object_and_wait(id) {
            Some(obj) => obj,
            None => return Err(ObjectError::NoSuchObject.into()),
        },
    };
    // TODO
    let _res = crate::operations::map_object_into_context(slot, obj, vm, prot.into());
    Ok(slot)
}

pub fn sys_object_unmap(handle: Option<ObjID>, slot: usize) -> Result<u64> {
    let vm = if let Some(handle) = handle {
        get_vmcontext_from_handle(handle).ok_or(ArgumentError::BadHandle)?
    } else {
        current_memory_context().unwrap()
    };
    vm.remove_object(Slot::try_from(slot).map_err(|_| ArgumentError::InvalidArgument)?);
    Ok(0)
}

pub fn sys_object_readmap(handle: ObjID, slot: usize) -> Result<MapInfo> {
    let vm = if handle.raw() == 0 {
        current_memory_context().unwrap()
    } else {
        get_vmcontext_from_handle(handle).ok_or(ArgumentError::InvalidArgument)?
    };
    let info = vm.lookup_slot(slot).ok_or(ArgumentError::InvalidAddress)?;
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
    fn new<NewFn>(id: ObjID, new: NewFn) -> Result<Self>
    where
        NewFn: FnOnce(ObjectRef) -> T::HandleType,
    {
        let obj = crate::obj::lookup_object(id, LookupFlags::empty());
        let obj = match obj {
            crate::obj::LookupResult::Found(obj) => obj,
            _ => return Err(ObjectError::NoSuchObject.into()),
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

pub fn sys_new_handle(id: ObjID, handle_type: HandleType) -> Result<u64> {
    let mut ah = get_all_handles().lock();
    if ah.all.contains(&id) {
        return Err(NamingError::AlreadyBound.into());
    }
    match handle_type {
        HandleType::VmContext => ah
            .vm_contexts
            .insert(id, Handle::new(id, |_obj| Context::new())?),
        HandleType::PagerQueue => {
            if ah.pager_q_count == 2 {
                return Err(ResourceError::OutOfNames.into());
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
pub fn sys_sctx_attach(id: ObjID) -> Result<u32> {
    let sctx = get_sctx(id)?;

    let current_thread = current_thread_ref().unwrap();
    let current_context = current_memory_context().unwrap();
    current_context.register_sctx(sctx.id(), ArchContext::new());
    current_thread.secctx.attach(sctx)?;

    Ok(0)
}

pub fn object_ctrl(id: ObjID, cmd: ObjectControlCmd) -> (u64, u64) {
    match cmd {
        ObjectControlCmd::Sync => {
            crate::pager::sync_object(id);
        }
        ObjectControlCmd::Delete(_) => {
            let mut invoke_pager = true;
            if let Some(obj) = lookup_object(id, LookupFlags::empty()).ok_or(()).ok() {
                invoke_pager = obj.use_pager();
                obj.mark_for_delete();
            }
            if invoke_pager {
                crate::pager::del_object(id);
            }
            crate::obj::scan_deleted();
        }
        _ => {}
    }
    (0, 0)
}
