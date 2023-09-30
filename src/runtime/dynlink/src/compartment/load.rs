use std::collections::{BTreeMap, HashSet, VecDeque};

use talc::{ErrOnOom, Talc};
use tracing::debug;

use crate::{
    compartment::internal::InternalCompartment,
    context::Context,
    library::{LibraryCollection, LibraryId, LibraryLoader, UnloadedLibrary, UnrelocatedLibrary},
    AddLibraryError, AdvanceError,
};

use super::{CompartmentId, LibraryResolver, UnloadedCompartment};

impl UnloadedCompartment {
    pub fn new(name: impl ToString, id: CompartmentId) -> Self {
        Self {
            int: InternalCompartment::new(
                id,
                BTreeMap::new(),
                BTreeMap::new(),
                None,
                vec![],
                Talc::new(ErrOnOom),
                name.to_string(),
            ),
        }
    }
}

impl UnloadedCompartment {
    pub fn add_library(&mut self, lib: UnloadedLibrary) -> Result<LibraryId, AddLibraryError> {
        let id = lib.internal().id();
        self.int.insert_library(lib.into());
        Ok(id)
    }

    pub fn id(&self) -> CompartmentId {
        self.internal().id
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
        names.insert(loaded_root.internal().name().to_owned());
        let mut deps = vec![];

        // Breadth-first. Root is done separately.
        while let Some(lib) = queue.pop_front() {
            if names.contains(lib.internal().name()) {
                debug!("tossing duplicate dependency {}", lib.internal().name());
                continue;
            }

            let (loaded, loaded_deps) = lib.load(ctx, resolver, loader)?;
            names.insert(loaded.internal().name().to_owned());
            deps.push(loaded);
            for dep in loaded_deps {
                queue.push_back(dep);
            }
        }
        debug!("generated {} deps", deps.len());
        Ok((loaded_root, deps).into())
    }
}
