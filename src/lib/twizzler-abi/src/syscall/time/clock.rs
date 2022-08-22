use bitflags::bitflags;

use super::{TimeSpan, FemtoSeconds};

bitflags! {
    /// Flags about a given clock or clock read.
    pub struct ClockFlags: u32 {
        const MONOTONIC = 1;
    }
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
/// Information about a given clock source, including precision and current clock value.
pub struct ClockInfo {
    current: TimeSpan,
    precision: FemtoSeconds,
    resolution: FemtoSeconds,
    flags: ClockFlags,
}

impl ClockInfo {
    pub const ZERO: ClockInfo = ClockInfo::new(
        TimeSpan::ZERO,
        FemtoSeconds(0),
        FemtoSeconds(0),
        ClockFlags::MONOTONIC
    );

    /// Construct a new ClockInfo. You probably want to be getting these from [sys_read_clock_info], though.
    pub const fn new(
        current: TimeSpan,
        precision: FemtoSeconds,
        resolution: FemtoSeconds,
        flags: ClockFlags,
    ) -> Self {
        Self {
            current,
            precision,
            resolution,
            flags,
        }
    }

    /// Get the precision of a clock source.
    pub fn precision(&self) -> FemtoSeconds {
        self.precision
    }

    /// Get the resolution of a clock source.
    pub fn resolution(&self) -> FemtoSeconds {
        self.resolution
    }

    /// Get the current value of a clock source.
    pub fn current_value(&self) -> TimeSpan {
        self.current
    }

    /// Is the clock source monotonic?
    pub fn is_monotonic(&self) -> bool {
        self.flags.contains(ClockFlags::MONOTONIC)
    }
}


/// Different kinds of clocks exposed by the kernel.
#[derive(Clone, Copy)]
#[repr(C)]
pub enum ClockGroup {
    Unknown,
    Monotonic,
    RealTime,
}

/// ID used internally to read the appropriate clock source.
#[derive(Clone, Copy, Debug)]
#[repr(transparent)]
pub struct ClockID(pub u64);

// #[allow(dead_code)]
// abstract representation of a clock source to users
pub struct Clock {
    pub info: ClockInfo,
    id: ClockID,
    group: ClockGroup
}

impl Clock {
    pub fn new(info: ClockInfo, id: ClockID, group: ClockGroup) -> Clock {
        Self {info, id, group}
    }

    pub fn read(&self) -> TimeSpan {
        TimeSpan::ZERO
    }
    
    pub fn info(&self) -> ClockInfo {
        self.info
    }

    /// Returns a new instance of a Clock from the specified ClockGroup
    pub fn get(group: ClockGroup) -> Clock {
        Clock {
            group : group,
            id: ClockID(0),
            info: ClockInfo::ZERO,
        }
    }
}
