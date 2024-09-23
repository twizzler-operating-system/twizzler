use std::mem::size_of;

use elf::{
    abi::{DT_INIT, DT_INIT_ARRAY, DT_INIT_ARRAYSZ, DT_PREINIT_ARRAY, DT_PREINIT_ARRAYSZ, PT_TLS},
    endian::NativeEndian,
    file::Class,
};
use petgraph::stable_graph::NodeIndex;
use secgate::RawSecGateInfo;
use tracing::{debug, warn};

use super::{Context, LoadedOrUnloaded};
use crate::{
    compartment::{Compartment, CompartmentId},
    engines::{LoadDirective, LoadFlags},
    library::{CtorInfo, Library, LibraryId, SecgateInfo, UnloadedLibrary},
    tls::TlsModule,
    DynlinkError, DynlinkErrorKind, HeaderError,
};

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Ord, Eq, Hash)]
pub struct LoadIds {
    pub comp: CompartmentId,
    pub lib: LibraryId,
}

impl From<&Library> for LoadIds {
    fn from(value: &Library) -> Self {
        Self {
            comp: value.comp_id,
            lib: value.id(),
        }
    }
}

impl Context {
    pub(crate) fn get_secgate_info(
        &self,
        libname: &str,
        elf: &elf::ElfBytes<'_, NativeEndian>,
        base_addr: usize,
    ) -> Result<SecgateInfo, DynlinkError> {
        let info = elf
            .section_header_by_name(".twz_secgate_info")?
            .map(|info| SecgateInfo {
                info_addr: Some((info.sh_addr as usize) + base_addr),
                num: (info.sh_size as usize) / core::mem::size_of::<RawSecGateInfo>(),
            })
            .unwrap_or_default();

        debug!(
            "{}: registered secure gate info: {} gates",
            libname, info.num
        );

        Ok(info)
    }
    // Collect information about constructors.
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

        // If this isn't present, just call it 0, since if there's an init_array, this entry must be
        // present in valid ELF files.
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
            if d.d_tag == DT_INIT_ARRAY && d.clone().d_ptr() != 0 {
                Some(base_addr + d.d_ptr() as usize)
            } else {
                None
            }
        });

        // Legacy _init call. Supported for, well, legacy.
        let leg_init = dynamic.iter().find_map(|d| {
            if d.d_tag == DT_INIT && d.clone().d_ptr() != 0 {
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

    // Load (map) a single library into memory via creating two objects, one for text, and one for
    // data.
    fn load(
        &mut self,
        comp_id: CompartmentId,
        unlib: UnloadedLibrary,
        idx: NodeIndex,
    ) -> Result<Library, DynlinkError> {
        let backing = self.engine.load_object(&unlib)?;
        let elf = backing.get_elf()?;

        // Step 0: sanity check the ELF header.

        const EXPECTED_CLASS: Class = Class::ELF64;
        const EXPECTED_VERSION: u32 = 1;
        const EXPECTED_ABI: u8 = elf::abi::ELFOSABI_SYSV;
        const EXPECTED_ABI_VERSION: u8 = 0;
        const EXPECTED_TYPE: u16 = elf::abi::ET_DYN;

        #[cfg(target_arch = "x86_64")]
        const EXPECTED_MACHINE: u16 = elf::abi::EM_X86_64;

        #[cfg(target_arch = "aarch64")]
        const EXPECTED_MACHINE: u16 = elf::abi::EM_AARCH64;

        if elf.ehdr.class != EXPECTED_CLASS {
            return Err(DynlinkErrorKind::from(HeaderError::ClassMismatch {
                expect: Class::ELF64,
                got: elf.ehdr.class,
            })
            .into());
        }

        if elf.ehdr.version != EXPECTED_VERSION {
            return Err(DynlinkErrorKind::from(HeaderError::VersionMismatch {
                expect: EXPECTED_VERSION,
                got: elf.ehdr.version,
            })
            .into());
        }

        if elf.ehdr.osabi != EXPECTED_ABI {
            return Err(DynlinkErrorKind::from(HeaderError::OSABIMismatch {
                expect: EXPECTED_ABI,
                got: elf.ehdr.osabi,
            })
            .into());
        }

        if elf.ehdr.abiversion != EXPECTED_ABI_VERSION {
            return Err(DynlinkErrorKind::from(HeaderError::ABIVersionMismatch {
                expect: EXPECTED_ABI_VERSION,
                got: elf.ehdr.abiversion,
            })
            .into());
        }

        if elf.ehdr.e_machine != EXPECTED_MACHINE {
            return Err(DynlinkErrorKind::from(HeaderError::MachineMismatch {
                expect: EXPECTED_MACHINE,
                got: elf.ehdr.e_machine,
            })
            .into());
        }

        if elf.ehdr.e_type != EXPECTED_TYPE {
            return Err(DynlinkErrorKind::from(HeaderError::ELFTypeMismatch {
                expect: EXPECTED_TYPE,
                got: elf.ehdr.e_type,
            })
            .into());
        }

        // Step 1: map the PT_LOAD directives to copy-from commands Twizzler can use for creating
        // objects.
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

                debug!("{}: {:?}", unlib, ld);

                ld
            })
            .collect();

        // call the system impl to actually map things
        let backings = self.engine.load_segments(&backing, &directives)?;
        if backings.is_empty() {
            return Err(DynlinkErrorKind::NewBackingFail.into());
        }
        let base_addr = backings[0].load_addr();
        debug!(
            "{}: loaded to {:x} (data at {:x})",
            unlib,
            base_addr,
            backings.get(1).map(|b| b.load_addr()).unwrap_or_default()
        );

        // Step 2: look for any TLS information, stored in program header PT_TLS.
        let tls_phdr = elf
            .segments()
            .and_then(|phdrs| phdrs.iter().find(|phdr| phdr.p_type == PT_TLS));

        let tls_id = tls_phdr
            .map(|tls_phdr| {
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
                let comp = &mut self.get_compartment_mut(comp_id)?;
                Ok::<_, DynlinkError>(comp.insert(tm))
            })
            .transpose()?;

        debug!("{}: got TLS ID {:?}", unlib, tls_id);

        // Step 3: lookup constructor and secgate information for this library.
        let ctor_info = self.get_ctor_info(&unlib.name, &elf, base_addr)?;
        let secgate_info = self.get_secgate_info(&unlib.name, &elf, base_addr)?;

        let comp = self.get_compartment(comp_id)?;
        Ok(Library::new(
            unlib.name,
            idx,
            comp.id,
            comp.name.clone(),
            backing,
            backings,
            tls_id,
            ctor_info,
            secgate_info,
        ))
    }

    fn find_cross_compartment_library(
        &self,
        unlib: &UnloadedLibrary,
    ) -> Option<(NodeIndex, CompartmentId, &Compartment)> {
        for (idx, comp) in self.compartments.iter().enumerate() {
            if let Some(lib_id) = comp.library_names.get(&unlib.name) {
                let lib = self.get_library(LibraryId(*lib_id));
                if let Ok(lib) = lib {
                    // Only allow cross-compartment refs for a library that has secure gates.
                    if lib.secgate_info.info_addr.is_some() {
                        return Some((*lib_id, CompartmentId(idx), comp));
                    }
                    return None;
                }
            }
        }

        None
    }

    fn has_secgate_info(&self, elf: &elf::ElfBytes<'_, NativeEndian>) -> bool {
        elf.section_header_by_name(".twz_secgate_info")
            .ok()
            .is_some_and(|s| s.is_some())
    }

    fn select_compartment(
        &mut self,
        unlib: &UnloadedLibrary,
        parent_comp_name: String,
    ) -> Option<CompartmentId> {
        let backing = self.engine.load_object(unlib).ok()?;
        let elf = backing.get_elf().ok()?;
        if self.has_secgate_info(&elf) {
            let name = format!("{}::{}", parent_comp_name, unlib.name);
            let id = self.add_compartment(&name).ok()?;
            tracing::debug!(
                "creating new compartment {}({}) for library {}",
                name,
                id,
                unlib.name
            );
            // TODO: Handle collisions
            Some(id)
        } else {
            None
        }
    }

    // Load a library and all its deps, using the supplied name resolution callback for deps.
    pub(crate) fn load_library(
        &mut self,
        comp_id: CompartmentId,
        root_unlib: UnloadedLibrary,
        idx: NodeIndex,
    ) -> Result<Vec<LoadIds>, DynlinkError> {
        let root_comp_name = self.get_compartment(comp_id)?.name.clone();
        debug!(
            "loading library {} (idx = {:?}) in {}",
            root_unlib, idx, root_comp_name
        );
        let mut ids = vec![];
        // First load the main library.
        let lib = self.load(comp_id, root_unlib.clone(), idx).map_err(|e| {
            DynlinkError::new_collect(
                DynlinkErrorKind::LibraryLoadFail {
                    library: root_unlib.clone(),
                },
                vec![e],
            )
        })?;
        ids.push((&lib).into());

        // Second, go through deps
        let deps = self.enumerate_needed(&lib).map_err(|e| {
            DynlinkError::new_collect(
                DynlinkErrorKind::DepEnumerationFail {
                    library: root_unlib.name.to_string(),
                },
                vec![e],
            )
        })?;
        if !deps.is_empty() {
            debug!("{}: loading {} dependencies", root_unlib, deps.len());
        }
        let deps = deps
            .into_iter()
            .map(|dep_unlib| {
                // Dependency search + load alg:
                // 1. Search library name in current compartment. If found, use that.
                // 2. Fallback to searching globally for the name, by checking compartment by
                //    compartment. If found, use that.
                // 3. Okay, now we know we need to load the dep, so check if it can go in the
                //    current compartment. If not, create a new compartment.
                // 4. Finally, recurse to load it and its dependencies into either the current
                //    compartment or the new one, if created.

                let comp = self.get_compartment(comp_id)?;
                let (existing_idx, load_comp) =
                    if let Some(existing) = comp.library_names.get(&dep_unlib.name) {
                        debug!(
                            "{}: dep using existing library for {} (intra-compartment in {}): {:?}",
                            root_unlib, dep_unlib.name, comp.name, existing
                        );
                        (Some(*existing), comp_id)
                    } else if let Some((existing, other_comp_id, other_comp)) =
                        self.find_cross_compartment_library(&dep_unlib)
                    {
                        debug!(
                            "{}: dep using existing library for {} (cross-compartment to {}): {:?}",
                            root_unlib, dep_unlib.name, other_comp.name, existing
                        );
                        (Some(existing), other_comp_id)
                    } else {
                        (
                            None,
                            self.select_compartment(&dep_unlib, root_comp_name.clone())
                                .unwrap_or(comp_id),
                        )
                    };

                // If we decided to use an existing library, then use that. Otherwise, load into the
                // chosen compartment.
                let idx = if let Some(existing_idx) = existing_idx {
                    existing_idx
                } else {
                    let idx = self.add_library(dep_unlib.clone());

                    let comp = self.get_compartment_mut(load_comp)?;
                    comp.library_names.insert(dep_unlib.name.clone(), idx);
                    let mut recs = self
                        .load_library(load_comp, dep_unlib.clone(), idx)
                        .map_err(|e| {
                            DynlinkError::new_collect(
                                DynlinkErrorKind::LibraryLoadFail {
                                    library: dep_unlib.clone(),
                                },
                                vec![e],
                            )
                        })?;
                    ids.append(&mut recs);
                    idx
                };
                self.add_dep(lib.idx, idx);
                Ok(idx)
            })
            .collect::<Vec<Result<_, DynlinkError>>>();

        let _ = DynlinkError::collect(
            DynlinkErrorKind::LibraryLoadFail {
                library: root_unlib,
            },
            deps,
        )?;

        assert_eq!(idx, lib.idx);
        self.library_deps[idx] = LoadedOrUnloaded::Loaded(lib);
        Ok(ids)
    }

    /// Load a library into a given compartment.
    pub fn load_library_in_compartment(
        &mut self,
        comp_id: CompartmentId,
        unlib: UnloadedLibrary,
    ) -> Result<Vec<LoadIds>, DynlinkError> {
        let idx = self.add_library(unlib.clone());
        // Step 1: insert into the compartment's library names.
        let comp = self.get_compartment_mut(comp_id)?;

        // At this level, it's an error to insert an already loaded library.
        if comp.library_names.contains_key(&unlib.name) {
            return Err(DynlinkErrorKind::NameAlreadyExists {
                name: unlib.name.clone(),
            }
            .into());
        }
        comp.library_names.insert(unlib.name.clone(), idx);

        // Step 2: load the library. This call recurses on dependencies.
        self.load_library(comp_id, unlib.clone(), idx)
    }
}
