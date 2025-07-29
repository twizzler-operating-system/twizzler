use std::sync::OnceLock;

use dynlink::{
    compartment::{CompartmentId, MONITOR_COMPARTMENT_ID},
    engines::{Backing, ContextEngine, LoadCtx},
    library::UnloadedLibrary,
    DynlinkError, DynlinkErrorKind,
};
use naming_core::{GetFlags, NameStore, NsNodeKind};
use twizzler_abi::{
    aux::KernelInitInfo,
    object::{Protections, MAX_SIZE, NULLPAGE_SIZE},
    syscall::{BackingType, ObjectCreate, ObjectCreateFlags},
};
use twizzler_rt_abi::{
    error::{NamingError, TwzError},
    object::{MapFlags, ObjID},
};

use crate::mon::{
    get_monitor,
    space::{MapInfo, Space},
};

pub struct Engine;

fn get_new_sctx_instance(_sctx: ObjID) -> ObjID {
    // TODO: we don't support real sctx instances yet
    twizzler_abi::syscall::sys_object_create(
        ObjectCreate::new(
            BackingType::Normal,
            twizzler_abi::syscall::LifetimeType::Volatile,
            None,
            ObjectCreateFlags::empty(),
            Protections::all(),
        ),
        &[],
        &[],
    )
    .unwrap()
}

impl ContextEngine for Engine {
    fn load_segments(
        &mut self,
        src: &Backing,
        ld: &[dynlink::engines::LoadDirective],
        comp_id: CompartmentId,
        load_ctx: &mut LoadCtx,
    ) -> Result<Vec<Backing>, dynlink::DynlinkError> {
        let instance = *load_ctx
            .set
            .entry(comp_id)
            .or_insert_with(|| get_new_sctx_instance(1.into()));
        let map = |text_id, data_id| {
            #[allow(deprecated)]
            let (text_handle, data_handle) = get_monitor()
                .space
                .lock()
                .unwrap()
                .map_pair(
                    MapInfo {
                        id: text_id,
                        flags: MapFlags::READ | MapFlags::EXEC,
                    },
                    MapInfo {
                        id: data_id,
                        flags: MapFlags::READ | MapFlags::WRITE,
                    },
                )
                .map_err(|_| DynlinkErrorKind::NewBackingFail)?;

            if data_handle.monitor_data_start() as usize
                != text_handle.monitor_data_start() as usize + MAX_SIZE
            {
                tracing::error!(
                                "internal runtime error: failed to map text and data adjacent and in-order ({:p} {:p})",
                                text_handle.monitor_data_start(),
                                data_handle.monitor_data_start(),
                            );
                return Err(DynlinkErrorKind::NewBackingFail.into());
            }
            unsafe {
                Ok((
                    Backing::new_owned(
                        text_handle.monitor_data_start(),
                        MAX_SIZE - NULLPAGE_SIZE * 2,
                        text_id,
                        text_handle,
                        src.full_name().to_owned(),
                    ),
                    Backing::new_owned(
                        data_handle.monitor_data_start(),
                        MAX_SIZE - NULLPAGE_SIZE * 2,
                        data_id,
                        data_handle,
                        src.full_name().to_owned(),
                    ),
                ))
            }
        };
        dynlink::engines::twizzler::load_segments(src, ld, instance, map)
    }

    fn load_object(&mut self, unlib: &UnloadedLibrary) -> Result<Backing, DynlinkError> {
        let (id, full) = name_resolver(&unlib.name)?;
        let mapping = Space::map(
            &get_monitor().space,
            MapInfo {
                id,
                flags: MapFlags::READ,
            },
        )
        .map_err(|_err| DynlinkErrorKind::NewBackingFail)?;
        Ok(unsafe {
            Backing::new_owned(
                mapping.monitor_data_start(),
                MAX_SIZE - NULLPAGE_SIZE * 2,
                id,
                mapping,
                full,
            )
        })
    }

    fn select_compartment(
        &mut self,
        _unlib: &UnloadedLibrary,
    ) -> Option<dynlink::compartment::CompartmentId> {
        Some(MONITOR_COMPARTMENT_ID)
    }
}

static NAMING: OnceLock<NameStore> = OnceLock::new();

pub fn set_naming(root: ObjID) -> Result<(), TwzError> {
    NAMING
        .set(NameStore::new_with_root(root)?)
        .map_err(|_| NamingError::AlreadyBound.into())
}

pub fn naming() -> Option<&'static NameStore> {
    NAMING.get()
}

fn do_name_resolver(name: &str) -> Result<(ObjID, String), DynlinkError> {
    if let Some(namer) = naming() {
        let session = namer.root_session();
        let node = session.get(name, GetFlags::FOLLOW_SYMLINK).map_err(|_| {
            DynlinkErrorKind::NameNotFound {
                name: name.to_string(),
            }
        })?;
        return match node.kind {
            NsNodeKind::Object => Ok((node.id, name.to_string())),
            _ => Err(DynlinkErrorKind::NameNotFound {
                name: name.to_string(),
            }
            .into()),
        };
    }

    find_init_name(name).ok_or(
        DynlinkErrorKind::NameNotFound {
            name: name.to_string(),
        }
        .into(),
    )
}

fn name_resolver(mut name: &str) -> Result<(ObjID, String), DynlinkError> {
    if name.starts_with("libstd") {
        name = "libstd.so";
    }
    if name.starts_with("libtest") {
        name = "libtest.so";
    }

    if let Ok(r) = do_name_resolver(name) {
        return Ok(r);
    }

    let initrdname = format!("/initrd/{}", name);
    do_name_resolver(initrdname.as_str())
}

pub fn get_kernel_init_info() -> &'static KernelInitInfo {
    unsafe {
        (((twizzler_abi::slot::RESERVED_KERNEL_INIT * MAX_SIZE) + NULLPAGE_SIZE)
            as *const KernelInitInfo)
            .as_ref()
            .unwrap()
    }
}

fn find_init_name(name: &str) -> Option<(ObjID, String)> {
    let init_info = get_kernel_init_info();
    for n in init_info.names() {
        if n.name() == name {
            return Some((n.id(), name.to_owned()));
        }
    }
    None
}
