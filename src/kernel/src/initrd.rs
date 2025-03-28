use alloc::{borrow::ToOwned, collections::BTreeMap, string::String, sync::Arc};

use twizzler_abi::{
    meta::{MetaExt, MetaFlags, MetaInfo, MEXT_SIZED},
    object::{ObjID, MAX_SIZE, NULLPAGE_SIZE},
};
use twizzler_rt_abi::object::Nonce;

use crate::{
    memory::VirtAddr,
    obj::{self, pages::Page, ObjectRef, PageNumber},
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
    ::core::slice::from_raw_parts((p as *const T) as *const u8, ::core::mem::size_of::<T>())
}
pub fn init(modules: &[BootModule]) {
    let mut boot_objects = BootObjects::default();
    for module in modules {
        let tar = tar_no_std::TarArchiveRef::new(module.as_slice())
            .expect("failed to open initrd as tar file");
        logln!(
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
            logln!("[kernel::initrd]  loading {:?} -> {:x}", name, obj.id());
            let data = e.data();
            let mut total = 0;
            let mut pagenr = 1;
            while total < data.len() {
                let page = Page::new();
                let va: *mut u8 = page.as_virtaddr().as_mut_ptr();
                let thislen = core::cmp::min(4096, data.len() - total);
                unsafe {
                    va.copy_from(data.as_ptr().add(total), thislen);
                }
                obj.add_page(pagenr.into(), page);
                total += thislen;
                pagenr += 1;
            }

            let mut buffer = [0; 0x1000];
            let meta = MetaInfo {
                nonce: Nonce(0),
                kuid: ObjID::new(0),
                flags: MetaFlags(0),
                fotcount: 0,
                extcount: 1,
            };
            let me = MetaExt {
                tag: MEXT_SIZED,
                value: e.data().len() as u64,
            };
            unsafe {
                buffer[0..size_of::<MetaInfo>()].copy_from_slice(any_as_u8_slice(&meta));
                buffer[size_of::<MetaInfo>()..(size_of::<MetaInfo>() + size_of::<MetaExt>())]
                    .copy_from_slice(any_as_u8_slice(&me));
            }
            let page = Page::new();
            let va: *mut u8 = page.as_virtaddr().as_mut_ptr();
            unsafe {
                va.copy_from(buffer.as_ptr(), 0x1000);
            }
            obj.add_page(PageNumber::from_offset(MAX_SIZE - NULLPAGE_SIZE), page);

            let obj = Arc::new(obj);
            obj::register_object(obj.clone());

            if name == "bootstrap" {
                boot_objects.init = Some(obj.clone());
            }
            boot_objects.name_map.insert(name.to_owned(), obj);
            total_alloc += total;
        }
        logln!(
            "[kernel::initrd]  done, loaded {} MB of object data",
            total_alloc / (1024 * 1024)
        );
    }
    BOOT_OBJECTS.call_once(|| boot_objects);
}
