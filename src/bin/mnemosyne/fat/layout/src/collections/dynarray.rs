use crate::{io::*, *};

impl<T: Encode + Fixed> Encode for Box<[T]> {
    fn encode<W: Write + Seek + IO>(
        &self,
        writer: &mut W
    ) -> Result<(), W::Error> {
        (self.len() as u64).encode(writer)?;
        for e in self.as_ref() {
            e.encode(writer)?;
        }

        Ok(())
    }
}

impl<T: Decode + Fixed> Decode for Box<[T]> {
    fn decode<R: Read + Seek + IO>(reader: &mut R) -> Result<Self, R::Error> {
        let len = u64::decode(reader)?;

        let mut out = Vec::with_capacity(len as usize);
        for _ in 0..len {
            out.push(T::decode(reader)?);
        }

        Ok(out.into_boxed_slice())
    }
}

pub struct DynArrFrame<'a, S, T> {
    stream: &'a mut S,
    offset: u64,
    len: u64,
    pd: PhantomData<T>,
}

impl<'a, S, T> Frame<S> for DynArrFrame<'a, S, T> {
    fn stream(&mut self) -> &mut S {
        self.stream
    }

    fn offset(&self) -> u64 {
        self.offset
    }
}

impl<'a, S, T> DynArrFrame<'a, S, T> {
    pub fn len(&self) -> u64 {
        self.len
    }
}

impl<'a, R: Read + Seek + IO, T: Fixed + Decode> DynArrFrame<'a, R, T> {
    pub fn get(&mut self, index: u64) -> Result<T, R::Error> {
        if index >= self.len {
            panic!("index out of bounds: {index} >= {}", self.len);
        }

        self.stream.seek(SeekFrom::Start(self.offset + u64::size() + index * T::size()))?;
        T::decode(self.stream)
    }
}

impl<'a, W: Write + Seek + IO, T: Fixed + Encode> DynArrFrame<'a, W, T> {
    pub fn set_len(&mut self, len: u64) -> Result<(), W::Error> {
        self.stream.seek(SeekFrom::Start(self.offset))?;
    
        self.len = len;
        len.encode(self.stream)
    }

    pub fn set(&mut self, index: u64, elem: T) -> Result<(), W::Error> {
        if index >= self.len {
            panic!("index out of bounds: {index} >= {}", self.len);
        }

        self.stream.seek(SeekFrom::Start(self.offset + u64::size() + index * T::size()))?;
        elem.encode(self.stream)
    }
}

impl<'a, S: IO, T: Fixed> FramedDynamic<S> for DynArrFrame<'a, S, T> {
    fn framed_size(&mut self) -> Result<u64, <S as IO>::Error> {
        Ok(u64::size() + self.len * T::size())
    }
}

impl<T: Fixed> SourcedDynamic for Box<[T]> {
    fn sourced_size(&self) -> u64 {
        u64::size() + self.len() as u64 * T::size()
    }
}

impl<'a, R: 'a + Read + Seek + IO, T: 'a + Fixed + Encode + Decode> ApplyLayout<'a, R>
    for Box<[T]>
{
    type Frame = DynArrFrame<'a, R, T>;

    fn apply_layout(stream: &'a mut R, offset: u64) -> Result<Self::Frame, R::Error> {
        stream.seek(SeekFrom::Start(offset))?;
        let len = u64::decode(stream)?;

        Ok(DynArrFrame {
            stream,
            offset,
            len,
            pd: PhantomData,
        })
    }
}
