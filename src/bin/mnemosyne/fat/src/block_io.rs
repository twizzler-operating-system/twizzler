use alloc::vec;
use alloc::vec::Vec;
use layout::{io::SeekFrom, Read, Seek, Write, IO, ApplyLayout};

use crate::{
    filesystem::{FSError, FileSystem},
    schema::FATEntry,
};

pub struct BlockIO<'a, S> {
    fs: &'a mut FileSystem<S>,
    block_size: u64,

    blocks: Vec<u64>,

    cur_block_idx: u64,
    cur_offset: u64,

    fixed_size: bool,
}

impl<'a, S: Read + Write + Seek + IO> BlockIO<'a, S> {
    pub fn from_block(
        fs: &'a mut FileSystem<S>,
        start_block: u64,
        fixed_size: bool,
    ) -> Result<Self, FSError<S::Error>> {
        let block_size = fs.frame()?.super_block()?.block_size()? as u64;

        let mut this = Self {
            fs,
            block_size,

            blocks: vec![start_block],

            cur_block_idx: 0,
            cur_offset: 0,

            fixed_size,
        };

        this.seek(SeekFrom::Start(0))?;

        Ok(this)
    }

    pub fn create(
        fs: &'a mut FileSystem<S>,
        fixed_size: Option<u64>,
    ) -> Result<Self, FSError<S::Error>> {
        let block_size = fs.frame()?.super_block()?.block_size()? as u64;
        let head = fs.alloc_block()?;
        let mut this = Self {
            fs,
            block_size,
            blocks: vec![head],
            cur_block_idx: 0,
            cur_offset: 0,
            fixed_size: fixed_size.is_some(),
        };

        if let Some(len) = fixed_size {
            this.fill_blocks_to(Some(len / this.block_size + 1), true)?;
        }

        this.seek(SeekFrom::Start(0))?;
        Ok(this)
    }

    pub fn start_block(&self) -> u64 {
        self.blocks[0]
    }

    pub fn align_stream(&mut self) -> Result<(), <Self as IO>::Error> {
        self.seek(SeekFrom::Start(self.cur_offset)).map(|_| ())
    }
}

impl <'a, S: IO> BlockIO<'a, S> {
    pub fn as_frame<L: ApplyLayout<'a, Self>>(&'a mut self) -> Result<L::Frame, <Self as IO>::Error> {
        L::apply_layout(self, 0)
    }
}

impl<'a, S: Read + Write + Seek + IO> BlockIO<'a, S> {
    // warning: this ruins the current position in the stream
    fn fill_blocks_to(
        &mut self,
        block_count: Option<u64>,
        expand: bool,
    ) -> Result<(), FSError<S::Error>> {
        let max = block_count.unwrap_or(u64::max_value());

        let mut cur_block = *self.blocks.last().unwrap();
        while max > self.blocks.len() as u64 {
            match self.fs.frame()?.fat()?.get(cur_block)?.unwrap() {
                Some(next_block) => {
                    self.blocks.push(next_block);
                    cur_block = next_block;
                }
                None if block_count.is_none() || !expand => break,
                None => {
                    let next_block = self.fs.alloc_block()?;
                    self.blocks.push(next_block);
        
                    self.fs
                        .frame()?
                        .fat()?
                        .set(cur_block, FATEntry::Block(next_block))?;
    
                    cur_block = next_block;
                }
            }
        }

        Ok(())
    }
}

impl<'a, S: IO> IO for BlockIO<'a, S> {
    type Error = FSError<S::Error>;
}

impl<'a, S: Read + Write + Seek + IO> Seek for BlockIO<'a, S> {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64, Self::Error> {
        let (target_idx, next_offset) = match pos {
            SeekFrom::Start(off) => {
                let target_block = off / self.block_size;
                self.fill_blocks_to(Some(target_block + 1), false)?;

                (target_block, off)
            }
            SeekFrom::End(off) => {
                let target_block = self.blocks.len() as i64 + off / self.block_size as i64;

                if target_block < 0 {
                    return Err(FSError::OutOfBounds);
                }

                self.fill_blocks_to(None, false)?;

                (
                    target_block as u64,
                    (self.block_size as i64 * self.blocks.len() as i64 + off) as u64,
                )
            }
            SeekFrom::Current(off) => {
                let target_block = self.cur_block_idx as i64 + off / self.block_size as i64;

                if target_block < 0 {
                    return Err(FSError::OutOfBounds);
                }

                self.fill_blocks_to(Some(target_block as u64 + 1), false)?;

                (
                    target_block as u64,
                    (self.cur_offset as i64 + off) as u64,
                )
            }
        };

        self.cur_offset = next_offset;

        if target_idx == self.blocks.len() as u64 && next_offset % self.block_size == 0 {
            self.cur_block_idx = target_idx;

            return Ok(target_idx * self.block_size);
        }

        if let Some(&target) = self.blocks.get(target_idx as usize) {
            self.cur_block_idx = target_idx;

            self.fs.disk.seek(SeekFrom::Start(
                target * self.block_size + next_offset % self.block_size,
            ))?;

            Ok(self.cur_offset)
        } else {
            Err(FSError::OutOfBounds)
        }
    }

    fn stream_len(&mut self) -> Result<u64, Self::Error> {
        self.fill_blocks_to(None, false)?;
        self.align_stream()?;

        Ok(self.blocks.len() as u64 * self.block_size)
    }

    fn stream_position(&mut self) -> Result<u64, Self::Error> {
        Ok(self.cur_offset)
    }
}

impl<'a, S: Read + Write + Seek + IO> Read for BlockIO<'a, S> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        // todo: lol no eof on growable things
        if self.cur_block_idx == self.blocks.len() as u64 {
            self.fill_blocks_to(Some(self.blocks.len() as u64 + 1), !self.fixed_size)?;
            self.align_stream()?;

            if self.fixed_size && self.cur_block_idx == self.blocks.len() as u64 {
                return Ok(0);
            }
        }

        let trunc = buf.len().min((self.block_size - self.cur_offset % self.block_size) as usize);
        let cropped_buf = &mut buf[..trunc];
        let read = self.fs.disk.read(cropped_buf)?;

        self.cur_offset += read as u64;
        if self.cur_offset % self.block_size == 0 {
            self.cur_block_idx += 1;
        }

        Ok(read)
    }

    // modified from std::io::Read's provided definition for the corollary function
    // TODO: ignore interrupted reads by using underlying IO's read_exact definition
    fn read_exact(&mut self, mut buf: &mut [u8]) -> Result<(), Self::Error> {
        while !buf.is_empty() {
            match self.read(buf) {
                Ok(0) => break,
                Ok(n) => {
                    let tmp = buf;
                    buf = &mut tmp[n..];
                }
                Err(e) => return Err(e),
            }
        }

        if buf.is_empty() {
            Ok(())
        } else {
            Err(FSError::UnexpectedEof)
        }
    }
}

impl<'a, S: Read + Write + Seek + IO> Write for BlockIO<'a, S> {
    fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        if self.cur_block_idx == self.blocks.len() as u64 {
            self.fill_blocks_to(Some(self.blocks.len() as u64 + 1), !self.fixed_size)?;
            self.align_stream()?;

            if self.fixed_size && self.cur_block_idx == self.blocks.len() as u64{
                return Err(FSError::OutOfBounds);
            }
        }

        let trunc = buf.len().min((self.block_size - self.cur_offset % self.block_size) as usize);
        let cropped_buf = &buf[..trunc];

        let written = self.fs.disk.write(cropped_buf)?;

        self.cur_offset += written as u64;
        if self.cur_offset % self.block_size == 0 {
            self.cur_block_idx += 1;
        }

        Ok(written)
    }

    // modified from std::io::Writes's provided definition for the corollary function
    // TODO: ignore interrupted writes by using underlying IO's write_all definition
    fn write_all(&mut self, mut buf: &[u8]) -> Result<(), Self::Error> {
        while !buf.is_empty() {
            match self.write(buf) {
                Ok(0) => {
                    return Err(FSError::WriteZero);
                }
                Ok(n) => buf = &buf[n..],
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        self.fs.disk.flush().map_err(FSError::IO)
    }
}
