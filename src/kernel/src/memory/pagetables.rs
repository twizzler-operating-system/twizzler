#[cfg(test)]
mod tests;

mod consistency;
mod cursor;
mod mapper;
mod phys_provider;
mod reader;
mod settings;
mod shared;
mod table;

pub use consistency::{trace_tlb_invalidation, trace_tlb_shootdown, DeferredUnmappingOps};
pub use cursor::MappingCursor;
pub use mapper::Mapper;
pub use phys_provider::{ContiguousProvider, PhysAddrProvider, PhysMapInfo, ZeroPageProvider};
pub use reader::{MapInfo, MapReader};
pub use settings::{MappingFlags, MappingSettings};
pub use shared::{free_shared_frame, SharedPageTable};

pub use crate::arch::memory::pagetables::Table;
