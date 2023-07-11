mod ipi;
mod local;
mod trampolines;

pub use ipi::send_ipi;
pub use local::{eoi, init, lapic_interrupt, read_monotonic_nanoseconds, schedule_oneshot_tick};
pub use trampolines::poke_cpu;
