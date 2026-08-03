//! CMOS real-time clock (RTC) support.
//!
//! Reads the battery-backed wall-clock time out of CMOS at boot and uses it to seed a
//! `ClockHardware` source for [`twizzler_abi::syscall::ClockKind::RealTime`]. We only read the
//! RTC once: continuously polling CMOS is slow and racy, so afterwards we track elapsed time
//! using the TSC and add it to the epoch we captured at boot.

use twizzler_abi::syscall::{ClockFlags, ClockInfo, FEMTOS_PER_SEC, FemtoSeconds, TimeSpan};
use x86::io::{inb, outb};

use crate::time::{ClockHardware, Ticks, bench_clock, register_realtime_clock};

// source: https://wiki.osdev.org/CMOS
const CMOS_ADDRESS: u16 = 0x70;
const CMOS_DATA: u16 = 0x71;

const REG_SECONDS: u8 = 0x00;
const REG_MINUTES: u8 = 0x02;
const REG_HOURS: u8 = 0x04;
const REG_DAY: u8 = 0x07;
const REG_MONTH: u8 = 0x08;
const REG_YEAR: u8 = 0x09;
const REG_STATUS_A: u8 = 0x0A;
const REG_STATUS_B: u8 = 0x0B;

unsafe fn read_cmos_register(reg: u8) -> u8 {
    unsafe {
        outb(CMOS_ADDRESS, reg);
        inb(CMOS_DATA)
    }
}

unsafe fn update_in_progress() -> bool {
    unsafe { read_cmos_register(REG_STATUS_A) & 0x80 != 0 }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct RawRtcTime {
    second: u8,
    minute: u8,
    hour: u8,
    day: u8,
    month: u8,
    year: u8,
}

unsafe fn read_rtc_once() -> RawRtcTime {
    unsafe {
        RawRtcTime {
            second: read_cmos_register(REG_SECONDS),
            minute: read_cmos_register(REG_MINUTES),
            hour: read_cmos_register(REG_HOURS),
            day: read_cmos_register(REG_DAY),
            month: read_cmos_register(REG_MONTH),
            year: read_cmos_register(REG_YEAR),
        }
    }
}

/// Read CMOS, retrying until we get two consecutive identical readings taken outside of an
/// update cycle. This is the standard technique for avoiding torn reads on the CMOS RTC (see
/// the OSDev wiki's CMOS RTC page), since there's no way to atomically snapshot all the RTC
/// registers.
fn read_rtc_stable() -> RawRtcTime {
    loop {
        while unsafe { update_in_progress() } {}
        let first = unsafe { read_rtc_once() };
        while unsafe { update_in_progress() } {}
        let second = unsafe { read_rtc_once() };
        if first == second {
            return first;
        }
    }
}

fn bcd_to_bin(v: u8) -> u8 {
    (v & 0x0F) + (v >> 4) * 10
}

/// Days since the Unix epoch (1970-01-01) for a given civil date. Integer-only implementation
/// of Howard Hinnant's `days_from_civil` algorithm: http://howardhinnant.github.io/date_algorithms.html
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Read the CMOS RTC and convert it to a Unix timestamp.
///
/// Assumes the RTC is set to UTC, which is QEMU's default (and typical for bare metal x86
/// unless something has explicitly configured it for local time). CMOS only stores a two-digit
/// year, and there's no portable way to read a century register, so we assume 20xx.
fn read_rtc_unix_time() -> u64 {
    let status_b = unsafe { read_cmos_register(REG_STATUS_B) };
    let is_binary = status_b & 0x04 != 0;
    let is_24hr = status_b & 0x02 != 0;

    let raw = read_rtc_stable();

    let (second, minute, mut hour, day, month, year) = if is_binary {
        (
            raw.second, raw.minute, raw.hour, raw.day, raw.month, raw.year,
        )
    } else {
        (
            bcd_to_bin(raw.second),
            bcd_to_bin(raw.minute),
            bcd_to_bin(raw.hour & 0x7F) | (raw.hour & 0x80),
            bcd_to_bin(raw.day),
            bcd_to_bin(raw.month),
            bcd_to_bin(raw.year),
        )
    };

    if !is_24hr {
        let pm = hour & 0x80 != 0;
        hour &= 0x7F;
        if pm && hour != 12 {
            hour += 12;
        } else if !pm && hour == 12 {
            hour = 0;
        }
    }

    let full_year = 2000 + year as i64;
    let days = days_from_civil(full_year, month as u32, day as u32);
    (days * 86400 + hour as i64 * 3600 + minute as i64 * 60 + second as i64) as u64
}

/// A real-time clock backed by a one-time CMOS RTC read at boot, kept up to date afterwards by
/// tracking elapsed TSC ticks rather than re-reading CMOS (which is slow and racy).
struct RtcRealTimeClock {
    epoch_offset_secs: u64,
    tsc_baseline: u64,
    tsc_rate: FemtoSeconds,
}

impl ClockHardware for RtcRealTimeClock {
    fn read(&self) -> Ticks {
        let now = unsafe { x86::time::rdtsc() };
        let elapsed_ticks = now.saturating_sub(self.tsc_baseline);
        let elapsed_secs = (elapsed_ticks * self.tsc_rate).0.0;
        Ticks {
            value: self.epoch_offset_secs + elapsed_secs,
            rate: FemtoSeconds(FEMTOS_PER_SEC),
        }
    }

    fn info(&self) -> ClockInfo {
        ClockInfo::new(
            TimeSpan::ZERO,
            FemtoSeconds(FEMTOS_PER_SEC),
            FemtoSeconds(FEMTOS_PER_SEC),
            FemtoSeconds(FEMTOS_PER_SEC),
            ClockFlags::empty(),
        )
    }
}

/// Read the CMOS RTC and register it as the system's real-time clock source. Must run after
/// the TSC has already been registered (we borrow its calibrated tick rate) and before any
/// other code tries to read [`twizzler_abi::syscall::ClockKind::RealTime`].
pub fn init_realtime_clock() {
    let epoch_offset_secs = read_rtc_unix_time();
    let tsc_baseline = unsafe { x86::time::rdtsc() };
    let tsc_rate = bench_clock()
        .map(|c| c.info().resolution())
        .unwrap_or(FemtoSeconds(0));

    logln!(
        "[kernel::machine::pc::rtc] wall-clock time from CMOS RTC: {} (unix seconds)",
        epoch_offset_secs
    );

    register_realtime_clock(RtcRealTimeClock {
        epoch_offset_secs,
        tsc_baseline,
        tsc_rate,
    });
}
