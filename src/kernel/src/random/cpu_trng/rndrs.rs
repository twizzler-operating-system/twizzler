use core::{array::IntoIter, borrow::BorrowMut};

use arm64::{asm::random::ArmRng, registers};
use rand_core::{impls, RngCore};

use super::EntropySource;

#[derive(Clone, Copy)]
pub struct Rndrs(ArmRng);

#[derive(Clone, Copy)]
pub enum ErrorCode {
    UnsupportedInstruction,
    HardwareFailure,
}

impl ErrorCode {
    #[cfg(not(feature = "std"))]
    const fn as_randcore_code(self) -> core::num::NonZeroU32 {
        /// Arbitrary, off top of head bitmask for error codes that come from rdrand
        const RDRAND_TAG: u32 = rand_core::Error::CUSTOM_START + 0x3D34_7D00;
        unsafe { core::num::NonZeroU32::new_unchecked(RDRAND_TAG + self as u32) }
    }
}

#[cfg(not(feature = "std"))]
impl From<ErrorCode> for rand_core::Error {
    fn from(code: ErrorCode) -> rand_core::Error {
        code.as_randcore_code().into()
    }
}

// doesn't actually work on the chip we are targeting, but it might eventually
// get supported on future ARM chips.
// Untested because I don't have hardware to test it on
// and I don't want to try to emulate that hardware!
impl Rndrs {
    pub fn new() -> Result<Self, ErrorCode> {
        Ok(Rndrs(
            ArmRng::new().ok_or(ErrorCode::UnsupportedInstruction)?,
        ))
    }

    fn maybe_generate_u64(&self) -> Option<u64> {
        // https://github.com/CTSRD-CHERI/cheribsd/blob/bdeff30fb6b1744816f43ed8a3c2f0a133d872c1/sys/dev/random/armv8rng.c#L54-L73
        // todo!();
        for _ in 0..10 {
            if let Some(entropy) = self.0.rndrss() {
                return Some(entropy);
            }
        }
        None
    }

    fn get_8_bytes(self) -> Result<[u8; 8], ErrorCode> {
        Ok(self
            .maybe_generate_u64()
            .ok_or(ErrorCode::HardwareFailure)?
            .to_ne_bytes())
    }

    pub fn try_iter(&self) -> Result<RndrsIterator, ErrorCode> {
        Ok(RndrsIterator {
            rndrs: &self,
            current_entropy: self.get_8_bytes()?.into_iter(),
        })
    }
}

struct RndrsIterator<'a> {
    rndrs: &'a Rndrs,
    current_entropy: IntoIter<u8, 8>,
}

impl Iterator for RndrsIterator<'_> {
    type Item = Result<u8, ErrorCode>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(n) = self.current_entropy.next() {
            return Some(Ok(n));
        }
        match self.rndrs.get_8_bytes() {
            Ok(bytes) => self.current_entropy = bytes.into_iter(),
            Err(e) => return Some(Err(e)),
        }
        self.next()
    }
}

impl EntropySource for Rndrs {
    fn try_fill_entropy(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
        let mut dest_iter = dest.iter_mut();
        let mut rndrs_iter = self.try_iter()?;
        for (d, r) in dest_iter.zip(rndrs_iter) {
            *d = r?
        }
        Ok(())
    }
}

impl rand_core::RngCore for Rndrs {
    /// Do not use as Rndrs is fallable. This can panic!
    fn next_u32(&mut self) -> u32 {
        impls::next_u32_via_fill(self)
    }
    /// Do not use as Rndrs is fallable. This can panic!
    fn next_u64(&mut self) -> u64 {
        impls::next_u64_via_fill(self)
    }
    /// Do not use as Rndrs is fallable. This can panic!
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        self.try_fill_bytes(dest).unwrap()
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
        self.try_fill_entropy(dest)
    }
}
