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
#[repr(C)]
/// Information about a given clock source, including precision and current clock value.
pub struct ClockInfo {
    precision: Duration,
    current: Duration,
    flags: ClockFlags,
    source: ClockSource,
}

impl ClockInfo {
    /// Construct a new ClockInfo. You probably want to be getting these from [sys_read_clock_info], though.
    pub fn new(
        current: Duration,
        precision: Duration,
        flags: ClockFlags,
        source: ClockSource,
    ) -> Self {
        Self {
            precision,
            current,
            flags,
            source,
        }
    }

    /// Get the precision of a clock source.
    pub fn precision(&self) -> Duration {
        self.precision
    }

    /// Get the current value of a clock source.
    pub fn current_value(&self) -> Duration {
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
    Monotonic = 0,
    RealTime = 1,
}

impl TryFrom<u64> for ClockSource {
    type Error = ReadClockInfoError;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        Ok(match value {
            0 => Self::Monotonic,
            1 => Self::RealTime,
            _ => return Err(ReadClockInfoError::InvalidArgument),
        })
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
                clock_source as u64,
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
