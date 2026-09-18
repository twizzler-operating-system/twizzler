//! Twizzler backend over the runtime's entropy call.
//!
//! Vendored (not the fork branch) because this copy is also built by rust's bootstrap for the
//! hosted cargo, whose tool dependency allowlist rejects `twizzler-rt-abi`; declare the runtime
//! entry point directly instead, as upstream's own backends do for their libcs.
use core::mem::MaybeUninit;

use crate::Error;

extern "C" {
    fn twz_rt_get_random(buf: *mut u8, len: usize, flags: u32) -> usize;
}

pub fn getrandom_inner(dest: &mut [MaybeUninit<u8>]) -> Result<(), Error> {
    let n = unsafe { twz_rt_get_random(dest.as_mut_ptr().cast(), dest.len(), 0) };
    if n == dest.len() { Ok(()) } else { Err(Error::UNEXPECTED) }
}
