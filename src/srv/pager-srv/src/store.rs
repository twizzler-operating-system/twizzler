use std::{
    cmp::min,
    collections::hash_map::DefaultHasher,
    hash::Hasher,
    mem::{size_of, MaybeUninit},
    sync::Arc,
};

use futures::executor::block_on;
use tickv::{success_codes::SuccessCode, ErrorCode, FlashController};
use twizzler_object::ObjID;

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
// TODO: don't hardcode this
pub const SECTORS_TO_BLOCK: usize = 8;

impl FlashController<BLOCK_SIZE> for Storage {
    fn read_region(
        &self,
        region_number: usize,
        offset: usize,
        buf: &mut [u8; BLOCK_SIZE],
    ) -> Result<(), tickv::ErrorCode> {
        block_on(self.nvme.read_page(region_number as u64 * 8, buf, offset))
            .map_err(|_| tickv::ErrorCode::ReadFail)
    }

    fn write(&self, mut address: usize, mut buf: &[u8]) -> Result<(), tickv::ErrorCode> {
        while !buf.is_empty() {
            let offset = address % BLOCK_SIZE;
            let start = (address / BLOCK_SIZE) * SECTORS_TO_BLOCK;
            let thislen = min(BLOCK_SIZE - offset, buf.len());

            block_on(self.nvme.write_page(start as u64, &buf[0..thislen], offset))
                .map_err(|_| tickv::ErrorCode::WriteFail)?;

            buf = &buf[thislen..buf.len()];
            address += thislen;
        }
        Ok(())
    }

    fn erase_region(&self, region_number: usize) -> Result<(), tickv::ErrorCode> {
        block_on(self.nvme.write_page(
            (region_number * SECTORS_TO_BLOCK) as u64,
            &[0xffu8; BLOCK_SIZE],
            0,
        ))
        .map_err(|_| tickv::ErrorCode::WriteFail)
    }
}

pub struct KeyValueStore<'a> {
    pub internal: tickv::tickv::TicKV<'a, Storage, BLOCK_SIZE>,
}

pub fn hasher<T: std::hash::Hash>(t: &T) -> u64 {
    let mut h = DefaultHasher::new();
    t.hash(&mut h);
    let x = h.finish();
    // Don't ever hash to 0, 1, MAX, or MAX-1. Makes the open addressing easier, and 0 and MAX-1 are
    // required for tickv.
    match x {
        0 => 2,
        u64::MAX => u64::MAX - 2,
        m => m,
    }
}

#[derive(Clone, Copy, Hash, PartialEq, PartialOrd, Ord, Eq, Debug)]
#[repr(C)]
pub struct Key {
    pub id: ObjID,
    pub info: u32,
    pub kind: KeyKind,
}

impl Key {
    pub fn new(id: ObjID, info: u32, kind: KeyKind) -> Self {
        Self { id, info, kind }
    }
}

#[derive(Clone, Copy, Hash, PartialEq, PartialOrd, Ord, Eq, Debug)]
#[repr(u32)]
pub enum KeyKind {
    ObjectInfo = 10,
    Tombstone = 42,
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

    pub fn do_get(&self, hash: u64, buf_size: usize) -> Result<(SuccessCode, Vec<u8>), ErrorCode> {
        let mut buf = Vec::new();
        buf.resize(buf_size, 0u8);
        match self.internal.get_key(hash, &mut buf) {
            Ok(s) => Ok((s, buf)),
            Err(ErrorCode::BufferTooSmall(l)) => self.do_get(hash, l),
            Err(e) => Err(e),
        }
    }

    fn convert<T: Copy>(buf: &[u8]) -> T {
        let mut mu = MaybeUninit::uninit();
        let num_bytes = std::mem::size_of::<T>();
        unsafe {
            let buffer = std::slice::from_raw_parts_mut(
                &mut mu as *mut MaybeUninit<T> as *mut u8,
                num_bytes,
            );
            buffer.copy_from_slice(&buf[0..num_bytes]);
            mu.assume_init()
        }
    }

    pub fn get<V: Copy>(&self, key: Key) -> Result<V, ErrorCode> {
        let mut hash = hasher(&key);
        let prev = hash.wrapping_sub(1);
        let size = size_of::<Key>() + size_of::<V>();
        while hash != prev {
            if hash == 0 || hash == u64::MAX {
                hash = hash.wrapping_add(1);
                continue;
            }
            let data = self.do_get(hash, size)?;
            let thiskey: Key = Self::convert(&data.1);
            if key == thiskey {
                return Ok(Self::convert(&data.1[size_of::<Key>()..]));
            }
            hash = hash.wrapping_add(1);
        }
        Err(ErrorCode::KeyNotFound)
    }

    pub fn put<V: Copy>(&mut self, key: Key, value: V) -> Result<SuccessCode, ErrorCode> {
        let mut hash = hasher(&key);
        let prev = hash.wrapping_sub(1);
        let size = size_of::<Key>() + size_of::<V>();
        let mut raw_value = Vec::new();
        let key_slice = unsafe {
            std::slice::from_raw_parts(&key as *const Key as *const u8, size_of::<Key>())
        };
        let val_slice =
            unsafe { std::slice::from_raw_parts(&value as *const V as *const u8, size_of::<V>()) };
        raw_value.extend_from_slice(key_slice);
        raw_value.extend_from_slice(val_slice);
        while hash != prev {
            if hash == 0 || hash == u64::MAX {
                hash = hash.wrapping_add(1);
                continue;
            }
            let data = self.do_get(hash, size);
            if let Ok(data) = data {
                let thiskey: Key = Self::convert(&data.1);
                if key == thiskey {
                    return Err(ErrorCode::KeyAlreadyExists);
                }
            } else {
                return self.internal.append_key(hash, &raw_value);
            }

            hash = hash.wrapping_add(1);
        }
        Err(ErrorCode::KeyNotFound)
    }

    pub fn del(&mut self, key: Key) -> Result<SuccessCode, ErrorCode> {
        let mut hash = hasher(&key);
        let prev = hash.wrapping_sub(1);
        let size = size_of::<Key>();
        while hash != prev {
            if hash == 0 || hash == u64::MAX {
                hash = hash.wrapping_add(1);
                continue;
            }
            let data = self.do_get(hash, size)?;
            let thiskey: Key = Self::convert(&data.1);
            if key == thiskey {
                return self.do_del(hash);
            }
            hash = hash.wrapping_add(1);
        }
        Err(ErrorCode::KeyNotFound)
    }

    pub fn do_del(&self, hash: u64) -> Result<SuccessCode, ErrorCode> {
        let next = hash.wrapping_add(1);
        let res = self.internal.get_key(next, &mut []);
        if let Err(ErrorCode::BufferTooSmall(_)) = res {
            // leave a tombstone
            let t = Key::new(0.into(), 0, KeyKind::Tombstone);
            let t_slice = unsafe {
                std::slice::from_raw_parts(&t as *const Key as *const u8, size_of::<Key>())
            };
            self.internal.invalidate_key(hash).unwrap();
            self.internal.append_key(hash, t_slice)
        } else {
            self.internal.invalidate_key(hash)
        }
    }
}
