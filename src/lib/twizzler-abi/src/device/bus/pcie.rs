use crate::kso::KactionError;

#[repr(C)]
pub struct PcieInfo {
    pub bus_start: u8,
    pub bus_end: u8,
    pub seg_nr: u16,
}

#[repr(u32)]
pub enum PcieKactionSpecific {
    RegisterDevice = 0,
    AllocateInterrupt = 1,
}

impl TryFrom<u32> for PcieKactionSpecific {
    type Error = KactionError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        Ok(match value {
            0 => PcieKactionSpecific::RegisterDevice,
            1 => PcieKactionSpecific::AllocateInterrupt,
            _ => Err(KactionError::InvalidArgument)?,
        })
    }
}
