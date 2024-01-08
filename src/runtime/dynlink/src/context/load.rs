use std::mem::size_of;

use elf::{
    abi::{DT_INIT, DT_INIT_ARRAY, DT_INIT_ARRAYSZ, DT_PREINIT_ARRAY, DT_PREINIT_ARRAYSZ, PT_TLS},
    endian::NativeEndian,
    ParseError,
};
use petgraph::stable_graph::NodeIndex;
use tracing::{debug, trace, warn};

use crate::{
    compartment::{Compartment, CompartmentId},
    context::engine::{LoadDirective, LoadFlags},
    library::{BackingData, CtorInfo, Library, LibraryId, UnloadedLibrary},
    tls::TlsModule,
    DynlinkError, DynlinkErrorKind,
};

use super::{engine::ContextEngine, Context, LoadedOrUnloaded};

impl<Engine: ContextEngine> Context<Engine> {
    fn get_elf(
        &self,
        backing: &Engine::Backing,
    ) -> Result<elf::ElfBytes<'_, NativeEndian>, ParseError> {
        let slice = unsafe { core::slice::from_raw_parts(backing.data().0, backing.data().1) };
        elf::ElfBytes::minimal_parse(slice)
    }

    pub(crate) fn get_ctor_info(
        &self,
        libname: &str,
        elf: &elf::ElfBytes<'_, NativeEndian>,
        base_addr: usize,
    ) -> Result<CtorInfo, DynlinkError> {
        let dynamic = elf
            .dynamic()?
            .ok_or_else(|| DynlinkErrorKind::MissingSection {
                name: "dynamic".to_string(),
            })?;
        // If this isn't present, just call it 0, since if there's an init_array, this entry must be present in valid ELF files.
        let init_array_len = dynamic
            .iter()
            .find_map(|d| {
                if d.d_tag == DT_INIT_ARRAYSZ {
                    Some((d.d_val() as usize) / size_of::<usize>())
                } else {
                    None
                }
            })
            .unwrap_or_default();
        // Init array is a pointer to an array of function pointers.
        let init_array = dynamic.iter().find_map(|d| {
            if d.d_tag == DT_INIT_ARRAY {
                Some(base_addr + d.d_ptr() as usize)
            } else {
                None
            }
        });

        // Legacy _init call. Supported for, well, legacy.
        let leg_init = dynamic.iter().find_map(|d| {
            if d.d_tag == DT_INIT {
                Some(base_addr + d.d_ptr() as usize)
            } else {
                None
            }
        });

        if dynamic.iter().any(|d| d.d_tag == DT_PREINIT_ARRAY)
            && dynamic
                .iter()
                .find(|d| d.d_tag == DT_PREINIT_ARRAYSZ)
                .is_some_and(|d| d.d_val() > 0)
        {
            warn!("{}: PREINIT_ARRAY is unsupported", libname);
        }

        debug!(
            "{}: ctor info: init_array: {:?} len={}, legacy: {:?}",
            libname, init_array, init_array_len, leg_init
        );
        Ok(CtorInfo {
            legacy_init: leg_init.unwrap_or_default(),
            init_array: init_array.unwrap_or_default(),
            init_array_len,
        })
    }

    // Load (map) a single library into memory via creating two objects, one for text, and one for data.
    pub fn load<N>(
        &mut self,
        comp_id: CompartmentId,
        unlib: UnloadedLibrary,
        idx: NodeIndex,
        n: N,
    ) -> Result<Library<Engine::Backing>, DynlinkError>
    where
        N: FnMut(&str) -> Option<Engine::Backing>,
    {
        let backing = self.engine.load_object(&unlib, n)?;
        let elf = self.get_elf(&backing)?;
        // TODO: sanity check

        // Step 1: map the PT_LOAD directives to copy-from commands Twizzler can use for creating objects.
        let directives: Vec<_> = elf
            .segments()
            .ok_or_else(|| DynlinkErrorKind::MissingSection {
                name: "segment info".to_string(),
            })?
            .iter()
            .filter(|p| p.p_type == elf::abi::PT_LOAD)
            .map(|phdr| {
                let ld = LoadDirective {
                    load_flags: if phdr.p_flags & elf::abi::PF_W != 0 {
                        LoadFlags::TARGETS_DATA
                    } else {
                        LoadFlags::empty()
                    },
                    vaddr: phdr.p_vaddr as usize,
                    memsz: phdr.p_memsz as usize,
                    offset: phdr.p_offset as usize,
                    align: phdr.p_align as usize,
                    filesz: phdr.p_filesz as usize,
                };

                trace!("{}: {:?}", unlib, ld);

                ld
            })
            .collect();

        let backings = self.engine.load_segments(&backing, &directives)?;
        if backings.len() == 0 {
            return Err(DynlinkErrorKind::LibraryLoadFail { library: unlib }.into());
        }
        let base_addr = backings[0].load_addr();
        debug!(
            "{}: loaded to {:x} (data at {:x})",
            unlib,
            base_addr,
            backings.get(1).map(|b| b.load_addr()).unwrap_or_default()
        );

        // Need to grab this again to reborrow.
        let elf = self.get_elf(&backing)?;
        let tls_phdr = elf
            .segments()
            .and_then(|phdrs| phdrs.iter().find(|phdr| phdr.p_type == PT_TLS));

        let tls_id = tls_phdr.map(|tls_phdr| {
            let formatter = humansize::make_format(humansize::BINARY);
            debug!(
                "{}: registering TLS data ({} total, {} copy)",
                unlib,
                formatter(tls_phdr.p_memsz),
                formatter(tls_phdr.p_filesz)
            );
            let tm = TlsModule::new_static(
                base_addr + tls_phdr.p_vaddr as usize,
                tls_phdr.p_filesz as usize,
                tls_phdr.p_memsz as usize,
                tls_phdr.p_align as usize,
            );
            let comp = &mut self.get_compartment_mut(comp_id);
            comp.insert(tm)
        });

        let elf = self.get_elf(&backing)?;
        let ctor_info = self.get_ctor_info(&unlib.name, &elf, base_addr)?;

        Ok(Library::new(
            unlib.name, idx, backing, backings, tls_id, ctor_info,
        ))
    }

    fn find_cross_compartment_library(
        &self,
        unlib: &UnloadedLibrary,
    ) -> Option<(NodeIndex, CompartmentId, &Compartment<Engine::Backing>)> {
        for (idx, comp) in self.compartments.iter() {
            if let Some(lib) = comp.library_names.get(&unlib.name) {
                // TODO: only do this if it actually has secure gates.
                return Some((*lib, CompartmentId(idx), comp));
            }
        }

        None
    }

    pub(crate) fn load_library<N>(
        &mut self,
        comp_id: CompartmentId,
        unlib: UnloadedLibrary,
        idx: NodeIndex,
        n: N,
    ) -> Result<NodeIndex, DynlinkError>
    where
        N: FnMut(&str) -> Option<Engine::Backing> + Clone,
    {
        debug!("loading library {}", unlib);
        let lib = self.load(comp_id, unlib.clone(), idx, n.clone())?;

        let deps = self.enumerate_needed(&lib)?;
        if !deps.is_empty() {
            debug!("{}: loading {} dependencies", unlib, deps.len());
        }
        let deps = deps
            .into_iter()
            .map(|unlib| {
                // Dependency search + load alg:
                // 1. Search library name in current compartment. If found, use that.
                // 2. Fallback to searching globally for the name, by checking compartment by compartment. If found, use that.
                // 3. Okay, now we know we need to load the dep, so check if it can go in the current compartment. If not, create a new compartment.
                // 4. Finally, recurse to load it and its dependencies into either the current compartment or the new one, if created.

                let comp = self.get_compartment(comp_id);

                let (existing_idx, load_comp) =
                    if let Some(existing) = comp.library_names.get(&unlib.name) {
                        debug!(
                            "using existing library for {} (intra-compartment)",
                            unlib.name
                        );
                        (Some(*existing), comp_id)
                    } else if let Some((existing, other_comp_id, other_comp)) =
                        self.find_cross_compartment_library(&unlib)
                    {
                        debug!(
                            "using existing library for {} (cross-compartment -> {})",
                            unlib.name, other_comp.name
                        );
                        (Some(existing), other_comp_id)
                    } else {
                        (
                            None,
                            self.engine.select_compartment(&unlib).unwrap_or(comp_id),
                        )
                    };

                let idx = if let Some(existing_idx) = existing_idx {
                    existing_idx
                } else {
                    let idx = self.add_library(unlib.clone());

                    let comp = self.get_compartment_mut(load_comp);
                    comp.library_names.insert(unlib.name.clone(), idx);
                    self.load_library(comp_id, unlib, idx, n.clone())?
                };
                self.add_dep(&lib, idx);
                Ok(idx)
            })
            .collect::<Vec<Result<_, DynlinkError>>>();

        let _ = DynlinkError::collect(DynlinkErrorKind::LibraryLoadFail { library: unlib }, deps)?;

        assert_eq!(idx, lib.idx);
        self.library_deps[idx] = LoadedOrUnloaded::Loaded(lib);
        Ok(idx)
    }

    /// Load a library into a given compartment. The namer callback resolves names to Backing objects.
    pub fn load_library_in_compartment<N>(
        &mut self,
        comp_id: CompartmentId,
        unlib: UnloadedLibrary,
        namer: N,
    ) -> Result<LibraryId, DynlinkError>
    where
        N: FnMut(&str) -> Option<Engine::Backing> + Clone,
    {
        let idx = self.add_library(unlib.clone());
        // Step 1: insert into the compartment's library names.
        let comp = self.get_compartment_mut(comp_id);

        // At this level, it's an error to insert an already loaded library.
        if comp.library_names.contains_key(&unlib.name) {
            return Err(DynlinkErrorKind::NameAlreadyExists {
                name: unlib.name.clone(),
            }
            .into());
        }
        comp.library_names.insert(unlib.name.clone(), idx);

        // Step 2: load the library. This call recurses on dependencies.
        let idx = self.load_library(comp_id, unlib.clone(), idx, namer)?;
        match &self.library_deps[idx] {
            LoadedOrUnloaded::Unloaded(_) => {
                Err(DynlinkErrorKind::LibraryLoadFail { library: unlib }.into())
            }
            LoadedOrUnloaded::Loaded(_) => Ok(LibraryId(idx)),
        }
    }
}
