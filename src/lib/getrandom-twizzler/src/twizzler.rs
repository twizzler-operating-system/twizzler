// Copyright 2018 Developers of the Rand project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Implementation for Twizzler
use core::{mem::MaybeUninit, num::NonZeroU32};

use twizzler_abi::syscall::{sys_get_random, GetRandomError, GetRandomFlags};

use crate::Error;

pub fn getrandom_inner(dest: &mut [MaybeUninit<u8>]) -> Result<(), Error> {
    sys_get_random(dest, GetRandomFlags::empty()).map_err(|e| {

        let err_text = 
        match e {
        GetRandomError::Unseeded => 
            "Unexpected error: get_random called with blocking so it should never return if generator is unseeded."
        ,
        GetRandomError::InvalidArgument => 
            "Unexpected error: all arguments should be correct."
        };
        panic!("{}", err_text)
    }).map(|_|{})
}
