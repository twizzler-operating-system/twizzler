use std::{
    cell::Cell,
    fmt::Debug,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
};

use elf::{endian::NativeEndian, ParseError};

mod deps;
mod init;
mod load;
mod relocate;
mod tls;

pub(crate) use init::CtorInfo;
pub use load::LibraryLoader;

use petgraph::stable_graph::NodeIndex;
use twizzler_abi::object::MAX_SIZE;
use twizzler_object::Object;

use crate::tls::TlsModId;

pub type LibraryRef = Arc<Library>;

#[derive(Debug)]
#[repr(u32)]
pub(crate) enum RelocState {
    Unrelocated,
    Relocating,
    Relocated,
}

#[allow(dead_code)]
#[derive(Debug)]
#[repr(u32)]
pub(crate) enum InitState {
    Uninit,
    Constructed,
    Deconstructed,
}

pub struct Library {
    pub(crate) comp_id: u128,
    pub(crate) name: String,
    pub(crate) idx: Cell<Option<NodeIndex>>,
    pub(crate) full_obj: Object<u8>,
    reloc_state: AtomicU32,
    init_state: AtomicU32,

    pub(crate) text_object: Option<Object<u8>>,
    pub(crate) data_object: Option<Object<u8>>,
    pub(crate) base_addr: Option<usize>,

    pub(crate) tls_id: Option<TlsModId>,

    pub(crate) ctors: Option<CtorInfo>,
}

unsafe impl Sync for Library {}

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

    pub(crate) fn set_ctors(&mut self, ctors: CtorInfo) {
        self.ctors = Some(ctors);
    }

    pub(crate) fn set_mapping(&mut self, data: Object<u8>, text: Object<u8>, base_addr: usize) {
        self.text_object = Some(text);
        self.data_object = Some(data);
        self.base_addr = Some(base_addr);
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
