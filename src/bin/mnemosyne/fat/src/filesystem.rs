use layout::{io::SeekFrom, ApplyLayout, Frame, Read, Seek, Write, IO};

use crate::{
    block_io::BlockIO,
    schema::{self, FATEntry, FileSystemFrame},
};

#[derive(Debug)]
pub enum FSError<E> {
    ObjNotFound,
    Full,
    OutOfBounds,
    UnexpectedEof,
    WriteZero,
    IO(E),
}

impl<E> From<E> for FSError<E> {
    fn from(e: E) -> Self {
        FSError::IO(e)
    }
}

pub struct FileSystem<S> {
    pub disk: S, // todo pub
}

impl<S: Read + Write + Seek + IO> FileSystem<S> {
    const MAGIC_NUM: u64 = 0x1e0_15_c001;

    pub fn open(disk: S) -> Self {
        Self { disk }
    }

    pub fn create(mut disk: S, block_size: u32) -> Result<Self, S::Error> {
        let block_count = disk.stream_len()? / block_size as u64;

        let mut frame = schema::FileSystem::apply_layout(&mut disk, 0)?;

        let super_block = schema::Superblock {
            magic: Self::MAGIC_NUM,
            block_size,
            block_count,
        };

        frame.set_super_block(&super_block)?;
        frame.fat()?.set_len(block_count)?;
        frame.set_super_block_cp(&super_block)?;

        let padding = block_size as u64 - frame.obj_lookup()?.offset() % block_size as u64;
        let mut obj_lookup = frame.obj_lookup()?;
        obj_lookup.set_len(padding + block_size as u64)?;
        for i in 0..obj_lookup.len() {
            obj_lookup.set(i, FATEntry::None)?;
        }

        let reserved_len = frame.rest()?.offset() / block_size as u64 + 1;
        let mut fat = frame.fat()?;
        for i in 0..reserved_len {
            fat.set(i, FATEntry::Reserved)?;
        }
        for i in reserved_len..block_count {
            fat.set(i, FATEntry::None)?;
        }

        Ok(Self { disk })
    }

    pub fn frame(&mut self) -> Result<FileSystemFrame<'_, S>, S::Error> {
        schema::FileSystem::apply_layout(&mut self.disk, 0)
    }

    pub(crate) fn alloc_block(&mut self) -> Result<u64, FSError<S::Error>> {
        let mut frame = self.frame()?;
        let mut fat = frame.fat()?;

        let free_head = fat.get(0)?.unwrap().ok_or(FSError::Full)?;
        let next_free = fat.get(free_head)?;
        fat.set(0, next_free)?;
        fat.set(free_head, FATEntry::None)?;

        Ok(free_head)
    }

    pub(crate) fn free_block(&mut self, block: u64) -> Result<(), FSError<S::Error>> {
        let mut frame = self.frame()?;
        let mut fat = frame.fat()?;

        let free_head = fat.get(0)?;
        fat.set(block, free_head)?;
        fat.set(0, FATEntry::Block(block))?;

        Ok(())
    }

    pub fn create_object(&mut self, obj_id: u128, size: u64) -> Result<bool, FSError<S::Error>> {
        let mut frame = self.frame()?;
        let mut obj_lookup = frame.obj_lookup()?;

        let hash = obj_id as u64 % obj_lookup.len();
        let bucket_start = obj_lookup.get(hash)?.unwrap();

        let mut new = false;
        let mut bucket_blocks = match bucket_start {
            Some(bucket_start) => BlockIO::from_block(self, bucket_start, false)?,
            None => {
                let bio = BlockIO::create(self, None)?.start_block(); // TODO

                self.frame()?
                    .obj_lookup()?
                    .set(hash, FATEntry::Block(bio))?;

                new = true;

                BlockIO::from_block(self, bio, false)?
            }
        };

        let bucket_start = bucket_blocks.start_block();

        let mut bucket = bucket_blocks.as_frame::<schema::ObjLookupBucket>()?;
        if new {
            bucket.set_len(0)?;
        }

        for i in 0..bucket.len() {
            let entry = bucket.get(i)?;
            if entry.object_id == obj_id {
                return Ok(false);
            }
        }

        let obj_start_block = self.alloc_block()?;
        // let mut oio = BlockIO::from_block(self, obj_start_block, false)?;
        // oio.write_all(&[obj_id as u8].repeat(size as usize))?;

        let mut bucket_blocks = BlockIO::from_block(self, bucket_start, false)?;
        let mut bucket = bucket_blocks.as_frame::<schema::ObjLookupBucket>()?;

        bucket.set_len(bucket.len() + 1)?;
        bucket.set(
            bucket.len() - 1,
            schema::ONode {
                object_id: obj_id,
                size,
                first_block: obj_start_block,
                reserved: Default::default(),
            },
        )?;

        Ok(true)
    }

    pub fn read_exact(
        &mut self,
        obj_id: u128,
        buf: &mut [u8],
        off: u64,
    ) -> Result<(), FSError<S::Error>> {
        let mut frame = self.frame()?;
        let mut obj_lookup = frame.obj_lookup()?;
        let bucket_start = obj_lookup.get(obj_id as u64 % obj_lookup.len())?;

        let bucket_start = match bucket_start {
            FATEntry::Block(b) => b,
            _ => return Err(FSError::ObjNotFound),
        };

        let mut bucket_blocks = BlockIO::from_block(self, bucket_start, true)?;
        let mut bucket = bucket_blocks.as_frame::<schema::ObjLookupBucket>()?;

        let mut data_start = None;
        for i in 0..bucket.len() {
            let entry = bucket.get(i)?;
            if entry.object_id == obj_id {
                data_start = Some(entry.first_block);
                break;
            }
        }

        let mut data_blocks =
            BlockIO::from_block(self, data_start.ok_or(FSError::ObjNotFound)?, true)?;
        data_blocks.seek(SeekFrom::Start(off))?;
        data_blocks.read_exact(buf)?;

        Ok(())
    }

    pub fn write_all(
        &mut self,
        obj_id: u128,
        buf: &[u8],
        off: u64,
    ) -> Result<(), FSError<S::Error>> {
        let mut frame = self.frame()?;
        let mut obj_lookup = frame.obj_lookup()?;
        let bucket_start = obj_lookup.get(obj_id as u64 % obj_lookup.len())?;

        let bucket_start = match bucket_start {
            FATEntry::Block(b) => b,
            _ => return Err(FSError::ObjNotFound),
        };

        let mut bucket_blocks = BlockIO::from_block(self, bucket_start, true)?;
        let mut bucket = bucket_blocks.as_frame::<schema::ObjLookupBucket>()?;

        let mut data_start = None;
        for i in 0..bucket.len() {
            let entry = bucket.get(i)?;
            if entry.object_id == obj_id {
                data_start = Some(entry.first_block);
                break;
            }
        }

        let mut data_blocks =
            BlockIO::from_block(self, data_start.ok_or(FSError::ObjNotFound)?, true)?;
        data_blocks.seek(SeekFrom::Start(off))?;
        data_blocks.write_all(buf)?;

        Ok(())
    }
}
