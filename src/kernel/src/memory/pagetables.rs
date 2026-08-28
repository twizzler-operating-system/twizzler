#[cfg(test)]
mod tests;

mod consistency;
mod cursor;
mod mapper;
mod phys_provider;
mod reader;
mod settings;
mod table;
pub mod zeroprobe;

pub use consistency::{
    Consistency, DeferredUnmappingOps, TlbOrigin, fill_stats, invl_census,
    print_shootdown_counters, print_switch_counters, tlb_shootdown_inc_count, tlb_wait_record,
    trace_tlb_invalidation, trace_tlb_shootdown,
};
pub use cursor::MappingCursor;
pub use mapper::Mapper;
pub use phys_provider::{
    ContiguousProvider, FrameSliceProvider, PhysAddrProvider, PhysMapInfo, UninitPageProvider,
    ZeroPageProvider,
};
pub use reader::{MapInfo, MapReader};
pub use settings::{MappingFlags, MappingSettings};

pub use crate::arch::memory::pagetables::Table;

pub fn nonleaf_cow_print() {
    table::nonleaf_cow::print();
}
