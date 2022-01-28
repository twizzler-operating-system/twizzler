//! Raw time handling, provides a way to get a monotonic timer and the system time. You should use
//! the rust standard library's time functions instead of these directly.

use core::time::Duration;

// TODO
/// Return a Duration representing an instant in monotonic time.
pub fn get_monotonic() -> Duration {
    Duration::new(0, 0)
}

// TODO
/// Return a Duration representing the time since the unix epoch.
pub fn get_systemtime() -> Duration {
    Duration::new(0, 0)
}
