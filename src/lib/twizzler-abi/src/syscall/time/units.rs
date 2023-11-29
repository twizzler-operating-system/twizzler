use core::ops::Mul;

use super::TimeSpan;

pub const FEMTOS_PER_SEC: u64 = 1_000_000_000_000_000;
pub const FEMTOS_PER_NANO: u64 = 1_000_000;
pub const NANOS_PER_SEC: u64 = 1_000_000_000;

#[derive(Debug)]
pub enum TimeUnitError {
    ConversionOverflow,
}

#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(transparent)]
pub struct Seconds(pub u64);

impl Mul<Seconds> for u64 {
    type Output = TimeSpan;

    fn mul(self, rhs: Seconds) -> Self::Output {
        TimeSpan::from_secs(self * rhs.0)
    }
}

impl Mul<u64> for Seconds {
    type Output = TimeSpan;

    // apply reflexive property
    fn mul(self, rhs: u64) -> Self::Output {
        rhs * self
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(transparent)]
pub struct MilliSeconds(pub u64);

#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(transparent)]
pub struct MicroSeconds(pub u64);

#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(transparent)]
pub struct NanoSeconds(pub u64);

#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(transparent)]
pub struct PicoSeconds(pub u64);

#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(transparent)]
pub struct FemtoSeconds(pub u64);

impl Mul<FemtoSeconds> for u64 {
    type Output = TimeSpan;

    fn mul(self, rhs: FemtoSeconds) -> Self::Output {
        let t = self as u128 * rhs.0 as u128;
        TimeSpan::new(
            (t / FEMTOS_PER_SEC as u128) as u64,
            (t % FEMTOS_PER_SEC as u128) as u64,
        )
    }
}

impl Mul<u64> for FemtoSeconds {
    type Output = TimeSpan;

    // apply reflexive property
    fn mul(self, rhs: u64) -> Self::Output {
        rhs * self
    }
}

macro_rules! impl_scalar_mul {
    ($unit: ident, $conver: expr) => {
        impl Mul<$unit> for u64 {
            type Output = TimeSpan;

            fn mul(self, rhs: $unit) -> Self::Output {
                let t = self as u128 * rhs.0 as u128;
                let f: FemtoSeconds = $unit((t % $conver as u128) as u64).try_into().unwrap();
                TimeSpan::new((t / $conver as u128) as u64, f.0)
            }
        }

        impl Mul<u64> for $unit {
            type Output = TimeSpan;

            // apply reflexive property
            fn mul(self, rhs: u64) -> Self::Output {
                rhs * self
            }
        }
    };
}

impl_scalar_mul!(NanoSeconds, NANOS_PER_SEC);

macro_rules! impl_unit_conversion {
    ($big: ident, $small: ident, $conver: expr) => {
        impl From<$small> for $big {
            fn from(unit: $small) -> Self {
                $big(unit.0 / $conver)
            }
        }

        // conversion to a smaller unit might fail (overlfow)
        impl TryFrom<$big> for $small {
            type Error = TimeUnitError;
            fn try_from(unit: $big) -> Result<Self, Self::Error> {
                match unit.0.checked_mul($conver) {
                    Some(t) => Ok($small(t)),
                    None => Err(TimeUnitError::ConversionOverflow),
                }
            }
        }
    };
}

impl_unit_conversion!(Seconds, FemtoSeconds, FEMTOS_PER_SEC);
impl_unit_conversion!(NanoSeconds, FemtoSeconds, FEMTOS_PER_NANO);

#[cfg(test)]
mod tests {

    use crate::syscall::{FemtoSeconds, Seconds, TimeSpan, FEMTOS_PER_SEC};

    #[test]
    fn secs_mult() {
        let scalar: u64 = 100;
        let secs: u64 = 5;

        // lhs is Seconds(), rhs is a scalar
        assert_eq!(Seconds(secs) * scalar, TimeSpan::new(secs * scalar, 0));

        // lhs is a scalar, rhs is Seconds()
        assert_eq!(scalar * Seconds(secs), TimeSpan::new(secs * scalar, 0));
    }

    #[test]
    fn femtos_mult() {
        let scalar: u64 = 1234;
        let femtos: u64 = 500;

        // lhs is FemtoSeconds(), rhs is a scalar
        assert_eq!(
            FemtoSeconds(femtos) * scalar,
            TimeSpan::new(0, femtos * scalar)
        );

        // lhs is a scalar, rhs is FemtoSeconds()
        assert_eq!(
            scalar * FemtoSeconds(femtos),
            TimeSpan::new(0, femtos * scalar)
        );
    }

    #[test]
    fn conversion() {
        let femtos = FemtoSeconds(FEMTOS_PER_SEC * 3);
        let mut secs: Seconds = femtos.into();

        assert_eq!(secs, Seconds(3));

        secs = Seconds(3);
        let f: FemtoSeconds = secs
            .try_into()
            .expect("could not convert Seconds to FemtoSeconds");

        assert_eq!(femtos, f);
    }
}
