// pub mod buffered;
pub mod crypt;
// pub mod stat;
pub mod stdio;

use crate::consts::BLOCK_SIZE;

pub enum SeekFrom {
    Start(u64),
    End(i64),
    Current(i64),
}

pub trait Io {
    type Error: std::error::Error;
}

impl<T: Io> Io for &T {
    type Error = T::Error;
}

impl<T: Io> Io for &mut T {
    type Error = T::Error;
}

pub trait Read: Io {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error>;

    fn read_exact(&mut self, mut buf: &mut [u8]) -> Result<(), Self::Error> {
        while !buf.is_empty() {
            match self.read(buf) {
                Ok(0) => break,
                Ok(n) => buf = &mut buf[n..],
                Err(e) => return Err(e),
            }
        }

        // if !buf.is_empty() {
        //      panic!("unexpected EOF: failed to fill whole buffer");
        // } else {
        //     Ok(())
        // }
        Ok(())
    }

    fn read_to_end(&mut self, buf: &mut Vec<u8>) -> Result<usize, Self::Error>
    where
        Self: Seek,
    {
        let mut count = 0;
        let mut block = [0; BLOCK_SIZE];

        loop {
            match self.read(&mut block) {
                Ok(0) => break,
                Ok(n) => {
                    count += n;
                    buf.extend(&block[..n]);
                }
                Err(e) => return Err(e),
            }
        }

        Ok(count)
    }
}

impl<T: Read> Read for &mut T {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        T::read(self, buf)
    }
}

pub trait ReadAt: Io {
    fn read_at(&mut self, buf: &mut [u8], offset: u64) -> Result<usize, Self::Error>;

    fn read_exact_at(&mut self, mut buf: &mut [u8], mut offset: u64) -> Result<(), Self::Error> {
        while !buf.is_empty() {
            match self.read_at(buf, offset) {
                Ok(0) => break,
                Ok(n) => {
                    let tmp = buf;
                    buf = &mut tmp[n..];
                    offset += n as u64;
                }
                Err(e) => return Err(e),
            }
        }

        if !buf.is_empty() {
            panic!("unexpected EOF: failed to fill whole buffer");
        } else {
            Ok(())
        }
    }

    fn read_to_end_at(&mut self, buf: &mut Vec<u8>, mut offset: u64) -> Result<usize, Self::Error> {
        let mut count = 0;
        let mut block = [0; BLOCK_SIZE];

        loop {
            match self.read_at(&mut block, offset) {
                Ok(0) => break,
                Ok(n) => {
                    count += n;
                    offset += n as u64;
                    buf.extend(&block[..n]);
                }
                Err(e) => return Err(e),
            }
        }

        Ok(count)
    }
}

impl<T: ReadAt> ReadAt for &mut T {
    fn read_at(&mut self, buf: &mut [u8], offset: u64) -> Result<usize, Self::Error> {
        T::read_at(self, buf, offset)
    }
}

pub trait Write: Io {
    fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error>;

    fn flush(&mut self) -> Result<(), Self::Error>;

    fn write_all(&mut self, buf: &[u8]) -> Result<(), Self::Error> {
        match self.try_write_all(buf) {
            Ok(n) if n != buf.len() => panic!("write all: failed to write whole buffer"),
            Ok(_) => Ok(()),
            Err(e) => return Err(e),
        }
    }

    fn try_write_all(&mut self, mut buf: &[u8]) -> Result<usize, Self::Error> {
        let mut count = 0;
        while !buf.is_empty() {
            match self.write(buf) {
                Ok(0) => break,
                Ok(n) => {
                    buf = &buf[n..];
                    count += n;
                }
                Err(e) => return Err(e),
            }
        }
        Ok(count)
    }
}

impl<T: Write> Write for &mut T {
    fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        T::write(self, buf)
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        T::flush(self)
    }
}

pub trait WriteAt: Io {
    fn write_at(&mut self, buf: &[u8], offset: u64) -> Result<usize, Self::Error>;

    fn flush(&mut self) -> Result<(), Self::Error>;

    fn write_all_at(&mut self, buf: &[u8], offset: u64) -> Result<(), Self::Error> {
        match self.try_write_all_at(buf, offset) {
            Ok(n) if n != buf.len() => panic!("write all at: failed to write whole buffer"),
            Ok(_) => Ok(()),
            Err(e) => return Err(e),
        }
    }

    fn try_write_all_at(&mut self, mut buf: &[u8], mut offset: u64) -> Result<usize, Self::Error> {
        let mut count = 0;
        while !buf.is_empty() {
            match self.write_at(buf, offset) {
                Ok(0) => break,
                Ok(n) => {
                    buf = &buf[n..];
                    count += n;
                    offset += n as u64;
                }
                Err(e) => return Err(e),
            }
        }
        Ok(count)
    }
}

impl<T: WriteAt> WriteAt for &mut T {
    fn write_at(&mut self, buf: &[u8], offset: u64) -> Result<usize, Self::Error> {
        T::write_at(self, buf, offset)
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        T::flush(self)
    }
}

pub trait Seek: Io {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64, Self::Error>;

    fn rewind(&mut self) -> Result<(), Self::Error> {
        self.seek(SeekFrom::Start(0))?;
        Ok(())
    }

    fn stream_position(&mut self) -> Result<u64, Self::Error> {
        self.seek(SeekFrom::Current(0))
    }
}

impl<T: Seek> Seek for &mut T {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64, Self::Error> {
        T::seek(self, pos)
    }
}

pub trait DataSync: Io {
    fn sync_all(&self) -> Result<(), Self::Error>;

    fn sync_data(&self) -> Result<(), Self::Error>;
}

impl<T: DataSync> DataSync for &T {
    fn sync_all(&self) -> Result<(), Self::Error> {
        T::sync_all(&self)
    }

    fn sync_data(&self) -> Result<(), Self::Error> {
        T::sync_data(&self)
    }
}

impl<T: DataSync> DataSync for &mut T {
    fn sync_all(&self) -> Result<(), Self::Error> {
        T::sync_all(&self)
    }

    fn sync_data(&self) -> Result<(), Self::Error> {
        T::sync_data(&self)
    }
}
