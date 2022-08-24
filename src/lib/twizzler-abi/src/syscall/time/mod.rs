mod clock;
mod time;
mod units;

pub use clock::*;
pub use time::*;
pub use units::*;

use core::{fmt, mem::MaybeUninit};
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

#[derive(Debug, Copy, Clone)]
#[repr(u32)]
/// Possible error values for [sys_read_clock_list].
pub enum ReadClockListError {
    /// An unknown error occurred.
    Unknown = 0,
    /// One of the arguments was invalid.   
    InvalidArgument = 1,
}

impl ReadClockListError {
    fn as_str(&self) -> &str {
        match self {
            Self::Unknown => "an unknown error occurred",
            Self::InvalidArgument => "invalid argument",
        }
    }
}

impl From<ReadClockListError> for u64 {
    fn from(e: ReadClockListError) -> Self {
        e as u64
    }
}

impl From<u64> for ReadClockListError {
    fn from(x: u64) -> Self {
        match x {
            1 => Self::InvalidArgument,
            _ => Self::Unknown,
        }
    }
}

impl fmt::Display for ReadClockListError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[cfg(feature = "std")]
impl std::error::Error for ReadClockListError {
    fn description(&self) -> &str {
        self.as_str()
    }
}

bitflags! {
    /// Flags to pass to [sys_read_clock_info].
    pub struct ReadClockFlags: u32 {

    }

    pub struct ReadClockListFlags: u32 {
        const ALL_CLOCKS = 1 << 0;
        const ONLY_KIND = 1 << 1;
        const FIRST_KIND = 1 << 2;
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

/// Discover a list of clock sources exposed by the kernel.
pub fn sys_read_clock_list(
    clock: ClockGroup,
    clocks: &mut [Clock],
    start: u64,
    flags: ReadClockListFlags,
) -> Result<usize, ReadClockListError> {
    let (code, val) = unsafe {
        raw_syscall(
            Syscall::ReadClockList,
            &[
                clock.into(),
                clocks.as_mut_ptr() as u64,
                clocks.len() as u64,
                start,
                flags.bits() as u64,
            ],
        )
    };
    convert_codes_to_result(
        code,
        val,
        |c, _| c != 0,
        |_, v| v as usize,
        |_, v| v.into(),
    )
}
