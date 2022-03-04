#[repr(u8)]
pub enum Layer4Prot {
    None = 0,
}

impl From<Layer4Prot> for u8 {
    fn from(x: Layer4Prot) -> Self {
        x as u8
    }
}
