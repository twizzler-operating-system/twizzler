use core::{fmt, mem::MaybeUninit, time::Duration};
use bitflags::bitflags;

use crate::arch::syscall::raw_syscall;

use super::{convert_codes_to_result, Syscall};
#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(u32)]
/// Possible error values for [sys_read_clock_info].
pub enum ReadClockInfoError {
    /// An unknown error occurred.
    Unknown = 0,
    /// One of the arguments was invalid.   
    InvalidArgument = 1,
}

impl ReadClockInfoError {
    fn as_str(&self) -> &str {
        match self {
            Self::Unknown => "an unknown error occurred",
            Self::InvalidArgument => "invalid argument",
        }
    }
}

impl From<ReadClockInfoError> for u64 {
    fn from(x: ReadClockInfoError) -> Self {
        x as u64
    }
}

impl From<u64> for ReadClockInfoError {
    fn from(x: u64) -> Self {
        match x {
            1 => Self::InvalidArgument,
            _ => Self::Unknown,
        }
    }
}

impl fmt::Display for ReadClockInfoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[cfg(feature = "std")]
impl std::error::Error for ReadClockInfoError {
    fn description(&self) -> &str {
        self.as_str()
    }
}

bitflags! {
    /// Flags about a given clock or clock read.
    pub struct ClockFlags: u32 {
        const MONOTONIC = 1;
    }

    /// Flags to pass to [sys_read_clock_info].
    pub struct ReadClockFlags: u32 {

    }
}

#[derive(Clone, Copy, Debug)]
#[repr(transparent)]
pub struct Seconds(pub u64);

#[derive(Clone, Copy, Debug)]
#[repr(transparent)]
pub struct FemtoSeconds(pub u64);

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

/// Possible clock sources.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub enum ClockSource {
    BestMonotonic,
    BestRealTime,
    ID(ClockID)
}

impl From<u64> for ClockSource {
    fn from(value: u64) -> Self {
        match value {
            0 => Self::BestMonotonic,
            1 => Self::BestRealTime,
            _ => Self::ID(ClockID(value)),
        }
    }
}

impl From<ClockSource> for u64 {
    fn from(source: ClockSource) -> Self {
        match source {
            ClockSource::BestMonotonic => 0,
            ClockSource::BestRealTime => 1,
            ClockSource::ID(clk) => clk.0
        }
    }
}

/// Read information about a give clock, as specified by clock source.
pub fn sys_read_clock_info(
    clock_source: ClockSource,
    flags: ReadClockFlags,
) -> Result<ClockInfo, ReadClockInfoError> {
    let mut clock_info = MaybeUninit::uninit();
    let (code, val) = unsafe {
        raw_syscall(
            Syscall::ReadClockInfo,
            &[
                clock_source.into(),
                &mut clock_info as *mut MaybeUninit<ClockInfo> as usize as u64,
                flags.bits() as u64,
            ],
        )
    };
    convert_codes_to_result(
        code,
        val,
        |c, _| c != 0,
        |_, _| unsafe { clock_info.assume_init() },
        |_, v| v.into(),
    )
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

#[allow(dead_code)]
// abstract representation of a clock source
pub struct Clock {
    info: ClockInfo,
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
    pub fn get(_group: ClockGroup) -> Clock {
        Clock {
            group : ClockGroup::Monotonic,
            id: ClockID(0),
            info: ClockInfo::ZERO,
        }
    }
}

/// Discover a list of clock sources exposed by the kernel.
pub fn sys_read_clock_list(
    _clock: ClockGroup,
    _flags: ReadClockFlags,
) -> Result<Clock, ReadClockInfoError> { // should be a list like Vec
    todo!();
}
