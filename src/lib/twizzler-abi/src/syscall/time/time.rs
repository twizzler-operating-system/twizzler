use core::time::Duration;

use super::{NanoSeconds, FemtoSeconds, Seconds, FEMTOS_PER_SEC};

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
}

impl From<TimeSpan> for Duration {
    fn from(t: TimeSpan) -> Self {
        let nanos: NanoSeconds = t.1.into();
        Duration::new(t.0.0, nanos.0 as u32) 
    }
}
