use std::marker::PhantomData;

use crate::{io::SeekFrom, ApplyLayout, Decode, Encode, Fixed, Frame, Read, Seek, Write, IO};

pub struct Raw<T>(PhantomData<T>);

impl<T> Fixed for Raw<T> {
    fn size() -> u64 {
        0
    }
}

impl<T> Decode for Raw<T> {
    fn decode<R: Read + Seek + IO>(_: &mut R) -> Result<Self, R::Error> {
        Ok(Raw(PhantomData))
    }
}

impl<T> Encode for Raw<T> {
    fn encode<W: Write + Seek + IO>(&self, _: &mut W) -> Result<(), W::Error> {
        Ok(())
    }
}

impl<'a, T: 'a, R: 'a + IO> ApplyLayout<'a, R> for Raw<T> {
    type Frame = RawFrame<'a, T, R>;

    fn apply_layout(stream: &'a mut R, offset: u64) -> Result<Self::Frame, R::Error> {
        Ok(RawFrame {
            stream,
            offset,
            pd: PhantomData,
        })
    }
}

pub struct RawFrame<'a, T, R> {
    stream: &'a mut R,
    offset: u64,
    pd: PhantomData<T>,
}

impl<'a, T: Decode + Fixed, R: Read + Seek + IO> RawFrame<'a, T, R> {
    pub fn get(&mut self, index: u64) -> Result<T, R::Error> {
        self.stream
            .seek(SeekFrom::Start(self.offset + index * T::size()))?;
        T::decode(self.stream)
    }
}

impl<'a, T: Encode + Fixed, R: Write + Seek + IO> RawFrame<'a, T, R> {
    pub fn set(&mut self, index: u64, elem: &T) -> Result<(), R::Error> {
        self.stream
            .seek(SeekFrom::Current((index * T::size()) as i64))?;
        elem.encode(self.stream)
    }
}

impl<'a, T, R> Frame<R> for RawFrame<'a, T, R> {
    fn stream(&mut self) -> &mut R {
        self.stream
    }

    fn offset(&self) -> u64 {
        self.offset
    }
}

#[derive(Debug)]
pub struct RawBytes;

impl Fixed for RawBytes {
    fn size() -> u64 {
        0
    }
}

impl Decode for RawBytes {
    fn decode<R: Read + Seek + IO>(_: &mut R) -> Result<Self, R::Error> {
        Ok(RawBytes)
    }
}

impl Encode for RawBytes {
    fn encode<W: Write + Seek + IO>(&self, _: &mut W) -> Result<(), W::Error> {
        Ok(())
    }
}

impl<'a, R: 'a + IO> ApplyLayout<'a, R> for RawBytes {
    type Frame = RawBytesFrame<'a, R>;

    fn apply_layout(stream: &'a mut R, offset: u64) -> Result<Self::Frame, R::Error> {
        Ok(RawBytesFrame { stream, offset })
    }
}

pub struct RawBytesFrame<'a, R> {
    stream: &'a mut R,
    offset: u64,
}

impl<'a, R: Seek + IO> RawBytesFrame<'a, R> {
    pub fn direct_stream(&mut self) -> Result<&mut R, R::Error> {
        self.stream.seek(SeekFrom::Start(self.offset))?;
        Ok(&mut self.stream)
    }
}

impl<'a, R> Frame<R> for RawBytesFrame<'a, R> {
    fn stream(&mut self) -> &mut R {
        self.stream
    }

    fn offset(&self) -> u64 {
        self.offset
    }
}
