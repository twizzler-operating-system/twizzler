use x86::io::{inb, outb};

use crate::clock::Nanoseconds;

const CHANNEL_READBACK: u8 = 3 << 6;
const ACCESS_LATCH: u8 = 0;
const ACCESS_LO: u8 = 1 << 4;
const ACCESS_HI: u8 = 2 << 4;
const ACCESS_BOTH: u8 = 3 << 4;
const MODE_ONESHOT: u8 = 1 << 1;
const MODE_RATEGEN: u8 = 2 << 1;
const MODE_SQUAREGEN: u8 = 3 << 1;
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

static mut REAL_FREQ: u64 = 0;
static mut CB: Option<fn(Nanoseconds)> = None;

pub fn timer_interrupt() {
    unsafe {
        CB.unwrap()(1000000000 / REAL_FREQ);
    }
}

pub fn setup_freq(hz: u64, cb: fn(Nanoseconds)) {
    let count = CRYSTAL_HZ / hz;
    assert!(count < 65536);
    unsafe {
        outb(
            PIT_CMD,
            channel(0) | ACCESS_BOTH | MODE_SQUAREGEN | FORMAT_BINARY,
        );
        outb(pit_data(0), (count & 0xff) as u8);
        outb(pit_data(0), ((count >> 8) & 0xff) as u8);
    }
    unsafe {
        REAL_FREQ = CRYSTAL_HZ / count;
        CB = Some(cb);
        logln!(
            "x86-pit: setting up for statclock with freq {} ({} ms)",
            REAL_FREQ,
            (1000 / REAL_FREQ)
        );
    }
}

pub fn wait_ns(ns: u64) {
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
                if tc > 0xffff {
                    0xffff
                } else {
                    tc
                }
            } else {
                0xffff
            };

            outb(pit_data(2), (thiscount & 0xff) as u8);
            outb(pit_data(2), ((thiscount >> 8) & 0xff) as u8);
            outb(0x61, 0);
            outb(0x61, 1);

            let mut readback;
            loop {
                outb(PIT_CMD, channel(2) | ACCESS_LATCH);
                let readlo = inb(pit_data(2));
                let readhi = inb(pit_data(2));
                readback = readlo as u16 | ((readhi as u16) << 8);
                if readback < 64 {
                    break;
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
