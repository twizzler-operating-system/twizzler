use core::{time::Duration, ops::Sub};

use super::{NanoSeconds, FemtoSeconds, Seconds, FEMTOS_PER_SEC, NANOS_PER_SEC};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimeSpan(pub Seconds, pub FemtoSeconds);

impl TimeSpan {
    pub const ZERO: TimeSpan = TimeSpan(
        Seconds(0),
        FemtoSeconds(0)
    );

    pub const fn new(secs: u64, femtos: u64) -> TimeSpan {
        TimeSpan(
            Seconds(secs),
            FemtoSeconds(femtos)
        )
    }

    pub const fn from_secs(secs: u64) -> TimeSpan {
        TimeSpan(
            Seconds(secs),
            FemtoSeconds(0)
        )
    }

    pub const fn from_femtos(femtos: u64) -> TimeSpan {
        TimeSpan(
            Seconds(femtos / FEMTOS_PER_SEC),
            FemtoSeconds(femtos % FEMTOS_PER_SEC)
        )
    }

    pub fn as_nanos(&self) -> u128 {
        let nanos: NanoSeconds = self.1.into();
        self.0.0 as u128 * NANOS_PER_SEC as u128 + nanos.0 as u128
    }

    pub const fn checked_sub(&self, other: TimeSpan) -> Option<TimeSpan> {
        if self.0.0 >= other.0.0 {
            let mut secs = self.0.0 - other.0.0;
            let nanos = if self.1.0 >= other.1.0 {
                self.1.0 - other.1.0
            } else {
                secs -= 1;
                self.1.0 + FEMTOS_PER_SEC - other.1.0
            };
            return Some(TimeSpan(
                Seconds(secs),
                FemtoSeconds(nanos)
            ))
        }
        // rhs was bigger than lhs
        None
    }
}

impl From<TimeSpan> for Duration {
    fn from(t: TimeSpan) -> Self {
        let nanos: NanoSeconds = t.1.into();
        Duration::new(t.0.0, nanos.0 as u32) 
    }
}

impl Sub for TimeSpan {
    type Output = Self;

    fn sub(self, other: Self) -> Self::Output {
        self.checked_sub(other).expect("overflow occured when subtracting TimeSpan")
    }
}
