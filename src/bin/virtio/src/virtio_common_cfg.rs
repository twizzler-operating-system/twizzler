extern crate twizzler_abi;

struct CommonCfg {
    // About the whole device
    device_feature_select: u32, // read-write, little endian
    device_feature: u32, // read-only for driver, little endian
    driver_feature_select: u32, // read-write, little endian
    driver_feature: u32, // read-write, little endian
    config_msix_vector: u16, // read-write, little endian
    num_queues: u16, // read-only for driver, little endian
    device_status: u8, // read-write
    config_generation: u8, // read-only for driver

    // About a specific virtqueue
    queue_select: u16, // read-write, little endian
    queue_size: u16, // read-write, little endian
    queue_msix_vector: u16, // read-write, little endian
    queue_enable: u16, // read-write, little endian
    queue_notify_off: u32, // read-write, little endian
    queue_desc: u64, // read-write, little endian
    queue_driver: u64, // read-write, little endian
    queue_device: u64, // read-write, little endian
    queue_notify_data: u16, // read-write, little endian
    queue_reset: u16, // read-write, little endian
}   