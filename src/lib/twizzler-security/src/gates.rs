#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Gates {
    pub offset: u64,
    pub length: u64,
    pub align: u64,
}

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
        Gates {
            offset: 0,
            // what is the max length of an obj?
            length: todo!(),
            align: 1,
        }
    }
}
