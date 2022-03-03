#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Copy, Debug)]
pub struct EthernetAddr {
    bytes: [u8; 6],
}

impl From<[u8; 6]> for EthernetAddr {
    fn from(x: [u8; 6]) -> Self {
        Self { bytes: x }
    }
}

impl EthernetAddr {
    pub fn broadcast() -> Self {
        Self { bytes: [0xff; 6] }
    }

    pub fn local() -> Self {
        Self { bytes: [0; 6] }
    }
}

pub enum EthernetError {
    #[allow(dead_code)]
    Unknown,
}
