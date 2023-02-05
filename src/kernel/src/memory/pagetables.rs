#[cfg(test)]
mod tests;

mod consistency;
mod cursor;
mod mapper;
mod phys_provider;
mod reader;
mod settings;
mod table;

pub use cursor::MappingCursor;

pub use crate::arch::pagetables::Table;
pub use mapper::Mapper;
pub use phys_provider::{PhysAddrProvider, PhysFrame};
pub use reader::{MapInfo, MapReader};
pub use settings::{MappingFlags, MappingSettings};
