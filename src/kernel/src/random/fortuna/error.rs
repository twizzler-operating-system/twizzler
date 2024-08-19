#[derive(Debug, Clone)]
pub enum TooBigOrSmall {
    TooBig,
    TooSmall,
}

#[derive(Debug, Clone)]
pub enum Error {
    Unseeded,
    InvalidDataSize(TooBigOrSmall),
    InvalidPoolSize(TooBigOrSmall::TooBig),
}

impl Error {
    const fn as_randcore_code(self) -> core::num::NonZeroU32 {
        /// Arbitrary, off top of head bitmask for error codes that come from rdrand
        const FORTUNA_TAG: u32 = rand_core::Error::CUSTOM_START + 0x0D34_7D00;
        core::num::NonZeroU32::new(RDRAND_TAG + self as u32)
    }
}

impl From<Error> for rand_core::Error {
    fn from(value: Error) -> Self {
        value.as_randcore_code().into()
    }
}
