use std::{
    collections::HashMap,
    sync::atomic::{AtomicU64, Ordering},
};

use dynlink::{
    compartment::CompartmentId,
    context::engine::{ContextEngine, Selector},
    engines::Engine,
    library::{LibraryId, UnloadedLibrary},
};
use twizzler_runtime_api::ObjID;
use twz_rt::CompartmentInitInfo;

use crate::{compman::runcomp::RunComp, find_init_name};

use super::{CompMan, CompManInner, COMPMAN};

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
}

#[derive(Debug)]
pub struct Loader {
    extra_compartments: Vec<ExtraCompInfo>,
    start_unload: LibraryId,
    root_comp: ExtraCompInfo,
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

/*
impl Loader {
    fn start_thread_in_compartment(
        rt_id: LibraryId,
        sctx_id: ObjID,
        rt_info: CompartmentInitInfo,
    ) -> miette::Result<()> {
        let (entry, rt_info) = { todo!() };

        tracing::debug!(
            "spawning thread in compartment {} at {:p} with {:x}",
            sctx_id,
            entry,
            rt_info
        );
        Ok(())
    }

    fn maybe_inject_rt(root_id: LibraryId, comp_id: CompartmentId) -> miette::Result<LibraryId> {
        let rt_unlib = UnloadedLibrary::new(RUNTIME_NAME);

        let dynlink = COMPMAN.lock().dynlink_mut();
        if let Some(id) = dynlink.lookup_library(comp_id, RUNTIME_NAME) {
            return Ok(id);
        }

        let loads = dynlink.load_library_in_compartment(comp_id, rt_unlib, &Sel)?;

        let rt_id = loads[0].lib;
        dynlink.add_manual_dependency(root_id, rt_id);
        Ok(rt_id)
    }

    pub fn run_a_crate(&mut self, name: &str, comp_name: &str) -> miette::Result<()> {
        let root_unlib = UnloadedLibrary::new(name);
        let dynlink = COMPMAN.lock().dynlink_mut();
        let root_comp_id = dynlink.add_compartment(comp_name)?;

        let loads = dynlink.load_library_in_compartment(root_comp_id, root_unlib, &Sel)?;

        tracing::warn!("==> {:#?}", loads);

        let mut cache = HashMap::new();
        self.extra_compartments = loads
            .iter()
            .filter_map(|load| {
                if load.comp != root_comp_id {
                    if let Ok(lib) = dynlink.get_library(load.lib) {
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

                        let rt_id = Loader::maybe_inject_rt(load.lib, load.comp).ok()?;

                        let sctx_id = (CTX_NUM.fetch_add(1, Ordering::SeqCst) as u128).into();
                        cache.insert(load.comp, sctx_id);
                        let dep_comp = RunComp::new().unwrap();
                        Some(ExtraCompInfo {
                            root_id: load.lib,
                            rt_id,
                            sctx_id,
                            comp: dep_comp,
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
        self.start_unload = Some(root_id);
        tracing::info!("loaded {} as {}", name, root_id);

        let rt_id = Loader::maybe_inject_rt(root_id, root_comp_id)?;
        dynlink.relocate_all(root_id)?;

        let sctx_id: ObjID = (CTX_NUM.fetch_add(1, Ordering::SeqCst) as u128).into();
        let root_comp = RunComp::new().unwrap();

        let extra_ctors = loader
            .extra_compartments
            .iter()
            .filter_map(|extra_info| {
                let comp = state.lookup_comp(extra_info.sctx_id)?;
                Some((
                    state.dynlink.build_ctors_list(extra_info.root_id).ok()?,
                    comp.get_comp_config() as *const _ as usize,
                    extra_info.sctx_id,
                ))
            })
            .collect::<Vec<_>>();

        drop(state);

        for (ctors, comp_config_addr, sctx_id) in extra_ctors {
            let rtinfo = CompartmentInitInfo {
                ctor_array_start: ctors.as_ptr() as usize,
                ctor_array_len: ctors.len(),
                comp_config_addr,
            };
            Loader::start_thread_in_compartment(&self.state, rt_id, sctx_id, rtinfo)?;
        }

        let mut state = self.state.lock().unwrap();

        let ctors = state.dynlink.build_ctors_list(root_id).unwrap();
        // TODO: allocate this in-compartment
        let rtinfo = CompartmentInitInfo {
            ctor_array_start: ctors.as_ptr() as usize,
            ctor_array_len: ctors.len(),
            comp_config_addr: root_comp.get_comp_config() as *const _ as usize,
        };
        state.add_comp(root_comp, root_id.into());

        drop(state);
        tracing::trace!("entry runtime info: {:?}", rtinfo);
        Loader::start_thread_in_compartment(&self.state, rt_id, sctx_id, rtinfo)?;
        Ok(())
    }
}
        */

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

                        let sctx_id = (CTX_NUM.fetch_add(1, Ordering::SeqCst) as u128).into();
                        cache.insert(load.comp, sctx_id);
                        let dep_comp = RunComp::new(
                            sctx_id,
                            sctx_id,
                            &inner.dynlink().get_compartment(load.comp).unwrap().name,
                            load.comp,
                            load.lib,
                        )
                        .unwrap();
                        Some(ExtraCompInfo {
                            root_id: load.lib,
                            rt_id,
                            sctx_id,
                            comp: dep_comp,
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

        let sctx_id = (CTX_NUM.fetch_add(1, Ordering::SeqCst) as u128).into();
        let root_comp = RunComp::new(sctx_id, sctx_id, comp_name, root_comp_id, root_id).unwrap();

        Ok(Loader {
            extra_compartments,
            start_unload: root_id,
            root_comp: ExtraCompInfo {
                root_id,
                rt_id,
                sctx_id,
                comp: root_comp,
            },
        })
    }
}
