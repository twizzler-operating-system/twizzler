use alloc::{borrow::ToOwned, collections::BTreeMap, string::String, sync::Arc, vec::Vec};
use core::mem::size_of;

use memoffset::offset_of;
use twizzler_abi::{
    device::{
        BusType, CacheType, DeviceId, DeviceInterrupt, DeviceRepr, DeviceType, MmioInfo,
        SubObjectType,
    },
    kso::{
        pack_kaction_pin_token_and_len, unpack_kaction_pin_start_and_len, KactionCmd,
        KactionGenericCmd, KactionValue, KsoHdr,
    },
    object::{ObjID, NULLPAGE_SIZE},
    syscall::PinnedPage,
};
use twizzler_rt_abi::{
    error::{ArgumentError, GenericError, ObjectError},
    Result,
};

use crate::{
    interrupt::WakeInfo,
    memory::PhysAddr,
    mutex::Mutex,
    obj::{lookup_object, LookupFlags, ObjectRef},
    once::Once,
    syscall::create_user_slice,
};

pub struct DeviceInner {
    sub_objects: Vec<(SubObjectType, ObjectRef)>,
    children: Vec<DeviceRef>,
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

static DEVICES: Once<Mutex<BTreeMap<ObjID, DeviceRef>>> = Once::new();

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

static KSO_MANAGER: Once<KsoManager> = Once::new();

fn get_kso_manager() -> &'static KsoManager {
    KSO_MANAGER.call_once(|| {
        let root = Arc::new(crate::obj::Object::new_kernel());
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
            KactionGenericCmd::PinPages(_np) => {
                let id = id.ok_or(ArgumentError::InvalidArgument)?;
                let obj =
                    lookup_object(id, LookupFlags::empty()).ok_or(ObjectError::NoSuchObject)?;

                let (start, len) =
                    unpack_kaction_pin_start_and_len(arg2).ok_or(ArgumentError::InvalidArgument)?;

                let slice = unsafe { create_user_slice::<PinnedPage>(arg, len as u64) }
                    .ok_or(ArgumentError::InvalidArgument)?;

                let (pins, token) = obj
                    .pin(
                        (start as usize)
                            .try_into()
                            .map_err(|_| ArgumentError::InvalidArgument)?,
                        slice.len(),
                    )
                    .ok_or(GenericError::Internal)?;
                let len: u32 = core::cmp::min(pins.len(), len as usize).try_into().unwrap();

                for i in 0..(len as usize) {
                    slice[i] = PinnedPage::new(pins[i].into())
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
    let obj = Arc::new(crate::obj::Object::new_kernel());
    crate::obj::register_object(obj.clone());
    let device = Arc::new(Device {
        inner: Mutex::new(DeviceInner {
            sub_objects: Vec::new(),
            children: Vec::new(),
        }),
        kaction,
        bus_type: bt,
        dev_type: DeviceType::Bus,
        id: obj.id(),
        name: name.to_owned(),
    });
    let info = DeviceRepr::new(KsoHdr::new(name), DeviceType::Bus, bt, DeviceId::new(0));
    obj.write_base(&info);
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
    let obj = Arc::new(crate::obj::Object::new_kernel());
    crate::obj::register_object(obj.clone());
    let device = Arc::new(Device {
        inner: Mutex::new(DeviceInner {
            sub_objects: Vec::new(),
            children: Vec::new(),
        }),
        kaction,
        bus_type: bt,
        dev_type: DeviceType::Device,
        id: obj.id(),
        name: name.to_owned(),
    });
    let info = DeviceRepr::new(KsoHdr::new(name), DeviceType::Device, bt, id);
    obj.write_base(&info);
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
        let obj = Arc::new(crate::obj::Object::new_kernel());
        obj.write_base(info);
        crate::obj::register_object(obj.clone());
        self.inner
            .lock()
            .sub_objects
            .push((SubObjectType::Info, obj));
    }

    pub fn add_mmio(&self, start: PhysAddr, end: PhysAddr, ct: CacheType, info: u64) {
        let obj = Arc::new(crate::obj::Object::new_kernel());
        obj.map_phys(start, end, ct);
        let mmio_info = MmioInfo {
            length: (end - start) as u64,
            cache_type: CacheType::Uncacheable,
            info,
        };
        obj.write_base(&mmio_info);
        crate::obj::register_object(obj.clone());
        self.inner
            .lock()
            .sub_objects
            .push((SubObjectType::Mmio, obj));
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
