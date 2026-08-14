#[cfg(test)]
mod tests;

mod consistency;
mod cursor;
mod mapper;
mod phys_provider;
mod reader;
mod settings;
mod table;

pub use consistency::{
    Consistency, DeferredUnmappingOps, fill_stats, print_switch_counters, tlb_shootdown_inc_count,
    trace_tlb_invalidation, trace_tlb_shootdown,
};
pub use cursor::MappingCursor;
pub use mapper::Mapper;
pub use phys_provider::{
    ContiguousProvider, PhysAddrProvider, PhysMapInfo, UninitPageProvider, ZeroPageProvider,
};
pub use reader::{MapInfo, MapReader};
pub use settings::{MappingFlags, MappingSettings};

pub use crate::arch::memory::pagetables::Table;
