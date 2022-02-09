use alloc::{collections::BTreeMap, sync::Arc, vec::Vec};
use twizzler_abi::{
    device::{BusType, DeviceId, DeviceRepr, DeviceType, SubObjectType},
    kso::{KactionError, KactionValue, KsoHdr},
    object::ObjID,
};
use x86_64::{PhysAddr, VirtAddr};

use crate::{mutex::Mutex, obj::ObjectRef, once::Once};

pub struct DeviceInner {
    sub_objects: Vec<(SubObjectType, ObjectRef)>,
    children: Vec<DeviceRef>,
}

pub struct Device {
    inner: Mutex<DeviceInner>,
    kaction: fn(DeviceRef) -> Result<KactionValue, KactionError>,
    bus_type: BusType,
    dev_type: DeviceType,
}

pub type DeviceRef = Arc<Device>;

static DEVICES: Once<Mutex<BTreeMap<ObjID, DeviceRef>>> = Once::new();

fn get_device_map() -> &'static Mutex<BTreeMap<ObjID, DeviceRef>> {
    DEVICES.call_once(|| Mutex::new(BTreeMap::new()))
}

pub fn create_busroot(
    name: &str,
    bt: BusType,
    kaction: fn(DeviceRef) -> Result<KactionValue, KactionError>,
) -> DeviceRef {
    let obj = Arc::new(crate::obj::Object::new());
    crate::obj::register_object(obj.clone());
    let device = Arc::new(Device {
        inner: Mutex::new(DeviceInner {
            sub_objects: Vec::new(),
            children: Vec::new(),
        }),
        kaction,
        bus_type: bt,
        dev_type: DeviceType::Bus,
    });
    let info = DeviceRepr::new(KsoHdr::new(name), DeviceType::Bus, bt, DeviceId::new(0));
    obj.write_base(&info);
    get_device_map().lock().insert(obj.id(), device.clone());
    device
}

pub fn create_device(
    name: &str,
    bt: BusType,
    id: DeviceId,
    kaction: fn(DeviceRef) -> Result<KactionValue, KactionError>,
) -> DeviceRef {
    let obj = Arc::new(crate::obj::Object::new());
    crate::obj::register_object(obj.clone());
    let device = Arc::new(Device {
        inner: Mutex::new(DeviceInner {
            sub_objects: Vec::new(),
            children: Vec::new(),
        }),
        kaction,
        bus_type: bt,
        dev_type: DeviceType::Device,
    });
    let info = DeviceRepr::new(KsoHdr::new(name), DeviceType::Device, bt, id);
    obj.write_base(&info);
    get_device_map().lock().insert(obj.id(), device.clone());
    device
}

impl Device {
    pub fn add_info<T>(&self, info: &T) {
        let obj = Arc::new(crate::obj::Object::new());
        obj.write_base(info);
        crate::obj::register_object(obj.clone());
        self.inner
            .lock()
            .sub_objects
            .push((SubObjectType::Info, obj));
    }

    pub fn add_mmio(&self, start: PhysAddr, end: PhysAddr) {
        let obj = Arc::new(crate::obj::Object::new());
        obj.map_phys(start, end);
        crate::obj::register_object(obj.clone());
        self.inner
            .lock()
            .sub_objects
            .push((SubObjectType::Mmio, obj));
    }

    pub fn add_child(&self, child: DeviceRef) {
        self.inner.lock().children.push(child);
    }
}
