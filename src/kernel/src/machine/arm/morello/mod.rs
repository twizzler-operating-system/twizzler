pub mod info;
pub mod interrupt;
pub mod memory;
pub mod processor;
pub mod serial;

pub fn machine_post_init() {
    // initialize uart with interrupts
    serial::SERIAL.late_init();
}
