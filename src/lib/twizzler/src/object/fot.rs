use std::{ptr::addr_of, sync::atomic::AtomicU32};

use super::{BaseType, Object, RawObject};
use crate::ptr::InvPtr;

bitflags::bitflags! {
    #[repr(C)]
    pub struct FotFlags : u32 {
        const RESERVED = 1;
        const ACTIVE = 2;
        const RESOLVER = 4;
    }
}

pub type ResolverFn = extern "C" fn(ResolveRequest) -> Result<FotResolve, FotError>;

pub enum FotError {
    InvalidIndex,
    InvalidFotEntry,
    InactiveFotEntry,
}

pub struct ResolveRequest {}

pub struct FotResolve {}

#[repr(C)]
pub struct FotEntry {
    pub values: [u64; 2],
    pub resolver: InvPtr<ResolverFn>,
    pub flags: AtomicU32,
}

impl<Base: BaseType> Object<Base> {
    /*
    pub(crate) fn resolve_pointer(
        &self,
        idx: usize,
        pointer_value: u64,
    ) -> Result<RawResolvedPointer, FotError> {
        let fote = self.fote_ptr(idx).ok_or(FotError::InvalidIndex)?;
        let flags: *const AtomicU32 = addr_of!(fote.flags);
        let flags =
            FotFlags::from_bits(unsafe { (&*flags).load(std::sync::atomic::Ordering::SeqCst) })
                .ok_or(FotError::InvalidFotEntry)?;

        if !flags.contains(FotFlags::RESERVED) || !flags.contains(FotFlags::ACTIVE) {
            return Err(FotError::InactiveFotEntry);
        }

        let fote: &FotEntry = unsafe { &*fote };

        if flags.contains(FotFlags::RESOLVER) {
            let resolver_split = twizzler_abi::object::split_invariant_pointer(fote.resolver.raw());

            let resolver = self.resolve_pointer(resolver_split.0, resolver_split.1)?;
        }
    }
    */
}
