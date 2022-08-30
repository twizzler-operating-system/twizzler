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
pub enum ClockKind {
    Unknown,
    Monotonic,
    RealTime,
}

impl From<ClockKind> for u64 {
    fn from(clock: ClockKind) -> Self {
        match clock {
            ClockKind::Monotonic => 0,
            ClockKind::RealTime => 1,
            ClockKind::Unknown => 2
        }
    }
}

impl From<u64> for ClockKind {
    fn from(x: u64) -> Self {
        match x {
            0 => ClockKind::Monotonic,
            1 => ClockKind::RealTime,
            _ => ClockKind::Unknown
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
    kind: ClockKind
}

impl Clock {
    pub const ZERO: Clock = Clock {
        info: ClockInfo::ZERO,
        id: ClockID(0),
        kind: ClockKind::Unknown
    };

    pub fn new(info: ClockInfo, id: ClockID, kind: ClockKind) -> Clock {
        Self {info, id, kind}
    }

    pub fn read(&self) -> TimeSpan {
        match super::sys_read_clock_info(ClockSource::ID(self.id), ReadClockFlags::empty()) {
            Ok(ci) => ci.current_value(),
            _ => TimeSpan::ZERO
        }
    }
    
    pub fn info(&self) -> ClockInfo {
        self.info
    }

    /// Returns a new instance of a Clock from the specified ClockKind
    pub fn get(kind: ClockKind) -> Clock {
        let mut clk = [Clock::ZERO];
        if let Ok(filled) = super::sys_read_clock_list(kind, &mut clk, 0, ReadClockListFlags::FIRST_KIND) {
            if filled > 0 {
                return clk[0]
            }
        }
        Clock::ZERO
    }

    pub fn set(&mut self, info: ClockInfo, id: ClockID, kind: ClockKind) {
        self.info = info;
        self.id = id;
        self.kind = kind;
    }
}
