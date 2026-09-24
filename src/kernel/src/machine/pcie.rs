use alloc::{collections::BTreeMap, format, vec::Vec};

use memoffset::offset_of;
use twizzler_abi::{
    device::{
        BusType, CacheType, DeviceId, DeviceInterrupt, DeviceRepr, NUM_DEVICE_INTERRUPTS,
        bus::pcie::{
            PcieBridgeHeader, PcieDeviceHeader, PcieDeviceInfo, PcieFunctionHeader, PcieInfo,
            PcieKactionSpecific, get_bar,
        },
    },
    kso::{KactionValue, unpack_kaction_int_alloc},
    object::{NULLPAGE_SIZE, ObjID},
};
use twizzler_rt_abi::error::{ArgumentError, GenericError, ObjectError, ResourceError};
use volatile::map_field;

use crate::{
    device::DeviceRef,
    interrupt::{Destination, DynamicInterrupt, WakeInfo},
    memory::{PhysAddr, VirtAddr},
    mutex::Mutex,
    once::Once,
    processor::mp::with_each_active_processor,
};

/// Prefetchable BARs are device memory on aarch64; only x86 tolerates a cacheable mapping.
#[cfg(target_arch = "x86_64")]
const PREFETCH_CACHE_TYPE: CacheType = CacheType::WriteThrough;
#[cfg(not(target_arch = "x86_64"))]
const PREFETCH_CACHE_TYPE: CacheType = CacheType::MemoryMappedIO;

#[derive(Clone)]
struct PcieKernelInfo {
    seg_dev: DeviceRef,
    segnr: u16,
    /// The segment's ECAM region, and where the kernel has it mapped.
    ecam: PhysAddr,
    ecam_va: VirtAddr,
    /// MSI doorbell for devices on this segment; 0 where the arch fixes it (x86 LAPIC).
    msi_addr: u64,
}

fn ecam_offset(bus: u8, device: u8, function: u8) -> usize {
    (bus as usize) << 20 | (device as usize) << 15 | (function as usize) << 12
}

static DEVS: Mutex<BTreeMap<ObjID, PcieKernelInfo>> = Mutex::new(BTreeMap::new());

fn register_device(
    parent: DeviceRef,
    kinfo: &PcieKernelInfo,
    bus: u8,
    device: u8,
    function: u8,
) -> Option<DeviceRef> {
    let seg = kinfo.segnr;
    let id = DeviceId::new(
        (seg as u32) << 16 | (bus as u32) << 8 | (device as u32) << 3 | function as u32,
    );
    let cfgaddr = kinfo.ecam.raw() + ecam_offset(bus, device, function) as u64;
    let cfg_va = kinfo
        .ecam_va
        .offset(ecam_offset(bus, device, function))
        .ok()?;
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
            cfg_va.as_mut_ptr::<PcieFunctionHeader>().as_mut().unwrap(),
        )
    };
    let cfg = cfg.as_mut_ptr();
    let mut bars = Vec::new();
    match map_field!(cfg.header_type).read() {
        0 => {
            let mut cfg = unsafe {
                volatile::VolatileRef::from_mut_ref(
                    cfg_va.as_mut_ptr::<PcieDeviceHeader>().as_mut().unwrap(),
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
                    cfg_va.as_mut_ptr::<PcieBridgeHeader>().as_mut().unwrap(),
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
    // Firmware may leave an endpoint undecoded (EDK2 on virt does): drivers need memory-space
    // decoding for BARs and bus mastering for DMA and MSI.
    if map_field!(cfg.header_type).read() & 0x7f == 0 {
        let cmd = map_field!(cfg.command).read();
        map_field!(cfg.command).write(cmd | 0x6);
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
        msi_addr: kinfo.msi_addr,
    };
    dev.add_info(&info);
    dev.add_mmio(
        PhysAddr::new(cfgaddr).unwrap(),
        PhysAddr::new(cfgaddr + 0x1000).unwrap(),
        CacheType::MemoryMappedIO,
        0xff,
    );

    for bar in bars.iter().enumerate() {
        if bar.1.0 != 0 {
            dev.add_mmio(
                PhysAddr::new(bar.1.0).unwrap(),
                PhysAddr::new(bar.1.0 + bar.1.1 as u64).unwrap(),
                if bar.1.2 != 0 {
                    PREFETCH_CACHE_TYPE
                } else {
                    CacheType::MemoryMappedIO
                },
                bar.0 as u64,
            );
        }
    }

    DEVS.lock().insert(
        dev.objid(),
        PcieKernelInfo {
            seg_dev: parent,
            ..kinfo.clone()
        },
    );
    Some(dev)
}

/// Allocated vectors per device, by device interrupt slot.
static INTMAP: Once<Mutex<BTreeMap<ObjID, BTreeMap<usize, DynamicInterrupt>>>> = Once::new();

fn get_int_map() -> &'static Mutex<BTreeMap<ObjID, BTreeMap<usize, DynamicInterrupt>>> {
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
    let (pri, opts, cpu) = unpack_kaction_int_alloc(arg).ok_or(ArgumentError::InvalidArgument)?;
    let inum = arg2 as usize;
    let offset = pcie_calculate_int_sync_offset(inum).ok_or(ArgumentError::InvalidArgument)?;
    let destination = match cpu {
        Some(cpu) => {
            let mut up = false;
            with_each_active_processor(|p| up |= p.id == cpu);
            if !up {
                return Err(ArgumentError::InvalidArgument.into());
            }
            Destination::Single(cpu)
        }
        None => Destination::Bsp,
    };

    let mut maps = get_int_map().lock();
    let ints = maps.entry(device.objid()).or_default();
    if ints.contains_key(&inum) {
        return Err(ResourceError::Busy.into());
    }
    let vector = crate::interrupt::allocate_interrupt(pri, opts, destination)
        .ok_or(ResourceError::OutOfResources)?;
    let num = vector.num();
    let object = device.object();
    object.add_device_interrupt(num as u32, inum, offset);
    crate::interrupt::set_userspace_interrupt_wakeup(num as u32, WakeInfo::new(object, offset));
    ints.insert(inum, vector);

    Ok(KactionValue::U64(num as u64))
}

fn free_interrupt(device: DeviceRef, inum: u64) -> twizzler_rt_abi::Result<KactionValue> {
    let mut maps = get_int_map().lock();
    let vector = maps
        .get_mut(&device.objid())
        .and_then(|ints| ints.remove(&(inum as usize)))
        .ok_or(ArgumentError::InvalidArgument)?;
    // Unbind the slot before releasing waiters, so a sleep that starts now cannot bind to the
    // vector; free the vector last, once nothing refers to it.
    device.object().remove_device_interrupt(inum as usize);
    crate::interrupt::clear_userspace_interrupt_wakeup(vector.num() as u32);
    drop(vector);
    Ok(KactionValue::U64(0))
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
            let info = DEVS
                .lock()
                .get(&device.objid())
                .ok_or(ObjectError::NoSuchObject)?
                .clone();
            // logln!("register device {:x} {:x} {:x}", bus, dev, func);

            let dev = register_device(device, &info, bus as u8, dev as u8, func as u8)
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
        PcieKactionSpecific::FreeInterrupt => free_interrupt(device, arg2),
    }
}

// TODO: we can't just assume every segment has bus 0..255.
/// Register one PCIe segment: `addr` is its ECAM region (256 buses), `ecam_va` the kernel's
/// device-memory mapping of it, `msi_addr` the MSI doorbell handed to drivers (0 on x86).
pub(crate) fn init_segment(seg: u16, addr: PhysAddr, ecam_va: VirtAddr, msi_addr: u64) {
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
            ecam: addr,
            ecam_va,
            msi_addr,
        },
    );
}
