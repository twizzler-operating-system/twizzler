mod ipi;
mod local;
mod trampolines;

pub use ipi::send_ipi;
pub(super) use local::{LAPIC_SPURIOUS_VECTOR, get_lapic, init, lapic_interrupt, try_get_lapic};
pub use trampolines::poke_cpu;
