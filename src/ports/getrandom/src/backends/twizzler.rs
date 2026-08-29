//! Implementation for Twizzler, over the runtime ABI's entropy call.
use core::mem::MaybeUninit;

use crate::Error;

pub use crate::util::{inner_u32, inner_u64};

#[inline]
pub fn fill_inner(dest: &mut [MaybeUninit<u8>]) -> Result<(), Error> {
    // twz_rt_get_random returns the number of bytes filled. A short read means the runtime
    // could not satisfy the request, which for a blocking call is not a partial-fill retry
    // case -- it is a failure.
    let len = twizzler_rt_abi::random::twz_rt_get_random(
        dest,
        twizzler_rt_abi::random::GetRandomFlags::empty(),
    );
    if len == dest.len() {
        Ok(())
    } else {
        Err(Error::UNEXPECTED)
    }
}
