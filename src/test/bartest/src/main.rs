//! Exercises `Device::set_bar_cache_type` on the NVMe controller's register BAR.
//!
//! For emulated devices only: the BAR is switched to write-combining while its driver is live,
//! which real register memory would not tolerate. Registers are only read.
//!
//! `bartest big` instead exercises `Device::map_bar_range` on the memory BAR of an ivshmem
//! device, which is ordinary RAM of any size:
//! `-object memory-backend-ram,id=big,size=4G -device ivshmem-plain,memdev=big`.
//! `python3 src/test/bartest/big.py` (from the repo root) instead backs the BAR with a host file
//! holding [host_marker]s, and afterwards checks that the guest's [guest_marker]s reached it: at
//! the start, middle and last page of every chunk, the last page being where an object's meta page
//! would be.

use std::{
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::Instant,
};

use twizzler_abi::{
    device::{BusType, CacheType},
    kso::{KactionCmd, KactionFlags},
};
use twizzler_driver::{
    bus::pcie::{BarRange, PcieDeviceInfo, PcieKactionSpecific},
    device::{Device, MmioObject},
    get_bustree_root,
};

const BAR: usize = 0;
const ROUNDS: usize = 200;

fn find(want: impl Fn(&PcieDeviceInfo) -> bool) -> Option<(Device, Device)> {
    for seg in get_bustree_root().children() {
        if seg.bus_type() != BusType::Pcie {
            continue;
        }
        for dev in seg.children() {
            let Some(info) = (unsafe { dev.get_info::<PcieDeviceInfo>(0) }) else {
                continue;
            };
            let info = info.get_data();
            if want(info) {
                return Some((seg, dev));
            }
        }
    }
    None
}

/// CAP, VS and CC: constant while the controller runs.
fn regs(mmio: &MmioObject) -> (u64, u32, u32) {
    unsafe {
        (
            mmio.get_mmio_offset::<u64>(0).as_ptr().read(),
            mmio.get_mmio_offset::<u32>(8).as_ptr().read(),
            mmio.get_mmio_offset::<u32>(0x14).as_ptr().read(),
        )
    }
}

const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;

fn probes(len: u64) -> impl Iterator<Item = u64> {
    (0..len)
        .step_by(GIB as usize)
        .flat_map(|c| [c, c + GIB / 2 + 8, c + GIB - 4096, c + GIB - 8])
}

fn host_marker(off: u64) -> u64 {
    (0x4854 << 48) | off
}

fn guest_marker(off: u64) -> u64 {
    (0x4753 << 48) | off
}

fn big() {
    const BAR: usize = 2;
    let (_, dev) = find(|i| i.vendor_id == 0x1af4 && i.device_id == 0x1110).expect("no ivshmem");
    let len = dev.bar_length(BAR).unwrap();
    let mmio = dev.find_mmio_bar(BAR).unwrap();
    println!(
        "bartest: ivshmem BAR{} is {} MiB, its mmio object holds {} MiB",
        BAR,
        len / MIB,
        mmio.get_info().length / MIB
    );
    assert!(len >= 4 * GIB);
    assert!(dev.bar_length(6).is_err());

    let map = |offset: u64, len: u64| {
        let start = Instant::now();
        let r = dev.map_bar_range(BAR, offset, len as usize);
        println!(
            "bartest: map +{} MiB, {} KiB: {} in {:?}",
            offset / MIB,
            len >> 10,
            if r.is_ok() { "ok" } else { "refused" },
            start.elapsed()
        );
        r
    };
    let word = |r: &BarRange, off: u64| unsafe { r.as_ptr().add(off as usize).cast::<u64>() };

    // Before anything is written: all zero for a fresh RAM backend, or what big.py put there.
    let whole = map(0, len).unwrap();
    let found: Vec<_> = probes(len)
        .map(|off| (off, unsafe { word(&whole, off).read_volatile() }))
        .collect();
    drop(whole);
    let host = found[0].1 != 0;
    for (off, v) in found {
        let want = if host { host_marker(off) } else { 0 };
        assert_eq!(v, want, "BAR offset {:#x} before any write", off);
    }
    println!(
        "bartest: {} at every probe",
        if host { "host markers" } else { "zero" }
    );

    // The start of the BAR, against the mmio object.
    let head = map(0, MIB).unwrap();
    unsafe {
        mmio.get_mmio_offset_mut::<u64>(0x1000)
            .as_mut_ptr()
            .write(0x1111);
        assert_eq!(word(&head, 0x1000).read_volatile(), 0x1111);
    }

    // A pair's slots survive its members being mapped again, alone and in an overlapping pair.
    let pair = map(GIB - 4 * MIB, 8 * MIB).unwrap();
    let alone = map(GIB, 4 * MIB).unwrap();
    let overlap = map(2 * GIB - 4 * MIB, 8 * MIB).unwrap();
    unsafe {
        word(&pair, 4 * MIB).write_volatile(0x5555);
        assert_eq!(word(&alone, 0).read_volatile(), 0x5555);
    }
    drop((alone, overlap));
    unsafe {
        word(&pair, 4 * MIB + 8).write_volatile(0x6666);
        assert_eq!(word(&pair, 4 * MIB + 8).read_volatile(), 0x6666);
    }
    drop(pair);

    // A range across each chunk boundary, written through, read through one-chunk mappings.
    for boundary in (GIB..len).step_by(GIB as usize) {
        let across = map(boundary - 4 * MIB, 8 * MIB).unwrap();
        let below = map(boundary - 4 * MIB, 4 * MIB).unwrap();
        let above = map(boundary, 4 * MIB).unwrap();
        unsafe {
            for off in (0..8 * MIB).step_by(4096) {
                word(&across, off).write_volatile(boundary + off);
            }
            for off in (0..4 * MIB).step_by(4096) {
                assert_eq!(word(&below, off).read_volatile(), boundary + off);
                assert_eq!(word(&above, off).read_volatile(), boundary + 4 * MIB + off);
            }
        }
    }

    // The largest range that always fits, ending 2 MiB short of the BAR's end.
    let wide = map(len - GIB - 2 * MIB, GIB).unwrap();
    let tail = map(len - 4 * MIB, 4 * MIB).unwrap();
    unsafe {
        word(&wide, GIB - 8).write_volatile(0x2222);
        assert_eq!(word(&tail, 2 * MIB - 8).read_volatile(), 0x2222);
        word(&tail, 4 * MIB - 8).write_volatile(0x3333);
    }

    // Three and four chunks, written across each boundary they span and read through one-chunk
    // mappings; the first mapped is dropped first, and the other must keep its slots.
    let check = |r: &BarRange, base: u64, tag: u64| {
        let end = base + r.len() as u64;
        for b in (GIB..len)
            .step_by(GIB as usize)
            .filter(|b| *b > base && *b < end)
        {
            unsafe {
                word(r, b - base - 8).write_volatile(tag + b - 8);
                word(r, b - base).write_volatile(tag + b);
            }
            let below = map(b - 4 * MIB, 4 * MIB).unwrap();
            let above = map(b, 4 * MIB).unwrap();
            unsafe {
                assert_eq!(word(&below, 4 * MIB - 8).read_volatile(), tag + b - 8);
                assert_eq!(word(&above, 0).read_volatile(), tag + b);
            }
        }
    };
    let three = map(GIB, 3 * GIB).unwrap();
    let whole = map(0, len).unwrap();
    check(&three, GIB, 0x7 << 40);
    check(&whole, 0, 0x8 << 40);
    drop(three);
    check(&whole, 0, 0x9 << 40);
    drop(whole);
    let last = map(len - GIB, 4 * MIB).unwrap();
    assert_eq!(
        unsafe { word(&last, 0).read_volatile() },
        (0x9 << 40) + len - GIB
    );
    drop(last);

    assert!(map(len - MIB, 2 * MIB).is_err(), "past the end");
    assert!(map(0, 0).is_err(), "empty");

    for ct in [CacheType::WriteCombining, CacheType::MemoryMappedIO] {
        let start = Instant::now();
        dev.set_bar_cache_type(BAR, ct).unwrap();
        println!("bartest: BAR{} -> {:?} in {:?}", BAR, ct, start.elapsed());
        unsafe {
            assert_eq!(word(&head, 0x1000).read_volatile(), 0x1111);
            assert_eq!(word(&tail, 4 * MIB - 8).read_volatile(), 0x3333);
            word(&wide, 0).write_volatile(0x4444 + ct as u64);
            core::arch::x86_64::_mm_sfence();
            assert_eq!(word(&wide, 0).read_volatile(), 0x4444 + ct as u64);
        }
    }
    drop((head, wide, tail));

    // Written write-combining through one mapping of the whole BAR, read uncached through
    // one-chunk mappings and through a remapping of the whole BAR; big.py checks the host file.
    let whole = map(0, len).unwrap();
    dev.set_bar_cache_type(BAR, CacheType::WriteCombining)
        .unwrap();
    for off in probes(len) {
        unsafe { word(&whole, off).write_volatile(guest_marker(off)) };
    }
    unsafe { core::arch::x86_64::_mm_sfence() };
    dev.set_bar_cache_type(BAR, CacheType::MemoryMappedIO)
        .unwrap();
    drop(whole);
    for off in probes(len) {
        let chunk = map(off & !(GIB - 1), GIB).unwrap();
        assert_eq!(
            unsafe { word(&chunk, off % GIB).read_volatile() },
            guest_marker(off)
        );
    }
    let whole = map(0, len).unwrap();
    for off in probes(len) {
        assert_eq!(
            unsafe { word(&whole, off).read_volatile() },
            guest_marker(off)
        );
    }
    println!("bartest: guest markers written");
    println!("bartest: PASS");
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("big") {
        return big();
    }
    let (seg, dev) = find(|i| i.class == 1 && i.subclass == 8).expect("no NVMe controller");
    let mmio = dev.find_mmio_bar(BAR).expect("no BAR0");
    let orig = mmio.get_info().cache_type;
    let len = mmio.get_info().length;
    let want = regs(&mmio);
    println!(
        "bartest: NVMe BAR{} length {:#x} cache {:?} CAP {:#x} VS {:#x} CC {:#x}",
        BAR, len, orig, want.0, want.1, want.2
    );
    assert_eq!(orig, CacheType::MemoryMappedIO);
    assert_ne!(want.1, 0);

    for ct in [
        CacheType::WriteCombining,
        CacheType::WriteCombining,
        CacheType::Uncacheable,
        CacheType::MemoryMappedIO,
        CacheType::MemoryMappedIO,
    ] {
        let start = Instant::now();
        dev.set_bar_cache_type(BAR, ct).unwrap();
        let took = start.elapsed();
        assert_eq!(mmio.get_info().cache_type, ct);
        assert_eq!(mmio.get_info().length, len);
        assert_eq!(mmio.get_info().info, BAR as u64);
        assert_eq!(dev.find_mmio_bar(BAR).unwrap().get_info().cache_type, ct);
        assert_eq!(regs(&mmio), want);
        println!(
            "bartest: BAR{} -> {:?} in {:?}, registers unchanged",
            BAR, ct, took
        );
    }

    let missing = (0..6).find(|b| dev.find_mmio_bar(*b).is_none()).unwrap();
    let raw = |d: &Device, bar: u64, ct: u64| {
        d.kaction(
            KactionCmd::Specific(PcieKactionSpecific::SetBarCacheType.into()),
            bar,
            KactionFlags::empty(),
            ct,
        )
    };
    let wc = CacheType::WriteCombining;
    for (what, r) in [
        ("config space", dev.set_bar_cache_type(0xff, wc)),
        ("missing BAR", dev.set_bar_cache_type(missing, wc)),
        ("BAR index 6", dev.set_bar_cache_type(6, wc)),
        (
            "WriteBack",
            dev.set_bar_cache_type(BAR, CacheType::WriteBack),
        ),
        (
            "WriteThrough",
            dev.set_bar_cache_type(BAR, CacheType::WriteThrough),
        ),
        ("cache type 99", raw(&dev, BAR as u64, 99).map(|_| ())),
        ("segment ECAM", seg.set_bar_cache_type(0, wc)),
    ] {
        println!("bartest: {} -> {:?}", what, r);
        assert!(r.is_err(), "{} was accepted", what);
    }
    assert_eq!(mmio.get_info().cache_type, orig);
    assert_eq!(
        seg.get_mmio(0).unwrap().get_info().cache_type,
        CacheType::Uncacheable
    );
    assert_eq!(regs(&mmio), want);

    let stop = AtomicBool::new(false);
    let reads = AtomicU64::new(0);
    std::thread::scope(|s| {
        s.spawn(|| {
            while !stop.load(Ordering::Relaxed) {
                assert_eq!(regs(&mmio), want);
                reads.fetch_add(1, Ordering::Relaxed);
            }
        });
        while reads.load(Ordering::Relaxed) == 0 {
            std::thread::yield_now();
        }
        for _ in 0..ROUNDS {
            dev.set_bar_cache_type(BAR, wc).unwrap();
            assert_eq!(regs(&mmio), want);
            dev.set_bar_cache_type(BAR, orig).unwrap();
            assert_eq!(regs(&mmio), want);
        }
        stop.store(true, Ordering::Relaxed);
    });
    assert_eq!(mmio.get_info().cache_type, orig);
    println!(
        "bartest: {} switches under {} concurrent reads, no fault",
        2 * ROUNDS,
        reads.load(Ordering::Relaxed)
    );
    println!("bartest: PASS");
}
