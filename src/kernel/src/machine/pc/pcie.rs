use alloc::{collections::BTreeMap, format, vec::Vec};

use memoffset::offset_of;
use twizzler_abi::{
    device::{
        bus::pcie::{
            get_bar, PcieBridgeHeader, PcieDeviceHeader, PcieDeviceInfo, PcieFunctionHeader,
            PcieInfo, PcieKactionSpecific,
        },
        BusType, CacheType, DeviceId, DeviceInterrupt, DeviceRepr, NUM_DEVICE_INTERRUPTS,
    },
    kso::{unpack_kaction_int_pri_and_opts, KactionValue},
    object::{ObjID, NULLPAGE_SIZE},
};
use twizzler_rt_abi::error::{ArgumentError, GenericError, ObjectError, ResourceError};
use volatile::map_field;

use crate::{
    arch,
    arch::memory::phys_to_virt,
    device::DeviceRef,
    interrupt::{DynamicInterrupt, WakeInfo},
    memory::PhysAddr,
    mutex::Mutex,
    once::Once,
};

struct PcieKernelInfo {
    seg_dev: DeviceRef,
    segnr: u16,
}

lazy_static::lazy_static! {
    static ref DEVS: Mutex<BTreeMap<ObjID, PcieKernelInfo>> = Mutex::new(BTreeMap::new());
}

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
    let mut cfg = unsafe {
        volatile::VolatileRef::from_mut_ref(
            phys_to_virt(PhysAddr::new(cfgaddr).unwrap())
                .as_mut_ptr::<PcieFunctionHeader>()
                .as_mut()
                .unwrap(),
        )
    };
    let cfg = cfg.as_mut_ptr();
    let mut bars = Vec::new();
    match map_field!(cfg.header_type).read() {
        0 => {
            let mut cfg = unsafe {
                volatile::VolatileRef::from_mut_ref(
                    phys_to_virt(PhysAddr::new(cfgaddr).unwrap())
                        .as_mut_ptr::<PcieDeviceHeader>()
                        .as_mut()
                        .unwrap(),
                )
            };

            let cfg = cfg.as_mut_ptr();

            let mut bar_idx = 0;
            while bar_idx < 6 {
                let bar = get_bar(cfg, bar_idx);
                let bar_data = bar.read();
                bar.write(0xffffffff);
                let sz = (!(bar.read() & 0xfffffff0)).wrapping_add(1);
                bar.write(bar_data);
                let ty = (bar_data >> 1) & 3;
                let pref = (bar_data >> 3) & 1;
                if bar_data & 1 != 0 {
                    bars.push((0, 0, 0));
                } else {
                    if ty == 2 {
                        // TODO: does the second BAR contribute to sz?
                        bar_idx += 1;

                        let bar2 = get_bar(cfg, bar_idx);
                        let bar2_data = bar2.read();
                        bars.push((
                            ((bar2_data as u64 & 0xffffffff) << 32) | bar_data as u64 & 0xfffffff0,
                            sz,
                            pref,
                        ));
                        bars.push((0, 0, 0));
                    } else {
                        bars.push((bar_data as u64 & 0xfffffff0, sz, pref));
                    }
                }
                bar_idx += 1;
            }
        }
        1 => {
            let mut cfg = unsafe {
                volatile::VolatileRef::from_mut_ref(
                    phys_to_virt(PhysAddr::new(cfgaddr).unwrap())
                        .as_mut_ptr::<PcieBridgeHeader>()
                        .as_mut()
                        .unwrap(),
                )
            };
            let cfg = cfg.as_mut_ptr();
            let bar0 = map_field!(cfg.bar0);
            let bar1 = map_field!(cfg.bar1);
            let bar0_backup = bar0.read();
            let bar1_backup = bar1.read();

            bar0.write(0xffffffff);
            let sz = (!(bar0.read() & 0xfffffff0)).wrapping_add(1);
            bar0.write(bar0_backup);

            bar1.write(0xffffffff);
            let sz2 = (!(bar1.read() & 0xfffffff0)).wrapping_add(1);
            bar1.write(bar1_backup);
            let ty = (bar0_backup >> 1) & 3;
            let pref = (bar0_backup >> 3) & 1;
            if ty == 2 {
                // TODO: does the second BAR contribute to sz?
                bars.push((
                    ((bar1_backup as u64 & 0xfffffff0) << 32) | bar0_backup as u64 & 0xfffffff0,
                    sz,
                    pref,
                ));
                bars.push((0, 0, 0));
            } else {
                let pref2 = (bar1_backup >> 3) & 1;
                bars.push((bar0_backup as u64 & 0xfffffff0, sz, pref));
                bars.push((bar1_backup as u64 & 0xfffffff0, sz2, pref2));
            }
        }
        _ => {
            // do nothing -- don't know how to get BARs.
        }
    }
    let info = PcieDeviceInfo {
        seg_nr: seg,
        bus_nr: bus,
        dev_nr: device,
        func_nr: function,
        device_id: map_field!(cfg.device_id).read(),
        vendor_id: map_field!(cfg.vendor_id).read(),
        class: map_field!(cfg.class).read(),
        subclass: map_field!(cfg.subclass).read(),
        progif: map_field!(cfg.progif).read(),
        revision: map_field!(cfg.revision).read(),
    };
    dev.add_info(&info);
    dev.add_mmio(
        PhysAddr::new(cfgaddr).unwrap(),
        PhysAddr::new(cfgaddr + 0x1000).unwrap(),
        CacheType::Uncacheable,
        0xff,
    );

    for bar in bars.iter().enumerate() {
        if bar.1 .0 != 0 {
            dev.add_mmio(
                PhysAddr::new(bar.1 .0).unwrap(),
                PhysAddr::new(bar.1 .0 + bar.1 .1 as u64).unwrap(),
                if bar.1 .2 != 0 {
                    CacheType::WriteThrough
                } else {
                    CacheType::Uncacheable
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

struct InterruptState {
    ints: Vec<DynamicInterrupt>,
}

static INTMAP: Once<Mutex<BTreeMap<ObjID, InterruptState>>> = Once::new();

fn get_int_map() -> &'static Mutex<BTreeMap<ObjID, InterruptState>> {
    INTMAP.call_once(|| Mutex::new(BTreeMap::new()))
}

fn pcie_calculate_int_sync_offset(int: usize) -> Option<usize> {
    if int >= NUM_DEVICE_INTERRUPTS {
        return None;
    }

    Some(
        NULLPAGE_SIZE
            + offset_of!(DeviceRepr, interrupts)
            + core::mem::size_of::<DeviceInterrupt>() * int,
    )
}

fn allocate_interrupt(
    device: DeviceRef,
    arg: u64,
    arg2: u64,
) -> twizzler_rt_abi::Result<KactionValue> {
    let (pri, opts) = unpack_kaction_int_pri_and_opts(arg).ok_or(ArgumentError::InvalidArgument)?;
    let vector =
        crate::interrupt::allocate_interrupt(pri, opts).ok_or(ResourceError::OutOfResources)?;

    let mut maps = get_int_map().lock();
    let state = if let Some(x) = maps.get_mut(&device.objid()) {
        x
    } else {
        maps.insert(device.objid(), InterruptState { ints: Vec::new() });
        maps.get_mut(&device.objid()).unwrap()
    };

    let num = vector.num();
    let offset =
        pcie_calculate_int_sync_offset(arg2 as usize).ok_or(ArgumentError::InvalidArgument)?;
    let wi = WakeInfo::new(device.object(), offset);
    crate::interrupt::set_userspace_interrupt_wakeup(num as u32, wi);
    state.ints.push(vector);

    Ok(KactionValue::U64(num as u64))
}

fn kaction(
    device: DeviceRef,
    cmd: u32,
    arg: u64,
    arg2: u64,
) -> twizzler_rt_abi::Result<KactionValue> {
    let cmd: PcieKactionSpecific = cmd.try_into()?;
    match cmd {
        PcieKactionSpecific::RegisterDevice => {
            let bus = (arg >> 16) & 0xff;
            let dev = (arg >> 8) & 0xff;
            let func = arg & 0xff;
            let seg = DEVS
                .lock()
                .get(&device.objid())
                .ok_or(ObjectError::NoSuchObject)?
                .segnr;
            // logln!("register device {:x} {:x} {:x}", bus, dev, func);

            let dev = register_device(device, seg, bus as u8, dev as u8, func as u8)
                .ok_or(GenericError::Internal)?;
            /*
            let offset = pcie_calculate_int_sync_offset(0).ok_or(KactionError::InvalidArgument)?;
            let wi = WakeInfo::new(dev.object(), offset);
            crate::interrupt::set_userspace_interrupt_wakeup(43, wi);
            arch::set_interrupt(
                43,
                false,
                crate::interrupt::TriggerMode::Edge,
                crate::interrupt::PinPolarity::ActiveHigh,
                crate::interrupt::Destination::Bsp,
            );
            */
            Ok(KactionValue::ObjID(dev.objid()))
        }
        PcieKactionSpecific::AllocateInterrupt => allocate_interrupt(device, arg, arg2),
    }
}

// TODO: we can't just assume every segment has bus 0..255.
fn init_segment(seg: u16, addr: PhysAddr) {
    let dev = crate::device::create_busroot(&format!("pcie_root({})", seg), BusType::Pcie, kaction);
    let end_addr = addr.offset(255usize << 20 | 32 << 15 | 8 << 12).unwrap();
    let info = PcieInfo {
        bus_start: 0,
        bus_end: 0xff,
        seg_nr: seg,
    };
    dev.add_info(&info);
    dev.add_mmio(addr, end_addr, CacheType::Uncacheable, 0);
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
            init_segment(seg, PhysAddr::new(addr).unwrap());
        }
    }
}
