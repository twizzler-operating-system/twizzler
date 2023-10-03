use std::{cell::Cell, fmt::Debug, sync::Arc};

use elf::{endian::NativeEndian, ParseError};

mod initialize;
mod load;
mod name;
mod relocate;

pub use load::LibraryLoader;
use petgraph::stable_graph::NodeIndex;
use twizzler_abi::object::MAX_SIZE;
use twizzler_object::Object;

pub type LibraryRef = Arc<Library>;

pub struct Library {
    pub(crate) comp_id: u128,
    pub(crate) name: String,
    pub(crate) idx: Cell<Option<NodeIndex>>,
    pub(crate) full_obj: Object<u8>,
}

impl Library {
    pub fn new(obj: Object<u8>, name: impl ToString) -> Self {
        Self {
            comp_id: 0,
            name: name.to_string(),
            idx: Cell::new(None),
            full_obj: obj,
        }
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
