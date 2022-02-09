#[repr(C)]
pub struct PcieInfo {
    pub bus_start: u8,
    pub bus_end: u8,
    pub seg_nr: u16,
}
