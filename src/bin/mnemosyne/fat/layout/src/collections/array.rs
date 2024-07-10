use crate::{io::*, *};

// specialization o_O

// impl<const N: usize> Decode for [u8; N] {
//     fn decode<R: Read + Seek + IO>(reader: &mut R, offset: u64) -> Result<Self, R::Error> {
//         let mut out = [0; N];

//         reader.seek(SeekFrom::Start(offset))?;
//         reader.read_exact(&mut out)?;

//         Ok(out)
//     }
// }

// impl<const N: usize> Encode for [u8; N] {
//     fn encode<W: Write + Seek + IO>(&self, writer: &mut W, offset: u64) -> Result<(), W::Error> {
//         writer.seek(SeekFrom::Start(offset))?;
//         writer.write_all(self.as_ref())
//     }
// }

impl<T, const N: usize> Fixed for [T; N] {
    fn size() -> u64 {
        std::mem::size_of::<Self>() as u64 // abi assumption
    }
}

impl<T: Decode + Fixed, const N: usize> Decode for [T; N] {
    fn decode<R: Read + Seek + IO>(reader: &mut R) -> Result<Self, R::Error> {
        array_init::try_array_init(|_| T::decode(reader))
    }
}

impl<T: Encode + Fixed, const N: usize> Encode for [T; N] {
    fn encode<W: Write + Seek + IO>(&self, writer: &mut W) -> Result<(), W::Error> {
        for e in self {
            e.encode(writer)?;
        }

        Ok(())
    }
}

pub struct ArrFrame<'a, R, T, const N: usize> {
    stream: &'a mut R,
    offset: u64,
    pd: PhantomData<T>,
}

impl<'a, R: Read + Seek + IO, T: Fixed + Decode, const N: usize> ArrFrame<'a, R, T, N> {
    pub fn get(&mut self, index: u64) -> Result<T, R::Error> {
        if index >= (N as u64) {
            panic!("index out of bounds: {index} >= {N}");
        }

        self.stream.seek(SeekFrom::Start(self.offset + index * T::size()))?;
        T::decode(self.stream)
    }
}

impl<'a, W: Write + Seek + IO, T: Fixed + Encode, const N: usize> ArrFrame<'a, W, T, N> {
    pub fn set(&mut self, index: u64, elem: T) -> Result<(), W::Error> {
        if index >= (N as u64) {
            panic!("index out of bounds: {index} >= {N}");
        }

        self.stream.seek(SeekFrom::Start(self.offset + index * T::size()))?;
        elem.encode(self.stream)
    }
}

impl<'a, R, T, const N: usize> Frame<R> for ArrFrame<'a, R, T, N> {
    fn stream(&mut self) -> &mut R {
        self.stream
    }

    fn offset(&self) -> u64 {
        self.offset
    }
}

impl<'a, R: 'a + IO, T: 'a + Fixed + Encode + Decode, const N: usize> ApplyLayout<'a, R> for [T; N] {
    type Frame = ArrFrame<'a, R, T, N>;

    fn apply_layout(stream: &'a mut R, offset: u64) -> Result<Self::Frame, R::Error> {
        Ok(ArrFrame {
            stream,
            offset,
            pd: PhantomData,
        })
    }
}
