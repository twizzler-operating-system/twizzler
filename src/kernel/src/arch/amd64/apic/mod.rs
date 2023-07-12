mod ipi;
mod local;
mod trampolines;

pub use ipi::send_ipi;
pub(super) use local::{get_lapic, init, lapic_interrupt};
pub use trampolines::poke_cpu;
