use alloc::{borrow::ToOwned, collections::BTreeMap, string::String};

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
        tracker::{FrameAllocFlags, FrameAllocator},
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
            let mut small_fa = FrameAllocator::new(
                FrameAllocFlags::KERNEL | FrameAllocFlags::ZEROED,
                PHYS_LEVEL_LAYOUTS[0],
            );
            let mut large_fa = FrameAllocator::new(
                FrameAllocFlags::KERNEL | FrameAllocFlags::ZEROED,
                PHYS_LEVEL_LAYOUTS[1],
            );
            let nr_frames_per_large = PHYS_LEVEL_LAYOUTS[1].size() / PHYS_LEVEL_LAYOUTS[0].size();
            while total < data.len() {
                if pagenr & (nr_frames_per_large - 1) == 0
                    && total + PHYS_LEVEL_LAYOUTS[1].size() <= data.len()
                {
                    if let Some(large_frame) = large_fa.try_allocate() {
                        let va = large_frame.virtaddr().as_mut_ptr::<u8>();
                        let thislen = core::cmp::min(large_frame.size(), data.len() - total);
                        unsafe {
                            va.copy_from(data.as_ptr().add(total), thislen);
                        }
                        obj.add_frame(pagenr.into(), large_frame);
                        total += thislen;
                        pagenr += large_frame.size() / PHYS_LEVEL_LAYOUTS[0].size();
                        continue;
                    }
                }

                let frame = small_fa
                    .try_allocate()
                    .expect("failed to allocate frame for initrd");
                let va = frame.virtaddr().as_mut_ptr::<u8>();

                let thislen = core::cmp::min(frame.size(), data.len() - total);
                unsafe {
                    va.copy_from(data.as_ptr().add(total), thislen);
                }
                obj.add_frame(pagenr.into(), frame);

                total += thislen;
                pagenr += frame.size() / PHYS_LEVEL_LAYOUTS[0].size();
            }

            let mut buffer = [0; PHYS_LEVEL_LAYOUTS[0].size()];
            // This rewrite exists only to add `MEXT_SIZED`, so it keeps the nonce and kuid
            // `new_kernel` wrote. Zeroing them, as this used to, left every boot object with an ID
            // that does not verify against its own meta page -- and would contradict the ID check
            // recorded when that page was first written.
            let existing = obj.read_meta();
            let meta = MetaInfo {
                nonce: existing.map(|m| m.nonce).unwrap_or(Nonce(0)),
                kuid: existing.map(|m| m.kuid).unwrap_or(ObjID::new(0)),
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
            // The only place the initrd's name-to-ObjID mapping exists: userspace gets it via
            // `KernelInitInfo`, but nothing records it where a kernel-side counter reporting bare
            // ObjIDs can be read against it. See unmap.md's latched-object identification.
            // `emerglogln!`, not `info!`: initrd parsing runs before the logger is capturing, which
            // is why the neighbouring `info!("done, loaded ...")` never appears in a boot log
            // either. Same reason the shutdown counters use it.
            emerglogln!("== initrd object {} => {}", name, obj.id());
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
