use crate::{io::*, *};

macro_rules! impl_num {
    ($($ty:ty, )*) => {
        $(
            impl Fixed for $ty {
                fn size() -> u64 {
                    std::mem::size_of::<Self>() as u64
                }
            }

            impl Decode for $ty {
                fn decode<R: Read + Seek + IO>(reader: &mut R) -> Result<Self, R::Error> {
                    let mut raw = [0; std::mem::size_of::<Self>()];
                    reader.read_exact(&mut raw)?;

                    Ok(<$ty>::from_le_bytes(raw))
                }
            }

            impl Encode for $ty {
                fn encode<W: Write + Seek + IO>(&self, writer: &mut W) -> Result<(), W::Error> {
                    writer.write_all(&mut self.to_le_bytes())
                }
            }
        )*
    };
}

impl_num! { u8, u16, u32, u64, u128, i8, i16, i32, i64, i128, }

macro_rules! impl_tuple {
    ($head:ident, ) => {

    };
    ($head:ident, $($tail:ident, )*) => {
        impl_tuple! { $($tail, )* }

        impl<$head: Fixed, $($tail: Fixed, )*> Fixed for ($head, $($tail, )*) {
            fn size() -> u64 {
                $head::size() $( + $tail::size())*
            }
        }

        impl<$head: Decode + Fixed, $($tail: Decode + Fixed, )*> Decode for ($head, $($tail, )*) {
            fn decode<R: Read + Seek + IO>(reader: &mut R) -> Result<Self, R::Error> {
                Ok((
                    $head::decode(reader)?,
                    $(
                        $tail::decode(reader)?,
                    )*
                ))
            }
        }

        #[allow(non_snake_case)]
        impl<$head: Encode + Fixed, $($tail: Encode + Fixed, )*> Encode for ($head, $($tail, )*) {
            fn encode<W: Write + Seek + IO>(&self, writer: &mut W) -> Result<(), W::Error> {
                let (
                    $head,
                    $(
                        $tail,
                    )*
                ) = self;
            
                $head.encode(writer)?;

                $(
                    $tail.encode(writer)?;
                )*
            
                Ok(())
            }
        }
    };
}

impl_tuple! { A, B, C, D, E, F, G, H, I, J, K, L, M, N, O, P, }
