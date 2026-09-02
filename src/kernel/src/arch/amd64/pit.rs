use core::sync::atomic::{AtomicBool, Ordering};

use x86::io::{inb, outb};

const CHANNEL_READBACK: u8 = 3 << 6;
const ACCESS_LATCH: u8 = 0;
const ACCESS_LO: u8 = 1 << 4;
const ACCESS_HI: u8 = 2 << 4;
const ACCESS_BOTH: u8 = 3 << 4;
const MODE_ONESHOT: u8 = 1 << 1;
const FORMAT_BINARY: u8 = 0;

const PIT_BASE: u16 = 0x40;
const PIT_CMD: u16 = PIT_BASE + 3;

const CRYSTAL_HZ: u64 = 1193182;

fn channel(n: u8) -> u8 {
    n << 6
}

fn pit_data(channel: u16) -> u16 {
    assert!(channel < 3);
    PIT_BASE + channel
}

/// The PIT is a single device with global state -- one counter per channel, plus channel 2's gate
/// on port 0x61 -- but two unrelated boot paths wait on it: AP startup (`apic::trampolines`) and
/// TSC calibration. On SMP they overlap, and a second cpu reloading channel 2 holds the first's
/// readback loop above its exit threshold indefinitely, as well as corrupting the latched lo/hi
/// byte pair. Serialize every access.
static PIT_LOCK: AtomicBool = AtomicBool::new(false);

/// Proof that the caller owns the PIT.
///
/// Not interrupt-safe, and does not need to be: nothing on the interrupt path touches the PIT
/// (`timer_interrupt` only runs the registered callback), and the waits below are boot-path code.
pub struct PitGuard(());

impl Drop for PitGuard {
    fn drop(&mut self) {
        PIT_LOCK.store(false, Ordering::Release);
    }
}

pub fn lock() -> PitGuard {
    while PIT_LOCK
        .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        core::hint::spin_loop();
    }
    PitGuard(())
}

/// Halt channel 0 in case firmware left it in a periodic mode. The kernel never runs the PIT at
/// runtime -- the statclock lives on the per-cpu LAPIC timers -- so any channel-0 output would
/// arrive as a stray vector-32 interrupt. Writing a mode-1 control word stops the counter, and
/// nothing can restart it: mode 1 waits for a gate trigger, and channel 0's gate is tied high.
pub fn quiesce() {
    let _pit = lock();
    unsafe {
        outb(
            PIT_CMD,
            channel(0) | ACCESS_BOTH | MODE_ONESHOT | FORMAT_BINARY,
        );
    }
}

/// Abandon a countdown after this many TSC cycles.
///
/// The readback loop below exits only on `readback < 64`, a ~53us window out of each ~55ms
/// countdown, so any sampling round slower than that window misses it every cycle and the loop
/// never terminates. The bound only has to exceed the longest legitimate wait -- 200ms, TSC
/// calibration -- by enough that a starved vcpu under an emulated, heavily loaded host cannot trip
/// it. ~40s at any plausible tsc rate is far past that, and still turns a dead boot into a line of
/// output.
const READBACK_TIMEOUT_CYCLES: u64 = 100_000_000_000;

pub fn wait_ns(ns: u64) {
    let pit = lock();
    wait_ns_locked(&pit, ns);
}

/// `wait_ns` for a caller that already owns the PIT.
///
/// TSC calibration brackets its wait with `rdtsc` reads, so it has to hold the PIT across both:
/// acquiring inside the wait would charge lock-wait time to the measurement and inflate the
/// frequency estimate.
pub fn wait_ns_locked(_pit: &PitGuard, ns: u64) {
    let tmp = ns as u128 * CRYSTAL_HZ as u128;
    let mut count = (tmp / 1000000000) as u64;

    unsafe {
        outb(
            PIT_CMD,
            channel(2) | ACCESS_BOTH | MODE_ONESHOT | FORMAT_BINARY,
        );
        while count > 64 {
            let thiscount = if 0xffff > count {
                let tc = count + 64;
                if tc > 0xffff { 0xffff } else { tc }
            } else {
                0xffff
            };

            outb(pit_data(2), (thiscount & 0xff) as u8);
            outb(pit_data(2), ((thiscount >> 8) & 0xff) as u8);
            outb(0x61, 0);
            outb(0x61, 1);

            let mut readback;
            let start = x86::time::rdtsc();
            loop {
                outb(PIT_CMD, channel(2) | ACCESS_LATCH);
                let readlo = inb(pit_data(2));
                let readhi = inb(pit_data(2));
                readback = readlo as u16 | ((readhi as u16) << 8);
                if readback < 64 {
                    break;
                }
                if x86::time::rdtsc().wrapping_sub(start) > READBACK_TIMEOUT_CYCLES {
                    // emerglogln!, not logln!: a machine wedged here may well have a cpu stuck
                    // holding the console lock, and `_print_normal` would then block and this
                    // diagnostic would never appear -- which is precisely what happened the first
                    // time it was reached. The emergency path takes no lock.
                    emerglogln!(
                        "[kernel::arch::x86-pit] wait_ns: channel 2 never reached its exit window \
                         (readback {}, loaded {}, {} ticks left); abandoning the wait. Any tsc \
                         calibration from this wait is wrong.",
                        readback,
                        thiscount,
                        count
                    );
                    return;
                }
            }

            let steps = thiscount - readback as u64;
            if steps > count {
                break;
            }
            count -= steps;
        }
    }
}
