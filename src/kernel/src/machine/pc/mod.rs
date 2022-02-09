mod pcie;
pub mod serial;

pub fn machine_post_init() {
    pcie::init();
}
