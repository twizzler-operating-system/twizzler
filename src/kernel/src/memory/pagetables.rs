#[cfg(test)]
mod tests;

mod consistency;
mod cursor;
mod mapper;
mod phys_provider;
mod reader;
mod settings;
mod table;

pub use consistency::DeferredUnmappingOps;
pub use cursor::MappingCursor;
pub use mapper::Mapper;
pub use phys_provider::{ContiguousProvider, PhysAddrProvider, ZeroPageProvider};
pub use reader::{MapInfo, MapReader};
pub use settings::{MappingFlags, MappingSettings};

pub use crate::arch::memory::pagetables::Table;
