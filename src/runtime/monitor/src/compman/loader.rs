use std::{
    borrow::Borrow,
    cell::{Cell, RefCell},
    collections::HashMap,
    mem::ManuallyDrop,
    sync::atomic::{AtomicU64, Ordering},
};

use dynlink::{
    compartment::CompartmentId,
    context::engine::{ContextEngine, Selector},
    engines::Engine,
    library::{CtorInfo, LibraryId, UnloadedLibrary},
};
use twizzler_abi::syscall::{BackingType, ObjectCreate, ObjectCreateFlags};
use twizzler_runtime_api::ObjID;
use twz_rt::CompartmentInitInfo;

use super::{runcomp::RunCompReadyWaiter, CompMan, CompManInner, COMPMAN};
use crate::{compman::runcomp::RunComp, find_init_name, init::InitDynlinkContext};

struct Sel;

impl Selector<Engine> for Sel {
    fn resolve_name(&self, mut name: &str) -> Option<<Engine as ContextEngine>::Backing> {
        if name.starts_with("libstd-") {
            name = "libstd.so";
        }
        let id = find_init_name(name)?;
        let obj = twizzler_runtime_api::get_runtime()
            .map_object(id, twizzler_runtime_api::MapFlags::READ)
            .ok()?;
        Some(<Engine as ContextEngine>::Backing::new(obj))
    }
}

const RUNTIME_NAME: &'static str = "libtwz_rt.so";
static CTX_NUM: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
pub struct ExtraCompInfo {
    pub root_id: LibraryId,
    pub rt_id: LibraryId,
    pub sctx_id: ObjID,
    pub comp: RunComp,
    pub ctor_info: Vec<CtorInfo>,
    pub entry_point: usize,
}

#[derive(Debug)]
pub struct Loader {
    extra_compartments: Vec<ExtraCompInfo>,
    start_unload: LibraryId,
    root_comp: Option<ExtraCompInfo>,
}

impl Drop for Loader {
    fn drop(&mut self) {
        tracing::warn!("TODO: unload library");
        while let Some(extra) = self.extra_compartments.pop() {
            tracing::warn!("TODO: unload extra compartment");
        }
        tracing::warn!("TODO: unload root compartment")
    }
}

impl Loader {
    fn maybe_inject_rt(root_id: LibraryId, comp_id: CompartmentId) -> miette::Result<LibraryId> {
        let rt_unlib = UnloadedLibrary::new(RUNTIME_NAME);

        let mut inner = COMPMAN.lock();
        if let Some(id) = inner.dynlink().lookup_library(comp_id, RUNTIME_NAME) {
            return Ok(id);
        }

        let loads = inner
            .dynlink_mut()
            .load_library_in_compartment(comp_id, rt_unlib, &Sel)?;

        let rt_id = loads[0].lib;
        inner.dynlink_mut().add_manual_dependency(root_id, rt_id);
        Ok(rt_id)
    }

    pub fn start_main(mut self) -> miette::Result<RunCompReadyWaiter> {
        tracing::debug!(
            "starting main threads for dependency compartments of {}",
            self.root_comp.as_ref().unwrap().comp.name()
        );
        let start_comp = |info: ExtraCompInfo| -> miette::Result<RunCompReadyWaiter> {
            let ExtraCompInfo {
                comp,
                ctor_info,
                entry_point,
                sctx_id,
                ..
            } = info;
            tracing::debug!("starting thread for {}", comp.name());
            let waiter = {
                let mut inner = COMPMAN.lock();
                inner.insert(comp);

                let comp = inner.lookup(sctx_id).unwrap();
                let waiter = comp.ready_waiter();
                let comp_inner = comp.cloned_inner();
                drop(inner);
                comp_inner
                    .lock()
                    .unwrap()
                    .start_main(&ctor_info, entry_point)?;
                waiter
            };
            Ok(waiter)
        };

        for dep in self.extra_compartments.drain(..).rev() {
            let waiter = start_comp(dep)?;
            waiter.wait();
        }

        let root_comp = self.root_comp.take().unwrap();
        start_comp(root_comp)
    }
}

impl CompManInner {
    fn maybe_inject_rt(
        &mut self,
        root_id: LibraryId,
        comp_id: CompartmentId,
    ) -> miette::Result<LibraryId> {
        let rt_unlib = UnloadedLibrary::new(RUNTIME_NAME);

        if let Some(id) = self.dynlink().lookup_library(comp_id, RUNTIME_NAME) {
            return Ok(id);
        }

        let loads = self
            .dynlink_mut()
            .load_library_in_compartment(comp_id, rt_unlib, &Sel)?;

        let rt_id = loads[0].lib;
        self.dynlink_mut().add_manual_dependency(root_id, rt_id);
        Ok(rt_id)
    }
}

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

impl CompMan {
    pub fn load_compartment(
        &self,
        comp_name: &str,
        root_unlib: UnloadedLibrary,
    ) -> miette::Result<Loader> {
        let mut inner = self.lock();
        let root_comp_id = inner.dynlink_mut().add_compartment(comp_name)?;
        let loads = inner.dynlink_mut().load_library_in_compartment(
            root_comp_id,
            root_unlib.clone(),
            &Sel,
        )?;
        tracing::warn!("==> {:#?}", loads);
        let mut cache = HashMap::new();

        // TODO: collect errors
        let extra_compartments = loads
            .iter()
            .filter_map(|load| {
                if load.comp != root_comp_id {
                    if let Ok(lib) = inner.dynlink().get_library(load.lib) {
                        if cache.contains_key(&load.comp) {
                            tracing::info!(
                                "load alt compartment library {}: {} (existing)",
                                lib,
                                load.comp
                            );
                            return None;
                        }
                        tracing::info!(
                            "load returned alternate compartment for library {}: {}",
                            lib,
                            load.comp
                        );

                        let rt_id = inner.maybe_inject_rt(load.lib, load.comp).ok()?;

                        let sctx_id = get_new_sctx_instance(1.into());
                        cache.insert(load.comp, sctx_id);
                        let dep_comp = RunComp::new(
                            sctx_id,
                            sctx_id,
                            &inner.dynlink().get_compartment(load.comp).unwrap().name,
                            load.comp,
                            load.lib,
                        )
                        .unwrap();
                        dep_comp
                            .with_inner(|rc| rc.build_tls_template(inner.dynlink_mut()))
                            .ok()?;
                        let ctor_info = inner.dynlink().build_ctors_list(load.lib).ok()?;
                        let entry_point = inner
                            .dynlink()
                            .get_library(rt_id)
                            .unwrap()
                            .get_entry_address()
                            .ok()?;
                        Some(ExtraCompInfo {
                            root_id: load.lib,
                            rt_id,
                            sctx_id,
                            comp: dep_comp,
                            ctor_info,
                            entry_point,
                        })
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .collect();

        let root_id = loads[0].lib;
        tracing::info!("loaded {} as {}", root_unlib, root_id);

        let rt_id = inner.maybe_inject_rt(root_id, root_comp_id)?;
        inner.dynlink_mut().relocate_all(root_id)?;

        let sctx_id = get_new_sctx_instance(1.into());
        let root_comp = RunComp::new(sctx_id, sctx_id, comp_name, root_comp_id, root_id).unwrap();
        root_comp.with_inner(|rc| rc.build_tls_template(inner.dynlink_mut()))?;

        let ctor_info = inner.dynlink().build_ctors_list(root_id)?;
        let entry_point = inner
            .dynlink()
            .get_library(rt_id)
            .unwrap()
            .get_entry_address()?;

        Ok(Loader {
            extra_compartments,
            start_unload: root_id,
            root_comp: Some(ExtraCompInfo {
                root_id,
                rt_id,
                sctx_id,
                comp: root_comp,
                ctor_info,
                entry_point,
            }),
        })
    }
}
