use spin::Once;
use x86_64::VirtAddr;

use crate::obj::{self, pages::Page};
use crate::obj::{LookupFlags, ObjectRef};
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
}

static BOOT_OBJECTS: Once<BootObjects> = Once::new();

pub fn get_boot_objects() -> &'static BootObjects {
    BOOT_OBJECTS
        .poll()
        .expect("tried to get BootObjects before processing modules")
}

pub fn init(modules: &[BootModule]) {
    let mut boot_objects = BootObjects::default();
    for module in modules {
        let tar = tar_no_std::TarArchiveRef::new(module.as_slice());
        logln!("loading module...");
        for e in tar.entries() {
            let obj = obj::Object::new();
            logln!("  loading {:?} -> {:x}", e.filename(), obj.id());
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
            let id = obj.id();
            obj::register_object(obj);
            match e.filename().as_str() {
                "init" => {
                    boot_objects.init = Some(obj::lookup_object(id, LookupFlags::empty()).unwrap())
                }
                _ => {}
            }
        }
    }
    BOOT_OBJECTS.call_once(|| boot_objects);
}
