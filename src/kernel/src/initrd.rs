use alloc::{borrow::ToOwned, collections::BTreeMap, string::String, sync::Arc};

use log::{debug, info};
use twizzler_abi::{
    meta::{MEXT_SIZED, MetaExt, MetaFlags, MetaInfo},
    object::{ObjID, Protections},
};
use twizzler_rt_abi::object::Nonce;

use crate::{
    memory::{
        VirtAddr,
        frame::PHYS_LEVEL_LAYOUTS,
        tracker::{FrameAllocFlags, alloc_frame},
    },
    obj::{self, ObjectRef, PageNumber},
    once::Once,
};
pub struct BootModule {
    pub start: VirtAddr,
    pub length: usize,
}

impl BootModule {
    fn as_slice(&self) -> &[u8] {
        let p = self.start.as_ptr();
        unsafe { core::slice::from_raw_parts(p, self.length) }
    }
}

#[derive(Default)]
pub struct BootObjects {
    pub init: Option<ObjectRef>,
    pub name_map: BTreeMap<String, ObjectRef>,
}

static BOOT_OBJECTS: Once<BootObjects> = Once::new();

pub fn get_boot_objects() -> &'static BootObjects {
    BOOT_OBJECTS
        .poll()
        .expect("tried to get BootObjects before processing modules")
}
unsafe fn any_as_u8_slice<T: Sized>(p: &T) -> &[u8] {
    unsafe {
        ::core::slice::from_raw_parts((p as *const T) as *const u8, ::core::mem::size_of::<T>())
    }
}
pub fn init(modules: &[BootModule]) {
    let mut boot_objects = BootObjects::default();
    for module in modules {
        let tar = tar_no_std::TarArchiveRef::new(module.as_slice())
            .expect("failed to open initrd as tar file");
        info!(
            "[kernel::initrd] loading module, {} MB...",
            module.as_slice().len() / (1024 * 1024)
        );
        let mut total_alloc = 0;
        for e in tar.entries() {
            let filename = e.filename();
            let Ok(name) = filename.as_str() else {
                continue;
            };
            let obj = obj::Object::new_kernel();
            debug!("[kernel::initrd]  loading {:?} -> {:x}", name, obj.id());
            let data = e.data();
            let mut total = 0;
            let mut pagenr = 1;
            while total < data.len() {
                let frame = alloc_frame(FrameAllocFlags::KERNEL);
                let va = frame.virtaddr().as_mut_ptr::<u8>();

                let thislen = core::cmp::min(frame.size(), data.len() - total);
                unsafe {
                    va.copy_from(data.as_ptr().add(total), thislen);
                }
                obj.add_frame(pagenr.into(), frame);

                total += thislen;
                pagenr += 1;
            }

            let mut buffer = [0; PHYS_LEVEL_LAYOUTS[0].size()];
            let meta = MetaInfo {
                nonce: Nonce(0),
                kuid: ObjID::new(0),
                default_prot: Protections::all(),
                flags: MetaFlags::empty(),
                fotcount: 0,
                extcount: 1,
            };
            log::debug!(
                "[kernel::initrd]  writing meta for {} -> {:x}, len = {}",
                name,
                obj.id(),
                e.data().len()
            );
            let me = MetaExt::new(MEXT_SIZED, e.data().len() as u64);
            unsafe {
                buffer[0..size_of::<MetaInfo>()].copy_from_slice(any_as_u8_slice(&meta));
                buffer[size_of::<MetaInfo>()..(size_of::<MetaInfo>() + size_of::<MetaExt>())]
                    .copy_from_slice(any_as_u8_slice(&me));
            }
            obj.write_bytes(
                buffer.as_ptr(),
                buffer.len(),
                PageNumber::meta_page().as_byte_offset(),
            )
            .expect("failed to write meta");

            obj::register_object(obj.clone());

            if name == "bootstrap" {
                boot_objects.init = Some(obj.clone());
            }
            boot_objects.name_map.insert(name.to_owned(), obj);
            total_alloc += total;
        }
        info!(
            "[kernel::initrd]  done, loaded {} MB of object data",
            total_alloc / (1024 * 1024)
        );
    }
    BOOT_OBJECTS.call_once(|| boot_objects);
}
