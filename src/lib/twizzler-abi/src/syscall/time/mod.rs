mod clock;
mod timedefs;
mod units;

use core::mem::MaybeUninit;

use bitflags::bitflags;
pub use clock::*;
use num_enum::{FromPrimitive, IntoPrimitive};
pub use timedefs::*;
pub use units::*;

use super::{convert_codes_to_result, Syscall};
use crate::arch::syscall::raw_syscall;

#[derive(
    Debug,
    Copy,
    Clone,
    PartialEq,
    PartialOrd,
    Ord,
    Eq,
    Hash,
    IntoPrimitive,
    FromPrimitive,
    thiserror::Error,
)]
#[repr(u64)]
/// Possible error returns for [sys_read_clock_info].
pub enum ReadClockInfoError {
    /// An unknown error occurred.
    #[num_enum(default)]
    #[error("unknown error")]
    Unknown = 0,
    /// One of the arguments was invalid.
    #[error("invalid argument")]
    InvalidArgument = 1,
}

impl core::error::Error for ReadClockInfoError {}

#[derive(
    Debug,
    Copy,
    Clone,
    PartialEq,
    PartialOrd,
    Ord,
    Eq,
    Hash,
    IntoPrimitive,
    FromPrimitive,
    thiserror::Error,
)]
#[repr(u64)]
/// Possible error returns for [sys_read_clock_info].
pub enum ReadClockListError {
    /// An unknown error occurred.
    #[num_enum(default)]
    #[error("unknown error")]
    Unknown = 0,
    /// One of the arguments was invalid.
    #[error("invalid argument")]
    InvalidArgument = 1,
}

impl core::error::Error for ReadClockListError {}

bitflags! {
    /// Flags to pass to [`sys_read_clock_info`].
    pub struct ReadClockFlags: u32 {

    }

    /// Flags to pass to [`sys_read_clock_list`].
    #[derive(PartialEq, Eq)]
    pub struct ReadClockListFlags: u32 {
        /// Fill the buffer with all clocks from the clock list, for every `ClockKind`.
        const ALL_CLOCKS = 1 << 0;
        /// Fill the buffer with only clocks from a given `ClockKind` list.
        const ONLY_KIND = 1 << 1;
        /// Fill the buffer with the first clock in the `ClockKind` list.
        const FIRST_KIND = 1 << 2;
    }
}

/// Possible clock sources.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub enum ClockSource {
    BestMonotonic,
    BestRealTime,
    ID(ClockID),
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
            ClockSource::ID(clk) => clk.0,
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
///
/// This returns a list of clocks stored in `clocks` and the number of
/// entries filled. By default, one clock from every type of clock
/// exposed ([`ClockKind`]), is returned. All information in [`ClockInfo`]
/// except the current value is also returned. For each type of clock with more
/// than one clock source, the first one is returned. Users can get a list of
/// all clocks, and thus all clock sources, for a particular type by
/// specifying the [`ClockKind`] and setting the appropriate flag.
///
/// Users are expected to provide a slice, `clocks`, to be filled by the kernel.
/// `start` indicates what offset into the list of clocks the kernel should fill
/// the `clocks` buffer from. When there are no more clocks to read from a given
/// `start` offset, then the value 0 is returned.
///
/// # Examples
///
/// ```no_run
/// let mut clocks = [Clock::ZERO; 4];
/// let result = sys_read_clock_list(
///     ClockKind::Monotonic,
///     &mut clocks,
///     0,
///     ReadClockListFlags::FIRST_KIND,
/// );
/// if let Some(filled) = result {
///     if filled > 0 {
///         println!("time now: {}", clock[0].read().as_nanos());
///     }
/// }
/// ```
pub fn sys_read_clock_list(
    clock: ClockKind,
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
    convert_codes_to_result(code, val, |c, _| c != 0, |_, v| v as usize, |_, v| v.into())
}
