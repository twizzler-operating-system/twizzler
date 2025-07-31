use twizzler_abi::meta::MetaExtTag;
use twizzler_derive::Invariant;

use super::MetaExtension;
use crate::ptr::InvPtr;

#[derive(Invariant)]
#[repr(C)]
pub struct DirectIo {
    pub offset: u64,
    pub len: u64,
}

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct IoFlags: u32 {
        const NONBLOCKING = 0x1;
    }
}

pub type PreadFn = extern "C-unwind" fn(
    id: ObjID,
    offset: u64,
    buf: &mut [u8],
    flags: IoFlags,
) -> crate::Result<u64>;

pub type PwriteFn =
    extern "C-unwind" fn(id: ObjID, offset: u64, buf: &[u8], flags: IoFlags) -> crate::Result<u64>;

pub type FlushFn = extern "C-unwind" fn(id: ObjID) -> crate::Result<()>;

pub type CtrlFn =
    extern "C-unwind" fn(id: ObjID, cmd: u64, arg: u64, flags: IoFlags) -> crate::Result<u64>;

#[derive(Invariant)]
#[repr(C)]
pub struct TwzIo {
    pub direct_io: Option<DirectIo>,
    pub pread: InvPtr<PreadFn>,
    pub pwrite: InvPtr<PwriteFn>,
    pub flush: InvPtr<FlushFn>,
    pub ctrl: InvPtr<CtrlFn>,
}

const fn make_tag(x: u64) -> MetaExtTag {
    MetaExtTag(x)
}

impl MetaExtension for TwzIo {
    type Data = TwzIo;
    const TAG: MetaExtTag = make_tag(1024);
}

impl TwzIo {
    fn fallback_pread(&self, offset: u64, buf: &mut [u8], flags: IoFlags) -> Result<usize> {
        todo!()
    }

    pub fn pread(&self, offset: u64, buf: &mut [u8], flags: IoFlags) -> Result<usize> {
        if self.pread.is_null() {
            if let Some(dio) = self.direct_io.as_ref() {
                return self.fallback_pread(
                    offset + dio.offset,
                    &mut buf[0..dio.len.min(buf.len())],
                    flags,
                );
            }
        }
        let f: PreadFn = self.pread.resolve().unwrap();
        let id = todo!();
        f(id, offset, buf, flags).map(|r| r as usize)
    }
}
