use core::time::Duration;

use super::{FemtoSeconds, Seconds};

#[derive(Clone, Copy, Debug)]
pub struct TimeSpan(pub Seconds, pub FemtoSeconds);

impl TimeSpan {
    pub const ZERO: TimeSpan = TimeSpan(
        Seconds(0),
        FemtoSeconds(0)
    );
}

impl From<TimeSpan> for Duration {
    fn from(t: TimeSpan) -> Self {
        Duration::new(t.0.0, t.1.0 as u32) // TODO: convert femtos to nanos
    }
}
