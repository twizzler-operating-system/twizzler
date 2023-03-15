use std::{collections::hash_map::DefaultHasher, hash::Hasher, sync::Arc};

use tickv::{ErrorCode, FlashController};

use crate::nvme::NvmeController;

pub struct Storage {
    nvme: Arc<NvmeController>,
}

impl Storage {
    pub fn new(nvme: Arc<NvmeController>) -> Self {
        Self { nvme }
    }
}

pub const BLOCK_SIZE: usize = 4096;

impl FlashController<BLOCK_SIZE> for Storage {
    fn read_region(
        &self,
        region_number: usize,
        offset: usize,
        buf: &mut [u8; BLOCK_SIZE],
    ) -> Result<(), tickv::ErrorCode> {
        println!("read: {} {}", region_number, offset);
        twizzler_async::block_on(self.nvme.read_page(region_number as u64 * 8, buf, offset))
            .map_err(|_| tickv::ErrorCode::ReadFail)
    }

    fn write(&self, address: usize, buf: &[u8]) -> Result<(), tickv::ErrorCode> {
        println!("write: {} {}", address, buf.len());
        twizzler_async::block_on(self.nvme.write_page(
            (address / BLOCK_SIZE) as u64 * 8,
            buf,
            address % BLOCK_SIZE,
        ))
        .map_err(|_| tickv::ErrorCode::WriteFail)
    }

    fn erase_region(&self, region_number: usize) -> Result<(), tickv::ErrorCode> {
        println!("erase: {}", region_number);
        twizzler_async::block_on(self.nvme.write_page(
            region_number as u64 * 8,
            &[0xffu8; BLOCK_SIZE],
            0,
        ))
        .map_err(|_| tickv::ErrorCode::WriteFail)
    }
}

pub struct KeyValueStore<'a> {
    internal: tickv::tickv::TicKV<'a, Storage, BLOCK_SIZE>,
}

pub fn hasher<T: std::hash::Hash>(t: T) -> u64 {
    let mut h = DefaultHasher::new();
    t.hash(&mut h);
    h.finish()
}

impl<'a> KeyValueStore<'a> {
    pub fn new(
        storage: Storage,
        read_buffer: &'a mut [u8; BLOCK_SIZE],
        size: usize,
    ) -> Result<Self, ErrorCode> {
        let this = Self {
            internal: tickv::tickv::TicKV::new(storage, read_buffer, size),
        };
        this.internal.initialise(hasher(tickv::tickv::MAIN_KEY))?;
        Ok(this)
    }

    pub fn get(
        &self,
        hash: u64,
        buf: &mut [u8],
    ) -> Result<tickv::success_codes::SuccessCode, tickv::ErrorCode> {
        self.internal.get_key(hash, buf)
    }

    pub fn put(
        &self,
        hash: u64,
        buf: &[u8],
    ) -> Result<tickv::success_codes::SuccessCode, tickv::ErrorCode> {
        self.internal.append_key(hash, buf)
    }

    pub fn del(&self, hash: u64) -> Result<tickv::success_codes::SuccessCode, tickv::ErrorCode> {
        self.internal.invalidate_key(hash)
    }
}
