#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Gates {
    pub offset: u64,
    pub length: u64,
    pub align: u64,
}

//NOTE: ask daniel about this
static MAX_LEN: f32 = 1e9;

pub enum GatesError {
    OutsideBounds,
    Unaligned,
}

impl Gates {
    pub fn new(offset: u64, length: u64, align: u64) -> Self {
        Gates {
            offset,
            length,
            align,
        }
    }
}

impl Default for Gates {
    fn default() -> Self {
        //NOTE: verify with daniel that these are the default values for gates
        Gates {
            offset: 0,
            length: MAX_LEN as u64,
            align: 0,
        }
    }
}
