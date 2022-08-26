use bitflags::bitflags;

use super::{ClockSource, ReadClockFlags, ReadClockListFlags, TimeSpan, FemtoSeconds};

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
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub enum ClockGroup {
    Unknown,
    Monotonic,
    RealTime,
}

impl From<ClockGroup> for u64 {
    fn from(clock: ClockGroup) -> Self {
        match clock {
            ClockGroup::Monotonic => 0,
            ClockGroup::RealTime => 1,
            ClockGroup::Unknown => 2
        }
    }
}

impl From<u64> for ClockGroup {
    fn from(x: u64) -> Self {
        match x {
            0 => ClockGroup::Monotonic,
            1 => ClockGroup::RealTime,
            _ => ClockGroup::Unknown
        }
    }
}

/// ID used internally to read the appropriate clock source.
#[derive(Clone, Copy, Debug)]
#[repr(transparent)]
pub struct ClockID(pub u64);

#[allow(dead_code)]
// abstract representation of a clock source to users
#[derive(Clone, Copy, Debug)]
pub struct Clock {
    pub info: ClockInfo,
    id: ClockID,
    group: ClockGroup
}

impl Clock {
    pub const ZERO: Clock = Clock {
        info: ClockInfo::ZERO,
        id: ClockID(0),
        group: ClockGroup::Unknown
    };

    pub fn new(info: ClockInfo, id: ClockID, group: ClockGroup) -> Clock {
        Self {info, id, group}
    }

    pub fn read(&self) -> TimeSpan {
        match super::sys_read_clock_info(ClockSource::BestMonotonic, ReadClockFlags::empty()) {
            Ok(ci) => ci.current_value(),
            _ => TimeSpan::ZERO
        }
    }
    
    pub fn info(&self) -> ClockInfo {
        self.info
    }

    /// Returns a new instance of a Clock from the specified ClockGroup
    pub fn get(group: ClockGroup) -> Clock {
        let mut clk = [Clock::ZERO];
        if let Ok(filled) = super::sys_read_clock_list(group, &mut clk, 0, ReadClockListFlags::FIRST_KIND) {
            if filled > 0 {
                return clk[0]
            }
        }
        Clock::ZERO
    }
}
