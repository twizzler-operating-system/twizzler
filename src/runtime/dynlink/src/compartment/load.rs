use std::collections::VecDeque;

use tracing::{debug, error};

use crate::{
    compartment::{internal::InternalCompartment, Compartment, UnrelocatedCompartment},
    context::Context,
    library::{Library, LibraryId, UnloadedLibrary},
    AddLibraryError, AdvanceError,
};

use elf::abi::DT_NEEDED;

use super::{CompartmentId, LibraryResolver, UnloadedCompartment};

impl UnloadedCompartment {
    pub fn new(id: CompartmentId) -> Self {
        Self {
            int: InternalCompartment::new(id, None),
        }
    }
}

impl UnloadedCompartment {
    pub fn advance(
        self,
        mut library_resolver: LibraryResolver,
        ctx: &mut Context,
    ) -> Result<UnrelocatedCompartment, AdvanceError> {
        debug!("advancing compartment {}", self.int);
        let mut next = InternalCompartment::new(self.id(), self.int.dep_start());

        let mut queue: VecDeque<_> = self.int.into_values().collect();

        while let Some(lib) = queue.pop_front() {
            //TODO: check if we have loaded it already
            debug!("enumerating needed libraries for {}", lib);
            let id = lib.id();
            let elf = lib.get_elf().map_err(|_| AdvanceError::LibraryFailed(id))?;
            let common = elf.find_common_data()?;

            let neededs = common
                .dynamic
                .ok_or(AdvanceError::LibraryFailed(id))?
                .iter()
                .filter_map(|d| match d.d_tag {
                    DT_NEEDED => Some({
                        let name = common
                            .dynsyms_strs
                            .ok_or(AdvanceError::LibraryFailed(id))
                            .map(|strs| {
                                strs.get(d.d_ptr() as usize)
                                    .map_err(|e| AdvanceError::ParseError(e))
                            })
                            .flatten();
                        name.map(|name| {
                            let dep = library_resolver.resolve(name.into());
                            if dep.is_err() {
                                error!("failed to resolve library {} (needed by {})", name, lib);
                            }
                            dep.map_err(|_| AdvanceError::LibraryFailed(id))
                        })
                        .flatten()
                    }),
                    _ => None,
                });
            for needed in neededs {
                if let Ok(needed) = needed {
                    debug!("adding {} (needed by {})", needed, lib);
                    queue.push_back(needed);
                } else {
                }
            }

            next.insert_library(lib.load(ctx)?);
        }

        Ok(UnrelocatedCompartment { int: next })
    }

    pub fn add_library(&mut self, lib: UnloadedLibrary) -> Result<LibraryId, AddLibraryError> {
        let id = lib.id();
        self.int.insert_library(lib);
        Ok(id)
    }
}
