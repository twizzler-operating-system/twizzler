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
use volatile::{VolatilePtr, map_field};

use crate::{
    device::DeviceRef,
    interrupt::{Destination, DynamicInterrupt, WakeInfo},
    memory::{PhysAddr, VirtAddr},
    mutex::Mutex,
    once::Once,
    processor::mp::with_each_active_processor,
};

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

/// Size a memory BAR, returning (base, size); both zero for I/O BARs. `hi` is the upper dword of
/// a 64-bit BAR. The caller keeps the device from decoding while this runs.
fn probe_bar(lo: VolatilePtr<'_, u32>, hi: Option<VolatilePtr<'_, u32>>) -> (u64, u64) {
    let lo_data = lo.read();
    if lo_data & 1 != 0 {
        return (0, 0);
    }
    lo.write(0xffffffff);
    let lo_mask = lo.read() & 0xfffffff0;
    lo.write(lo_data);
    let base = (lo_data & 0xfffffff0) as u64;
    match hi {
        Some(hi) => {
            let hi_data = hi.read();
            hi.write(0xffffffff);
            let mask = (hi.read() as u64) << 32 | lo_mask as u64;
            hi.write(hi_data);
            ((hi_data as u64) << 32 | base, (!mask).wrapping_add(1))
        }
        None => (base, (!lo_mask).wrapping_add(1) as u64),
    }
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
    match map_field!(cfg.header_type).read() & 0x7f {
        0 => {
            let mut dcfg = unsafe {
                volatile::VolatileRef::from_mut_ref(
                    cfg_va.as_mut_ptr::<PcieDeviceHeader>().as_mut().unwrap(),
                )
            };
            let dcfg = dcfg.as_mut_ptr();

            // Sizing writes all-ones to live BARs; keep the device from decoding meanwhile.
            let cmd = map_field!(cfg.command).read();
            map_field!(cfg.command).write(cmd & !0x3);
            let mut bar_idx = 0;
            while bar_idx < 6 {
                let lo = get_bar(dcfg, bar_idx);
                let is_64 = lo.read() & 0x7 == 0x4 && bar_idx < 5;
                bars.push(probe_bar(lo, is_64.then(|| get_bar(dcfg, bar_idx + 1))));
                if is_64 {
                    bars.push((0, 0));
                    bar_idx += 1;
                }
                bar_idx += 1;
            }
            map_field!(cfg.command).write(cmd);
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
            if bar0.read() & 0x7 == 0x4 {
                bars.push(probe_bar(bar0, Some(bar1)));
                bars.push((0, 0));
            } else {
                bars.push(probe_bar(bar0, None));
                bars.push(probe_bar(bar1, None));
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

    for (idx, &(addr, size)) in bars.iter().enumerate() {
        if addr == 0 || size == 0 {
            continue;
        }
        let (Ok(start), Some(Ok(end))) = (
            PhysAddr::new(addr),
            addr.checked_add(size).map(PhysAddr::new),
        ) else {
            log::warn!(
                "pcie {:x}:{:x}.{:x}: BAR{} at {:#x} (+{:#x}) is not addressable, skipping",
                bus,
                device,
                function,
                idx,
                addr,
                size
            );
            continue;
        };
        dev.add_mmio(
            start,
            end,
            // Prefetchable BARs too: devices update that memory themselves, so reads must not
            // be served from cache.
            CacheType::MemoryMappedIO,
            idx as u64,
        );
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

/// BARs only: config space is registered as 0xff, and a segment's mmio is its ECAM.
fn check_bar(device: &DeviceRef, bar: u64) -> twizzler_rt_abi::Result<()> {
    if bar >= 6
        || DEVS
            .lock()
            .get(&device.objid())
            .is_none_or(|k| k.seg_dev.objid() == device.objid())
    {
        return Err(ArgumentError::InvalidArgument.into());
    }
    Ok(())
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
        PcieKactionSpecific::SetBarCacheType => {
            // Cached types are refused: the device changes this memory underneath any cache.
            let ct = [
                CacheType::WriteCombining,
                CacheType::Uncacheable,
                CacheType::MemoryMappedIO,
            ]
            .into_iter()
            .find(|ct| *ct as u64 == arg2)
            .ok_or(ArgumentError::InvalidArgument)?;
            check_bar(&device, arg)?;
            device.set_mmio_cache_type(arg, ct)?;
            Ok(KactionValue::U64(0))
        }
        PcieKactionSpecific::BarLength => {
            check_bar(&device, arg)?;
            Ok(KactionValue::U64(device.mmio_length(arg)?))
        }
        PcieKactionSpecific::BarChunk => {
            check_bar(&device, arg)?;
            Ok(KactionValue::ObjID(device.mmio_chunk(arg, arg2)?))
        }
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
