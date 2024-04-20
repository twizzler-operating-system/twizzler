mod pcie;
pub mod serial;

pub fn machine_post_init() {
    serial::late_init();
    pcie::init();
}
