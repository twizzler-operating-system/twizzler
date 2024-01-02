//! Management of individual libraries.

use std::fmt::Debug;

use elf::{abi::PT_PHDR, endian::NativeEndian, segment::Elf64_Phdr, ParseError};

mod deps;
mod init;
mod load;
mod relocate;
mod tls;

pub use init::CtorInfo;
pub use load::LibraryLoader;

use petgraph::stable_graph::NodeIndex;
use twizzler_abi::object::MAX_SIZE;
use twizzler_object::Object;

use crate::{compartment::Compartment, tls::TlsModId};

/// State of relocation.
#[derive(Debug)]
#[repr(u32)]
pub(crate) enum RelocState {
    /// The library has not been relocated.
    Unrelocated,
    /// The library is currently being relocated.
    Relocating,
    /// The library is relocated.
    Relocated,
}

#[allow(dead_code)]
#[derive(Debug)]
#[repr(u32)]
pub(crate) enum InitState {
    /// No constructors have been called.
    Uninit,
    /// This library has been loaded as part of the static set, but hasn't been initialized (waiting for runtime entry).
    StaticUninit,
    /// Constructors have been called, destructors have not been called.
    Constructed,
    /// Destructors have been called.
    Deconstructed,
}

pub trait BackingData {
    fn data(&self) -> (*mut u8, usize);
    fn new_data() -> Self;
}

pub struct UnloadedLibrary {
    pub name: String,
}

pub struct Library<Backing: BackingData> {
    /// Name of this library.
    pub name: String,
    /// Node index for the dependency graph.
    pub(crate) idx: NodeIndex,
    /// Object containing the full ELF data.
    pub full_obj: Object<u8>,
    /// State of relocation (see [RelocState]).
    reloc_state: RelocState,
    /// State of initialization (see [InitState]).
    init_state: InitState,

    /// Object containing R-X segments, if any.
    pub text_object: Option<Backing>,
    /// Object containing RW- segments, if any.
    pub data_object: Option<Backing>,
    /// Load base address of this library, used for relocations.
    pub base_addr: Option<usize>,

    /// The module ID for the TLS region, if any.
    pub tls_id: Option<TlsModId>,

    /// Information about constructors, if any.
    pub(crate) ctors: Option<CtorInfo>,
}

#[allow(dead_code)]
impl Library {
    pub fn new(obj: Object<u8>, name: impl ToString) -> Self {
        Self {
            comp_id: 0,
            name: name.to_string(),
            idx: Cell::new(None),
            full_obj: obj,
            reloc_state: AtomicU32::default(),
            init_state: AtomicU32::default(),
            text_object: None,
            data_object: None,
            base_addr: None,
            tls_id: None,
            ctors: None,
        }
    }

    pub fn used_slots(&self) -> Vec<usize> {
        let inner = self.inner.lock().unwrap();
        let mut v = vec![inner.full_obj.slot().slot_number()];
        if let Some(ref text) = inner.text_object {
            v.push(text.slot().slot_number());
        }
        if let Some(ref data) = inner.data_object {
            v.push(data.slot().slot_number());
        }
        v
    }

    pub fn get_phdrs_raw(&self) -> Option<(*const Elf64_Phdr, usize)> {
        Some((
            self.get_elf().ok()?.segments()?.iter().find_map(|p| {
                if p.p_type == PT_PHDR {
                    Some(self.laddr(p.p_vaddr))
                } else {
                    None
                }
            })??,
            self.get_elf().ok()?.segments()?.len(),
        ))
    }

    pub(crate) fn set_ctors(&mut self, ctors: CtorInfo) {
        self.inner.lock().unwrap().ctors = Some(ctors);
    }

    pub(crate) fn set_mapping(&mut self, data: Object<u8>, text: Object<u8>, base_addr: usize) {
        let inner = self.inner.lock.unwrap();
        inner.text_object = Some(text);
        inner.data_object = Some(data);
        inner.base_addr = Some(base_addr);
    }

    pub(crate) fn set_reloc_state(&self, state: RelocState) {
        self.reloc_state.store(state as u32, Ordering::SeqCst);
    }

    pub(crate) fn get_reloc_state(&self) -> RelocState {
        match self.reloc_state.load(Ordering::SeqCst) {
            0 => RelocState::Unrelocated,
            1 => RelocState::Relocating,
            2 => RelocState::Relocated,
            x => panic!("unexpected relocation state: {}", x),
        }
    }

    pub(crate) fn try_set_reloc_state(&self, old: RelocState, new: RelocState) -> bool {
        self.reloc_state
            .compare_exchange(old as u32, new as u32, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    pub(crate) fn try_set_init_state(&self, old: InitState, new: InitState) -> bool {
        self.init_state
            .compare_exchange(old as u32, new as u32, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    pub(crate) fn set_init_state(&self, state: InitState) {
        self.init_state.store(state as u32, Ordering::SeqCst);
    }

    /// Return a handle to the full ELF file.
    pub fn get_elf(&self) -> Result<elf::ElfBytes<'_, NativeEndian>, ParseError> {
        let slice =
            unsafe { core::slice::from_raw_parts(self.full_obj.base_unchecked(), MAX_SIZE) };
        elf::ElfBytes::minimal_parse(slice)
    }
}

impl Debug for Library {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Library")
            .field("name", &self.name)
            .field("comp_id", &self.comp_id)
            .finish()
    }
}

impl core::fmt::Display for Library {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", &self.name)
    }
}
