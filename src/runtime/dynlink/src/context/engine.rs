use crate::{
    compartment::CompartmentId,
    library::{BackingData, UnloadedLibrary},
    DynlinkError,
};

/// System-specific implementation functions for the dynamic linker, mostly
/// involving loading objects.
pub trait ContextEngine {
    type Backing: BackingData;

    /// Load a given source backing into new backings, according to the given load directives.
    fn load_segments(
        &mut self,
        src: &Self::Backing,
        ld: &[LoadDirective],
    ) -> Result<Vec<Self::Backing>, DynlinkError>;

    /// Load a single object, based on the given unloaded library.
    fn load_object<S: Selector<Self>>(
        &mut self,
        unlib: &UnloadedLibrary,
        select: &S,
    ) -> Result<Self::Backing, DynlinkError>
    where
        Self: Sized,
    {
        select.resolve_name(&unlib.name).ok_or_else(|| {
            DynlinkError::new(crate::DynlinkErrorKind::NameNotFound {
                name: unlib.name.clone(),
            })
        })
    }
}

/// A single load directive, matching closely with an ELF program header.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ord, Eq, Hash, Default)]
pub struct LoadDirective {
    pub load_flags: LoadFlags,
    pub vaddr: usize,
    pub memsz: usize,
    pub offset: usize,
    pub align: usize,
    pub filesz: usize,
}

bitflags::bitflags! {
    /// Some flags for a load directive.
    #[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ord, Eq, Hash, Default)]
    pub struct LoadFlags: u32 {
        /// This load directive specifies a data (writable) segment.
        const TARGETS_DATA = 1;
    }
}

pub trait Selector<E: ContextEngine> {
    fn resolve_name(&self, name: &str) -> Option<E::Backing>;
}
