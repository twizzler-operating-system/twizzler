pub mod info;
pub mod interrupt;
pub mod memory;
mod pcie;
pub mod processor;
pub mod rtc;
pub mod serial;

pub fn machine_post_init() {
    // initialize uart with interrupts
    serial::serial().late_init();
    pcie::init();
}
