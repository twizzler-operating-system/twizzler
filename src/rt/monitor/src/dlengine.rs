use std::collections::BTreeMap;

use dynlink::{
    compartment::{CompartmentId, MONITOR_COMPARTMENT_ID},
    engines::{Backing, ContextEngine, LoadCtx},
    library::UnloadedLibrary,
    DynlinkError, DynlinkErrorKind,
};
use smallstr::SmallString;
use twizzler_abi::{
    aux::KernelInitInfo,
    object::{Protections, MAX_SIZE, NULLPAGE_SIZE},
    syscall::ObjectCreate,
};
use twizzler_rt_abi::object::{MapFlags, ObjID};
use twizzler_security::{SecCtxBase, SecCtxFlags};

use crate::mon::{
    get_monitor,
    space::{MapInfo, Space},
};

pub struct Engine {
    name_map: BTreeMap<String, ObjID>,
}

impl Engine {
    pub fn new() -> Self {
        let mut name_map = BTreeMap::new();
        let init_info = get_kernel_init_info();
        for n in init_info.names() {
            name_map.insert(n.name().to_string(), n.id());
        }
        Self { name_map }
    }

    fn name_resolver(&self, mut name: &str) -> Result<(ObjID, String), DynlinkError> {
        if name.starts_with("libstd") {
            name = "libstd.so";
        }
        if name.starts_with("libtest") {
            name = "libtest.so";
        }

        if let Some(id) = self.name_map.get(name) {
            return Ok((*id, name.to_string()));
        }
        Err(DynlinkError::new(DynlinkErrorKind::NameNotFound {
            name: SmallString::from_str(name),
        }))
    }
}

fn get_new_sctx_instance(_sctx: ObjID) -> ObjID {
    let sec_ctx = SecCtxBase::new(Protections::all(), SecCtxFlags::empty());

    let handle = crate::mon::space::Space::safe_create_and_map_object(
        get_monitor().space,
        ObjectCreate::default(),
        &[],
        &[],
        MapFlags::READ | MapFlags::WRITE,
    )
    .unwrap();

    let base = handle.monitor_data_base().cast::<SecCtxBase>();
    unsafe { base.write(sec_ctx) };
    handle.id()
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
            tracing::trace!(
                "map {}: {} {}",
                src.full_name(),
                text_handle.id(),
                data_handle.id()
            );
            unsafe {
                Ok((
                    Backing::new_owned(
                        text_handle.monitor_data_start(),
                        MAX_SIZE - NULLPAGE_SIZE * 2,
                        text_id,
                        text_handle,
                        src.full_name().into(),
                    ),
                    Backing::new_owned(
                        data_handle.monitor_data_start(),
                        MAX_SIZE - NULLPAGE_SIZE * 2,
                        data_id,
                        data_handle,
                        src.full_name().into(),
                    ),
                ))
            }
        };
        dynlink::engines::twizzler::load_segments(src, ld, instance, map)
    }

    fn load_object(&mut self, unlib: &UnloadedLibrary) -> Result<Backing, DynlinkError> {
        let (id, full) = if unlib.id.is_some() {
            (unlib.id.unwrap(), unlib.name.clone())
        } else {
            self.name_resolver(&unlib.name)?
        };
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

pub fn get_kernel_init_info() -> &'static KernelInitInfo {
    unsafe {
        (((twizzler_abi::slot::RESERVED_KERNEL_INIT * MAX_SIZE) + NULLPAGE_SIZE)
            as *const KernelInitInfo)
            .as_ref()
            .unwrap()
    }
}
