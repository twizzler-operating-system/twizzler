use std::collections::{HashMap, HashSet};

use dynlink::{
    compartment::CompartmentId,
    context::{Context, LoadIds},
    library::{CtorInfo, LibraryId, UnloadedLibrary},
    DynlinkError,
};
use miette::IntoDiagnostic;
use twizzler_abi::syscall::{BackingType, ObjectCreate, ObjectCreateFlags};
use twizzler_runtime_api::{AuxEntry, ObjID};

use super::{CompartmentMgr, RunComp};

#[derive(Debug)]
pub struct RunCompLoader {
    loaded_extras: Vec<LoadInfo>,
    root_comp: LoadInfo,
}

#[derive(Debug, Clone)]
struct LoadInfo {
    root_id: LibraryId,
    rt_id: LibraryId,
    sctx_id: ObjID,
    name: String,
    comp_id: CompartmentId,
    ctor_info: Vec<CtorInfo>,
    entry: extern "C" fn(*const AuxEntry) -> !,
}

impl LoadInfo {
    fn new(
        dynlink: &mut Context,
        root_id: LibraryId,
        rt_id: LibraryId,
        sctx_id: ObjID,
    ) -> Result<Self, DynlinkError> {
        let lib = dynlink.get_library(rt_id)?;
        Ok(Self {
            root_id,
            rt_id,
            comp_id: lib.compartment(),
            sctx_id,
            name: dynlink.get_compartment(lib.compartment())?.name.clone(),
            ctor_info: dynlink.build_ctors_list(root_id, Some(lib.compartment()))?,
            entry: lib.get_entry_address()?,
        })
    }

    fn build_runcomp(&self) -> Result<RunComp, DynlinkError> {
        /*
        Ok(RunComp::new(
            self.sctx_id,
            self.sctx_id,
            self.name.clone(),
            self.comp_id,
            deps: vec![],
            comp_config,
            0,
        ))
        */
        todo!()
    }
}

impl Drop for RunCompLoader {
    fn drop(&mut self) {
        tracing::warn!("drop RunCompLoader: TODO");
    }
}

const RUNTIME_NAME: &'static str = "libtwz_rt.so";

fn get_new_sctx_instance(_sctx: ObjID) -> ObjID {
    // TODO
    twizzler_abi::syscall::sys_object_create(
        ObjectCreate::new(
            BackingType::Normal,
            twizzler_abi::syscall::LifetimeType::Volatile,
            None,
            ObjectCreateFlags::empty(),
        ),
        &[],
        &[],
    )
    .unwrap()
}

impl RunCompLoader {
    fn maybe_inject_runtime(
        dynlink: &mut Context,
        root_id: LibraryId,
        comp_id: CompartmentId,
    ) -> Result<LibraryId, DynlinkError> {
        if let Some(id) = dynlink.lookup_library(comp_id, RUNTIME_NAME) {
            return Ok(id);
        }

        let rt_unlib = UnloadedLibrary::new(RUNTIME_NAME);
        let loads = dynlink.load_library_in_compartment(comp_id, rt_unlib)?;
        dynlink.add_manual_dependency(root_id, loads[0].lib);
        Ok(loads[0].lib)
    }

    /// Build a new RunCompLoader. This will load and relocate libraries in the dynamic linker, but
    /// won't start compartment threads.
    pub fn new(
        dynlink: &mut Context,
        comp_name: &str,
        root_unlib: UnloadedLibrary,
    ) -> miette::Result<Self> {
        struct UnloadOnDrop(Vec<LoadIds>);
        impl Drop for UnloadOnDrop {
            fn drop(&mut self) {
                tracing::warn!("todo: drop");
            }
        }
        let root_comp_id = dynlink.add_compartment(comp_name)?;
        let loads =
            UnloadOnDrop(dynlink.load_library_in_compartment(root_comp_id, root_unlib.clone())?);

        // The dynamic linker gives us a list of loaded libraries, and which compartments they ended
        // up in. For each of those, we may need to inject the runtime library. Collect all
        // the information about the extra compartments.
        let mut cache = HashSet::new();
        let extra_compartments = loads.0.iter().filter_map(|load| {
            if load.comp != root_comp_id {
                // This compartment was loaded in addition to the root comp as part of our
                // initial load request. Check if we haven't seen it before.
                if cache.contains(&load.comp) {
                    return None;
                }
                cache.insert(load.comp);

                // Inject the runtime library, careful to collect the error and keep going.
                let rt_id = match Self::maybe_inject_runtime(dynlink, load.lib, load.comp) {
                    Ok(id) => id,
                    Err(e) => return Some(Err(e)),
                };
                Some(LoadInfo::new(
                    dynlink,
                    load.lib,
                    rt_id,
                    get_new_sctx_instance(1.into()),
                ))
            } else {
                None
            }
        });

        let extra_compartments = DynlinkError::collect(
            dynlink::DynlinkErrorKind::CompartmentLoadFail {
                compartment: comp_name.to_string(),
            },
            extra_compartments,
        )?;

        let root_id = loads.0[0].lib;
        let rt_id = Self::maybe_inject_runtime(dynlink, root_id, root_comp_id)?;

        let root_comp = LoadInfo::new(dynlink, root_id, rt_id, get_new_sctx_instance(1.into()))?;
        // We don't want to drop anymore, since now drop-cleanup will be handled by RunCompLoader.
        std::mem::forget(loads);
        Ok(RunCompLoader {
            loaded_extras: extra_compartments,
            root_comp,
        })
    }

    pub fn build_rcs(self, cmp: &mut CompartmentMgr) -> miette::Result<Vec<ObjID>> {
        let root_comp = &self.root_comp;
        let loaded_extras = &self.loaded_extras;

        let mut v = vec![];
        let extras = DynlinkError::collect(
            dynlink::DynlinkErrorKind::CompartmentLoadFail {
                compartment: root_comp.name.clone(),
            },
            loaded_extras.iter().map(|cmp| cmp.build_runcomp()),
        )?;
        for rc in extras.into_iter().rev() {
            let id = rc.instance;
            v.push(id);
            cmp.insert(rc);
        }

        let rc = root_comp.build_runcomp();
        let rc = match rc {
            Err(e) => {
                // TODO: unload extras
                return Err(e).into_diagnostic();
            }
            Ok(o) => o,
        };
        let id = rc.instance;
        v.insert(0, id);
        cmp.insert(rc);

        // TODO: build dependency graph for compartments.

        std::mem::forget(self);
        Ok(v)
    }
}
