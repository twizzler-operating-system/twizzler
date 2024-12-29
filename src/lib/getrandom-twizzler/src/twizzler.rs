// Copyright 2018 Developers of the Rand project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Implementation for Twizzler
use core::{mem::MaybeUninit, num::NonZeroU32};

// use twizzler_abi::syscall::{sys_get_random, GetRandomError, GetRandomFlags};
use crate::Error;
pub fn getrandom_inner(mut dest: &mut [MaybeUninit<u8>]) -> Result<(), Error> {
    let res = twizzler_rt_abi::random::twz_rt_get_random(
        dest,
        twizzler_rt_abi::random::GetRandomFlags::empty(),
    );
    if res == 0 {
        panic!("failed to fill entropy bytes");
    }
    Ok(())
}
