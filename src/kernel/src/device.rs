use alloc::{borrow::ToOwned, collections::BTreeMap, string::String, sync::Arc, vec::Vec};
use core::mem::size_of;

use memoffset::offset_of;
use twizzler_abi::{
    device::{
        BusType, CacheType, DeviceId, DeviceInterrupt, DeviceRepr, DeviceType, MMIO_OFFSET,
        MmioInfo, SubObjectType,
    },
    kso::{
        KactionCmd, KactionGenericCmd, KactionValue, KsoHdr, PinFlags,
        pack_kaction_pin_token_and_len, unpack_kaction_pin_start_and_len,
    },
    object::{MAX_SIZE, NULLPAGE_SIZE, ObjID},
    syscall::PinnedPage,
};
use twizzler_rt_abi::{
    Result,
    error::{ArgumentError, ObjectError, TwzError},
};

use crate::{
    interrupt::WakeInfo,
    memory::PhysAddr,
    mutex::Mutex,
    obj::{LookupFlags, Object, ObjectRef, lookup_object, register_object},
    once::OnceWait,
    syscall::create_user_slice,
};

pub struct DeviceInner {
    sub_objects: Vec<(SubObjectType, ObjectRef)>,
    mmio: Vec<Mmio>,
    children: Vec<DeviceRef>,
}

/// An mmio region; the copy of this in its sub-object is user-writable.
struct Mmio {
    info: u64,
    start: PhysAddr,
    end: PhysAddr,
    ct: CacheType,
    /// Covers the first `length` bytes, after an [MmioInfo].
    obj: ObjectRef,
    length: usize,
    /// Objects covering [MAX_SIZE] each from their first byte, by index; made when asked for.
    chunks: BTreeMap<u64, ObjectRef>,
}

pub struct Device {
    inner: Mutex<DeviceInner>,
    kaction: fn(DeviceRef, cmd: u32, arg: u64, arg2: u64) -> Result<KactionValue>,
    bus_type: BusType,
    dev_type: DeviceType,
    id: ObjID,
    name: String,
}

pub type DeviceRef = Arc<Device>;

static DEVICES: OnceWait<Mutex<BTreeMap<ObjID, DeviceRef>>> = OnceWait::new();

fn get_device_map() -> &'static Mutex<BTreeMap<ObjID, DeviceRef>> {
    DEVICES.call_once(|| Mutex::new(BTreeMap::new()))
}

struct KsoManager {
    root: ObjectRef,
    device_roots: Mutex<Vec<DeviceRef>>,
}

impl KsoManager {
    fn get_child_id(&self, n: usize) -> Option<ObjID> {
        self.device_roots.lock().get(n).map(|x| x.objid())
    }
}

static KSO_MANAGER: OnceWait<KsoManager> = OnceWait::new();

fn get_kso_manager() -> &'static KsoManager {
    KSO_MANAGER.call_once(|| {
        let root = crate::obj::Object::new_kernel();
        crate::obj::register_object(root.clone());
        KsoManager {
            root,
            device_roots: Mutex::new(Vec::new()),
        }
    })
}

pub fn kaction(cmd: KactionCmd, id: Option<ObjID>, arg: u64, arg2: u64) -> Result<KactionValue> {
    match cmd {
        KactionCmd::Generic(cmd) => match cmd {
            KactionGenericCmd::ReleasePin => {
                let id = id.ok_or(ArgumentError::InvalidArgument)?;
                let obj =
                    lookup_object(id, LookupFlags::empty()).ok_or(ObjectError::NoSuchObject)?;
                let pin = arg as u32;
                obj.release_pin(pin);
                Ok(KactionValue::U64(0))
            }
            KactionGenericCmd::PinPages(flags) => {
                let flags = PinFlags::from_bits(flags).ok_or(ArgumentError::InvalidArgument)?;
                let id = id.ok_or(ArgumentError::InvalidArgument)?;
                let obj =
                    lookup_object(id, LookupFlags::empty()).ok_or(ObjectError::NoSuchObject)?;

                let (start, len) =
                    unpack_kaction_pin_start_and_len(arg2).ok_or(ArgumentError::InvalidArgument)?;

                let slice = unsafe { create_user_slice::<PinnedPage>(arg, len as u64) }
                    .ok_or(ArgumentError::InvalidArgument)?;

                let (pins, token) = obj.pin(
                    (start as usize)
                        .try_into()
                        .map_err(|_| ArgumentError::InvalidArgument)?,
                    slice.len(),
                    flags,
                )?;
                let len: u32 = core::cmp::min(pins.len(), len as usize).try_into().unwrap();

                for i in 0..(len as usize) {
                    slice[i] = pins[i];
                }

                let retval = pack_kaction_pin_token_and_len(token, len as usize).unwrap();
                Ok(KactionValue::U64(retval))
            }
            KactionGenericCmd::GetKsoRoot => {
                let ksom = get_kso_manager();
                Ok(KactionValue::ObjID(ksom.root.id()))
            }
            KactionGenericCmd::GetChild(n) => {
                let ksom = get_kso_manager();
                if let Some(id) = id {
                    if id == ksom.root.id() {
                        ksom.get_child_id(n as usize)
                            .map_or(Err(ArgumentError::BadHandle.into()), |x| {
                                Ok(KactionValue::ObjID(x))
                            })
                    } else {
                        let dm = get_device_map().lock();
                        if let Some(dev) = dm.get(&id) {
                            dev.get_child_id(n as usize)
                                .map_or(Err(ArgumentError::BadHandle.into()), |x| {
                                    Ok(KactionValue::ObjID(x))
                                })
                        } else {
                            Err(ArgumentError::InvalidArgument.into())
                        }
                    }
                } else {
                    Err(ArgumentError::InvalidArgument.into())
                }
            }
            KactionGenericCmd::GetSubObject(t, n) => {
                let ksom = get_kso_manager();
                if let Some(id) = id {
                    if id == ksom.root.id() {
                        Err(ArgumentError::InvalidArgument.into())
                    } else {
                        let dm = get_device_map().lock();
                        if let Some(dev) = dm.get(&id) {
                            dev.get_subobj_id(t, n as usize)
                                .map_or(Err(ArgumentError::BadHandle.into()), |x| {
                                    Ok(KactionValue::ObjID(x))
                                })
                        } else {
                            Err(ArgumentError::InvalidArgument.into())
                        }
                    }
                } else {
                    Err(ArgumentError::InvalidArgument.into())
                }
            }
            KactionGenericCmd::MapPhys(obj_start_hi) => {
                let obj_start_lo = arg2 >> 32;
                let len = (arg2 & 0xFFFFFFFF) as usize;
                let obj_start = ((obj_start_hi as u64) << 32) | obj_start_lo;
                let phys_start = arg;
                /*
                logln!(
                    "{:x} {:x} mapping physical pages for object {:?}, {:x}, {:x}, {:x}",
                    arg,
                    arg2,
                    id,
                    obj_start,
                    phys_start,
                    len
                );
                */
                let Some(id) = id else {
                    return Err(TwzError::INVALID_ARGUMENT);
                };
                let obj = match lookup_object(id, LookupFlags::empty()) {
                    crate::obj::LookupResult::Found(o) => o,
                    _ => {
                        let obj = Object::new_kernel_with_id(id);
                        register_object(obj.clone());
                        obj
                    }
                };
                let start = PhysAddr::new(phys_start & !(1 << 63)).unwrap();
                let end = PhysAddr::new((phys_start & !(1 << 63)) + len as u64).unwrap();
                let ct = if phys_start & (1 << 63) != 0 {
                    CacheType::Uncacheable
                } else {
                    CacheType::WriteBack
                };
                obj.map_phys(obj_start as usize, start, end, ct)?;
                Ok(KactionValue::U64(0))
            }
        },
        KactionCmd::Specific(cmd) => id.map_or(Err(ArgumentError::InvalidArgument.into()), |id| {
            let dev = {
                let dm = get_device_map().lock();
                dm.get(&id).cloned()
            };
            dev.map_or(Err(ArgumentError::BadHandle.into()), |dev| {
                (dev.kaction)(dev.clone(), cmd, arg, arg2)
            })
        }),
    }
}

pub fn create_busroot(
    name: &str,
    bt: BusType,
    kaction: fn(DeviceRef, cmd: u32, arg: u64, arg2: u64) -> Result<KactionValue>,
) -> DeviceRef {
    let obj = crate::obj::Object::new_kernel();
    crate::obj::register_object(obj.clone());
    let device = Arc::new(Device {
        inner: Mutex::new(DeviceInner {
            sub_objects: Vec::new(),
            mmio: Vec::new(),
            children: Vec::new(),
        }),
        kaction,
        bus_type: bt,
        dev_type: DeviceType::Bus,
        id: obj.id(),
        name: name.to_owned(),
    });
    let info = DeviceRepr::new(KsoHdr::new(name), DeviceType::Bus, bt, DeviceId::new(0));
    obj.write_base(&info).unwrap();
    get_device_map().lock().insert(obj.id(), device.clone());
    let ksom = get_kso_manager();
    ksom.device_roots.lock().push(device.clone());
    device
}

pub fn create_device(
    parent: DeviceRef,
    name: &str,
    bt: BusType,
    id: DeviceId,
    kaction: fn(DeviceRef, cmd: u32, arg: u64, arg: u64) -> Result<KactionValue>,
) -> DeviceRef {
    let obj = crate::obj::Object::new_kernel();
    crate::obj::register_object(obj.clone());
    let device = Arc::new(Device {
        inner: Mutex::new(DeviceInner {
            sub_objects: Vec::new(),
            mmio: Vec::new(),
            children: Vec::new(),
        }),
        kaction,
        bus_type: bt,
        dev_type: DeviceType::Device,
        id: obj.id(),
        name: name.to_owned(),
    });
    let info = DeviceRepr::new(KsoHdr::new(name), DeviceType::Device, bt, id);
    obj.write_base(&info).unwrap();
    get_device_map().lock().insert(obj.id(), device.clone());
    parent.inner.lock().children.push(device.clone());
    device
}

impl Device {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn get_interrupt_wakeinfo(&self, num: usize) -> WakeInfo {
        let obj = lookup_object(self.id, LookupFlags::empty()).unwrap();
        WakeInfo::new(
            obj,
            NULLPAGE_SIZE
                + offset_of!(DeviceRepr, interrupts)
                + size_of::<DeviceInterrupt>() * num
                + offset_of!(DeviceInterrupt, sync),
        )
    }

    pub fn add_info<T>(&self, info: &T) {
        let obj = crate::obj::Object::new_kernel();
        obj.write_base(info).unwrap();
        crate::obj::register_object(obj.clone());
        self.inner
            .lock()
            .sub_objects
            .push((SubObjectType::Info, obj));
    }

    pub fn add_mmio(&self, start: PhysAddr, end: PhysAddr, ct: CacheType, info: u64) {
        let obj = crate::obj::Object::new_kernel();
        log::debug!(
            "add_mmio: {:?} - {:?} ({} MB)",
            start,
            end,
            (end - start) / (1024 * 1024)
        );
        let length = obj.map_phys(MMIO_OFFSET, start, end, ct).unwrap() as u64;
        let mmio_info = MmioInfo {
            length,
            cache_type: ct,
            info,
        };
        obj.write_base(&mmio_info).unwrap();
        crate::obj::register_object(obj.clone());
        let mut inner = self.inner.lock();
        inner.mmio.push(Mmio {
            info,
            start,
            end,
            ct,
            obj: obj.clone(),
            length: length as usize,
            chunks: BTreeMap::new(),
        });
        inner.sub_objects.push((SubObjectType::Mmio, obj));
    }

    /// Change the cache type of the mmio region whose [MmioInfo::info] is `info`.
    pub fn set_mmio_cache_type(&self, info: u64, ct: CacheType) -> Result<()> {
        let mut inner = self.inner.lock();
        let m = inner
            .mmio
            .iter_mut()
            .find(|m| m.info == info)
            .ok_or(ArgumentError::InvalidArgument)?;
        m.obj.set_cache_type(MMIO_OFFSET, m.length, ct)?;
        for (idx, chunk) in &m.chunks {
            let len = ((m.end - m.start) - *idx as usize * MAX_SIZE).min(MAX_SIZE);
            chunk.set_cache_type(0, len, ct)?;
        }
        m.ct = ct;
        m.obj.write_base(&MmioInfo {
            length: m.length as u64,
            cache_type: ct,
            info,
        })
    }

    /// Length of the whole mmio region `info`, which its sub-object may not cover.
    pub fn mmio_length(&self, info: u64) -> Result<u64> {
        let inner = self.inner.lock();
        let m = inner
            .mmio
            .iter()
            .find(|m| m.info == info)
            .ok_or(ArgumentError::InvalidArgument)?;
        Ok((m.end - m.start) as u64)
    }

    /// The object holding bytes `idx * MAX_SIZE..` of mmio region `info` from its first byte on,
    /// with no [MmioInfo]: mapped in adjacent slots, consecutive chunks are contiguous.
    pub fn mmio_chunk(&self, info: u64, idx: u64) -> Result<ObjID> {
        let mut inner = self.inner.lock();
        let m = inner
            .mmio
            .iter_mut()
            .find(|m| m.info == info)
            .ok_or(ArgumentError::InvalidArgument)?;
        if let Some(chunk) = m.chunks.get(&idx) {
            return Ok(chunk.id());
        }
        let start = (idx as usize)
            .checked_mul(MAX_SIZE)
            .and_then(|off| m.start.offset(off).ok())
            .filter(|start| *start < m.end)
            .ok_or(ArgumentError::InvalidArgument)?;
        let end = start.offset(MAX_SIZE).map_or(m.end, |e| e.min(m.end));
        let obj = crate::obj::Object::new_kernel();
        obj.map_phys_small(0, start, end, m.ct)?;
        crate::obj::register_object(obj.clone());
        m.chunks.insert(idx, obj.clone());
        Ok(obj.id())
    }

    pub fn add_child(&self, child: DeviceRef) {
        self.inner.lock().children.push(child);
    }

    pub fn objid(&self) -> ObjID {
        self.id
    }

    pub fn object(&self) -> ObjectRef {
        lookup_object(self.objid(), LookupFlags::ALLOW_DELETED).unwrap()
    }

    pub fn get_child_id(&self, n: usize) -> Option<ObjID> {
        self.inner.lock().children.get(n).map(|x| x.objid())
    }

    pub fn get_subobj_id(&self, t: u8, n: usize) -> Option<ObjID> {
        let t: SubObjectType = t.try_into().ok()?;
        let ret = self
            .inner
            .lock()
            .sub_objects
            .iter()
            .filter(|(x, _)| *x == t)
            .nth(n)
            .map(|x| x.1.id());
        ret
    }
}
