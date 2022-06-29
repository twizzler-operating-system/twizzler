use alloc::collections::BTreeMap;
use alloc::format;
use alloc::vec::Vec;
use twizzler_abi::device::bus::pcie::{
    PcieBridgeHeader, PcieDeviceHeader, PcieDeviceInfo, PcieFunctionHeader, PcieInfo,
    PcieKactionSpecific,
};
use twizzler_abi::device::{CacheType, DeviceId};
use twizzler_abi::object::ObjID;
use twizzler_abi::{
    device::BusType,
    kso::{KactionError, KactionValue},
};
use x86_64::PhysAddr;

use crate::arch::memory::phys_to_virt;
use crate::mutex::Mutex;
use crate::{arch, device::DeviceRef};

struct PcieKernelInfo {
    seg_dev: DeviceRef,
    segnr: u16,
}

lazy_static::lazy_static! {
    static ref DEVS: Mutex<BTreeMap<ObjID, PcieKernelInfo>> = Mutex::new(BTreeMap::new());
}

#[allow(unaligned_references)]
fn register_device(
    parent: DeviceRef,
    seg: u16,
    bus: u8,
    device: u8,
    function: u8,
) -> Option<DeviceRef> {
    let acpi = arch::acpi::get_acpi_root();
    let cfg = acpi::mcfg::PciConfigRegions::new(acpi).ok()?;
    let id = DeviceId::new(
        (seg as u32) << 16 | (bus as u32) << 8 | (device as u32) << 3 | function as u32,
    );
    let cfgaddr = cfg.physical_address(seg, bus, device, function)?;
    let dev = crate::device::create_device(
        parent.clone(),
        &format!(
            "pcie_device({:x}::{:x}.{:x}.{:x})",
            seg, bus, device, function
        ),
        BusType::Pcie,
        id,
        kaction,
    );
    let cfg: &PcieFunctionHeader = unsafe {
        phys_to_virt(PhysAddr::new(cfgaddr))
            .as_ptr::<PcieFunctionHeader>()
            .as_ref()
            .unwrap()
    };
    let mut bars = Vec::new();
    match cfg.header_type.get() {
        0 => {
            let cfg: &PcieDeviceHeader = unsafe {
                phys_to_virt(PhysAddr::new(cfgaddr))
                    .as_ptr::<PcieDeviceHeader>()
                    .as_ref()
                    .unwrap()
            };
            if cfg.fnheader.status.get() & (1 << 4) != 0 {
                let caps = cfg.cap_ptr.get();
                logln!("caps {:x}", caps);
            }
            let mut bar = 0;
            while bar < 6 {
                let info = cfg.bars[bar].get();
                cfg.bars[bar].set(0xffffffff);
                let sz = (!(cfg.bars[bar].get() & 0xfffffff0)).wrapping_add(1);
                cfg.bars[bar].set(info);
                let ty = (info >> 1) & 3;
                let pref = (info >> 3) & 1;
                if info & 1 != 0 {
                    bars.push((0, 0, 0));
                } else {
                    if ty == 2 {
                        // TODO: does the second BAR contribute to sz?
                        bar += 1;
                        let info2 = cfg.bars[bar].get();
                        bars.push((
                            ((info2 as u64 & 0xffffffff) << 32) | info as u64 & 0xfffffff0,
                            sz,
                            pref,
                        ));
                        bars.push((0, 0, 0));
                    } else {
                        bars.push((info as u64 & 0xfffffff0, sz, pref));
                    }
                }
                bar += 1;
            }
        }
        1 => {
            let cfg: &PcieBridgeHeader = unsafe {
                phys_to_virt(PhysAddr::new(cfgaddr))
                    .as_ptr::<PcieBridgeHeader>()
                    .as_ref()
                    .unwrap()
            };
            let info = cfg.bar[0].get();
            cfg.bar[0].set(0xffffffff);
            let sz = (!(cfg.bar[0].get() & 0xfffffff0)).wrapping_add(1);
            cfg.bar[0].set(info);
            let info2 = cfg.bar[1].get();
            cfg.bar[1].set(0xffffffff);
            let sz2 = (!(cfg.bar[1].get() & 0xfffffff0)).wrapping_add(1);
            cfg.bar[1].set(info2);
            let ty = (info >> 1) & 3;
            let pref = (info >> 3) & 1;
            if ty == 2 {
                // TODO: does the second BAR contribute to sz?
                bars.push((
                    ((info2 as u64 & 0xfffffff0) << 32) | info as u64 & 0xfffffff0,
                    sz,
                    pref,
                ));
                bars.push((0, 0, 0));
            } else {
                let pref2 = (info2 >> 3) & 1;
                bars.push((info as u64 & 0xfffffff0, sz, pref));
                bars.push((info2 as u64 & 0xfffffff0, sz2, pref2));
            }
        }
        _ => {
            logln!("[kernel::machine::pcie] unknown PCIe header type");
        }
    }
    let info = PcieDeviceInfo {
        seg_nr: seg,
        bus_nr: bus,
        dev_nr: device,
        func_nr: function,
        device_id: cfg.device_id.get(),
        vendor_id: cfg.vendor_id.get(),
        class: cfg.class.get(),
        subclass: cfg.subclass.get(),
        progif: cfg.progif.get(),
        revision: cfg.revision.get(),
    };
    dev.add_info(&info);
    dev.add_mmio(
        PhysAddr::new(cfgaddr),
        PhysAddr::new(cfgaddr + 0x1000),
        CacheType::Uncachable,
        0xff,
    );

    for bar in bars.iter().enumerate() {
        logln!(
            "  got bar {} {:x}, {:x}, {}",
            bar.0,
            bar.1 .0,
            bar.1 .1,
            bar.1 .2
        );
        if bar.1 .0 != 0 {
            dev.add_mmio(
                PhysAddr::new(bar.1 .0),
                PhysAddr::new(bar.1 .0 + bar.1 .1 as u64),
                if bar.1 .2 != 0 {
                    CacheType::WriteThrough
                } else {
                    CacheType::Uncachable
                },
                bar.0 as u64,
            );
        }
    }

    DEVS.lock().insert(
        dev.objid(),
        PcieKernelInfo {
            seg_dev: parent,
            segnr: seg,
        },
    );
    Some(dev)
}

fn kaction(device: DeviceRef, cmd: u32, arg: u64) -> Result<KactionValue, KactionError> {
    let cmd: PcieKactionSpecific = cmd.try_into()?;
    match cmd {
        PcieKactionSpecific::RegisterDevice => {
            let bus = (arg >> 16) & 0xff;
            let dev = (arg >> 8) & 0xff;
            let func = arg & 0xff;
            let seg = DEVS
                .lock()
                .get(&device.objid())
                .ok_or(KactionError::NotFound)?
                .segnr;
            // logln!("register device {:x} {:x} {:x}", bus, dev, func);

            let dev = register_device(device, seg, bus as u8, dev as u8, func as u8)
                .ok_or(KactionError::Unknown)?;
            Ok(KactionValue::ObjID(dev.objid()))
        }
        PcieKactionSpecific::AllocateInterrupt => todo!(),
    }
}

// TODO: we can't just assume every segment has bus 0..255.
fn init_segment(seg: u16, addr: PhysAddr) {
    let dev = crate::device::create_busroot(&format!("pcie_root({})", seg), BusType::Pcie, kaction);
    let end_addr = addr + (255u64 << 20 | 32 << 15 | 8 << 12);
    let info = PcieInfo {
        bus_start: 0,
        bus_end: 0xff,
        seg_nr: seg,
    };
    dev.add_info(&info);
    dev.add_mmio(addr, end_addr, CacheType::Uncachable, 0);
    DEVS.lock().insert(
        dev.objid(),
        PcieKernelInfo {
            seg_dev: dev,
            segnr: seg,
        },
    );
}

pub(super) fn init() {
    logln!("[kernel::machine::pcie] init");

    let acpi = arch::acpi::get_acpi_root();

    let cfg =
        acpi::mcfg::PciConfigRegions::new(acpi).expect("failed to get PCIe configuration regions");
    for seg in 0..0xffff {
        let addr = cfg.physical_address(seg, 0, 0, 0);
        if let Some(addr) = addr {
            init_segment(seg, PhysAddr::new(addr));
        }
    }
}
