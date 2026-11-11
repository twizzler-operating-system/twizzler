//! Exercises `Device::set_bar_cache_type` on the NVMe controller's register BAR.
//!
//! For emulated devices only: the BAR is switched to write-combining while its driver is live,
//! which real register memory would not tolerate. Registers are only read.

use std::{
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::Instant,
};

use twizzler_abi::{
    device::{BusType, CacheType},
    kso::{KactionCmd, KactionFlags},
};
use twizzler_driver::{
    bus::pcie::{PcieDeviceInfo, PcieKactionSpecific},
    device::{Device, MmioObject},
    get_bustree_root,
};

const BAR: usize = 0;
const ROUNDS: usize = 200;

fn find_nvme() -> Option<(Device, Device)> {
    for seg in get_bustree_root().children() {
        if seg.bus_type() != BusType::Pcie {
            continue;
        }
        for dev in seg.children() {
            let Some(info) = (unsafe { dev.get_info::<PcieDeviceInfo>(0) }) else {
                continue;
            };
            let info = info.get_data();
            if info.class == 1 && info.subclass == 8 {
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

fn main() {
    let (seg, dev) = find_nvme().expect("no NVMe controller");
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
