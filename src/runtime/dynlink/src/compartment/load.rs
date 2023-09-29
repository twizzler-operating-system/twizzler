use std::collections::{HashSet, VecDeque};

use tracing::debug;

use crate::{
    compartment::internal::InternalCompartment,
    context::Context,
    library::{
        Library, LibraryCollection, LibraryId, LibraryLoader, UnloadedLibrary, UnrelocatedLibrary,
    },
    AddLibraryError, AdvanceError,
};

use super::{CompartmentId, LibraryResolver, UnloadedCompartment};

impl UnloadedCompartment {
    pub fn new(name: impl ToString, id: CompartmentId) -> Self {
        Self {
            int: InternalCompartment::new(name.to_string(), id, None),
        }
    }
}

impl UnloadedCompartment {
    pub fn add_library(&mut self, lib: UnloadedLibrary) -> Result<LibraryId, AddLibraryError> {
        let id = lib.id();
        self.int.insert_library(lib.into());
        Ok(id)
    }
}

impl InternalCompartment {
    pub(crate) fn load_library(
        &mut self,
        lib: UnloadedLibrary,
        ctx: &mut Context,
        resolver: &mut LibraryResolver,
        loader: &mut LibraryLoader,
    ) -> Result<LibraryCollection<UnrelocatedLibrary>, AdvanceError> {
        debug!("loading library {}", lib);
        let (loaded_root, deps) = lib.load(ctx, resolver, loader)?;

        let mut queue: VecDeque<_> = deps.into();
        let mut names = HashSet::new();
        names.insert(loaded_root.name().to_owned());
        let mut deps = vec![];

        // Breadth-first. Root is done separately.
        while let Some(lib) = queue.pop_front() {
            if names.contains(lib.name()) {
                debug!("tossing duplicate dependency {}", lib.name());
                continue;
            }

            let (loaded, loaded_deps) = lib.load(ctx, resolver, loader)?;
            names.insert(loaded.name().to_owned());
            deps.push(loaded);
            for dep in loaded_deps {
                queue.push_back(dep);
            }
        }
        debug!("generated {} deps", deps.len());
        Ok((loaded_root, deps).into())
    }
}
