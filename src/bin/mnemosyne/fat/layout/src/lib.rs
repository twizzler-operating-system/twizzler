extern crate twizzler_abi;

use std::marker::PhantomData;

pub mod collections;
pub mod io;
pub mod primitive;

pub use io::{Read, Seek, Write, IO};
pub use layout_derive::layout;

pub trait ApplyLayout<'a, R: IO>: Encode + Decode {
    type Frame: Frame<R> + 'a;

    fn apply_layout(stream: &'a mut R, offset: u64) -> Result<Self::Frame, R::Error>;
}

pub trait Frame<S> {
    fn stream(&mut self) -> &mut S;
    fn offset(&self) -> u64;
}

pub trait Decode: Sized {
    fn decode<R: Read + Seek + IO>(reader: &mut R) -> Result<Self, R::Error>;
}

pub trait Encode {
    fn encode<W: Write + Seek + IO>(&self, writer: &mut W) -> Result<(), W::Error>;
}

pub trait Fixed {
    fn size() -> u64;
}

pub trait FramedDynamic<R: IO> {
    fn framed_size(&mut self) -> Result<u64, R::Error>;
}

pub trait SourcedDynamic {
    fn sourced_size(&self) -> u64;
}
