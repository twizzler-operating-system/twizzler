use std::{cell::Cell, fmt::Debug, sync::Arc};

use elf::{endian::NativeEndian, ParseError};

mod initialize;
mod load;
mod name;
mod relocate;

pub use load::LibraryLoader;
use petgraph::stable_graph::NodeIndex;
use twizzler_object::Object;

pub type LibraryRef = Arc<Library>;

pub struct Library {
    pub(crate) comp_id: u64,
    pub(crate) name: String,
    pub(crate) idx: Cell<Option<NodeIndex>>,
}

impl Library {
    pub fn get_elf(&self) -> Result<elf::ElfBytes<'_, NativeEndian>, ParseError> {
        todo!()
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

impl From<Object<u8>> for Library {
    fn from(value: Object<u8>) -> Self {
        todo!()
    }
}
