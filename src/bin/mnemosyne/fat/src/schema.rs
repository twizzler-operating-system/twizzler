use alloc::boxed::Box;
use layout::{collections::raw::RawBytes, layout, Decode, Encode, Fixed};

#[derive(Debug, Clone, Copy)]
pub enum FATEntry {
    Block(u64),
    Reserved,
    None,
}

impl FATEntry {
    pub fn unwrap(self) -> Option<u64> {
        match self {
            FATEntry::Block(b) => Some(b),
            FATEntry::None => None,
            FATEntry::Reserved => panic!("Attempted to unwrap reserved FAT entry"),
        }
    }
}

impl Fixed for FATEntry {
    fn size() -> u64 {
        u64::size()
    }
}

impl Encode for FATEntry {
    fn encode<W: layout::Write + layout::Seek + layout::IO>(
        &self,
        writer: &mut W,
    ) -> Result<(), W::Error> {
        match self {
            FATEntry::Block(b) => b.encode(writer),
            FATEntry::Reserved => (u64::max_value() - 1).encode(writer),
            FATEntry::None => u64::max_value().encode(writer),
        }
    }
}

impl Decode for FATEntry {
    fn decode<R: layout::Read + layout::Seek + layout::IO>(
        reader: &mut R,
    ) -> Result<Self, R::Error> {
        u64::decode(reader).map(|b| {
            if b == u64::max_value() {
                FATEntry::None
            } else if b == u64::max_value() - 1 {
                FATEntry::Reserved
            } else {
                FATEntry::Block(b)
            }
        })
    }
}

#[layout]
#[derive(Debug, Clone)]
pub struct Superblock {
    pub magic: u64,

    pub block_size: u32,
    pub block_count: u64,
}

#[layout]
#[derive(Debug)]
pub struct FileSystem {
    #[sublayout]
    pub super_block: Superblock,

    #[dynamic]
    pub fat: Box<[FATEntry]>,

    #[sublayout]
    pub super_block_cp: Superblock,

    #[dynamic]
    pub obj_lookup: Box<[FATEntry]>,

    #[sublayout]
    pub rest: RawBytes,
}

pub type ObjLookupBucket = Box<[ONode]>;

#[layout]
#[derive(Debug)]
pub struct ONode {
    pub object_id: u128,
    pub size: u64,

    pub first_block: u64,
    pub reserved: [u64; 4],
}
