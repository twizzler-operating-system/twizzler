pub mod twizzler;

use elf::{endian::NativeEndian, ParseError};
use twizzler_abi::object::{MAX_SIZE, NULLPAGE_SIZE};
use twizzler_runtime_api::ObjectHandle;

use crate::{compartment::CompartmentId, library::UnloadedLibrary, DynlinkError};

/// System-specific implementation functions for the dynamic linker, mostly
/// involving loading objects.
pub trait ContextEngine {
    /// Load a given source backing into new backings, according to the given load directives.
    fn load_segments(
        &mut self,
        src: &Backing,
        ld: &[LoadDirective],
    ) -> Result<Vec<Backing>, DynlinkError>;

    /// Load a single object, based on the given unloaded library.
    fn load_object(&mut self, unlib: &UnloadedLibrary) -> Result<Backing, DynlinkError>;

    /// Select which compartment a library should go in.
    fn select_compartment(&mut self, unlib: &UnloadedLibrary) -> Option<CompartmentId>;
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

/// A backing type for the dynamic linker. Contains a handle to an object, and abstractions
/// for treating Twizzler objects as object files.
#[derive(Clone)]
pub struct Backing {
    obj: twizzler_runtime_api::ObjectHandle,
}

impl Backing {
    pub fn new(inner: twizzler_runtime_api::ObjectHandle) -> Self {
        Self { obj: inner }
    }
}

impl Backing {
    pub(crate) fn data(&self) -> (*mut u8, usize) {
        (
            unsafe { self.obj.start.add(NULLPAGE_SIZE) },
            // one null-page, one meta-page
            MAX_SIZE - NULLPAGE_SIZE * 2,
        )
    }

    /// Get the underlying object handle.
    pub fn object(&self) -> &ObjectHandle {
        &self.obj
    }

    pub(crate) fn load_addr(&self) -> usize {
        self.obj.start as usize
    }

    pub(crate) fn slice(&self) -> &[u8] {
        let data = self.data();
        // Safety: a loaded library may have a slice constructed of its backing data.
        unsafe { core::slice::from_raw_parts(data.0, data.1) }
    }

    /// Get the ELF file for this backing.
    pub(crate) fn get_elf(&self) -> Result<elf::ElfBytes<'_, NativeEndian>, ParseError> {
        elf::ElfBytes::minimal_parse(self.slice())
    }
}
