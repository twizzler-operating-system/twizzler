use alloc::sync::Arc;

use twizzler_abi::{
    aux::{KernelInitInfo, KernelInitName},
    object::{MAX_SIZE, Protections},
    slot::RESERVED_STACK,
};
use twizzler_rt_abi::core::{RuntimeInfo, MinimalInitInfo, RUNTIME_INIT_MIN, InitInfoPtrs};
use xmas_elf::program::SegmentData;

use crate::{
    initrd::get_boot_objects,
    memory::{context::UserContext, VirtAddr},
    obj::ObjectRef,
    thread::current_memory_context,
};

pub fn create_blank_object() -> ObjectRef {
    let obj = crate::obj::Object::new();
    let obj = Arc::new(obj);
    crate::obj::register_object(obj.clone());
    obj
}

fn create_name_object() -> ObjectRef {
    let boot_objects = get_boot_objects();
    let obj = create_blank_object();
    let mut init_info = KernelInitInfo::new();
    for (name, obj) in &boot_objects.name_map {
        init_info.add_name(KernelInitName::new(name, obj.id()));
    }
    obj.write_base(&init_info);
    obj
}

pub extern "C" fn user_init() {
    // Reserve a big stack size
    const RTINFO_OFFSET: usize = 0x300000;
    const STACKTOP_OFFSET: usize = 0x200000;
    /* We need this scope to drop everything before we jump to user */
    let (rtinfo_start, entry) = {
        let vm = current_memory_context().unwrap();
        let boot_objects = get_boot_objects();

        let obj_text = create_blank_object();
        let obj_data = create_blank_object();
        let obj_stack = create_blank_object();
        let obj_name = create_name_object();
        crate::operations::map_object_into_context(
            twizzler_abi::slot::RESERVED_TEXT,
            obj_text.clone(),
            vm.clone(),
            Protections::READ | Protections::EXEC | Protections::WRITE,
        )
        .unwrap();
        crate::operations::map_object_into_context(
            twizzler_abi::slot::RESERVED_DATA,
            obj_data,
            vm.clone(),
            Protections::READ | Protections::WRITE,
        )
        .unwrap();
        crate::operations::map_object_into_context(
            twizzler_abi::slot::RESERVED_STACK,
            obj_stack,
            vm.clone(),
            Protections::READ | Protections::WRITE,
        )
        .unwrap();
        crate::operations::map_object_into_context(
            twizzler_abi::slot::RESERVED_KERNEL_INIT,
            obj_name,
            vm.clone(),
            Protections::READ,
        )
        .unwrap();

        let init_obj = boot_objects.init.as_ref().expect("no init found");
        crate::operations::map_object_into_context(
            twizzler_abi::slot::RESERVED_IMAGE,
            init_obj.clone(),
            vm.clone(),
            Protections::READ,
        )
        .unwrap();
        let obj1_data = crate::operations::read_object(init_obj);
        let elf = xmas_elf::ElfFile::new(&obj1_data).unwrap();
        let mut phinfo = None;
        for ph in elf.program_iter() {
            if ph.get_type() == Ok(xmas_elf::program::Type::Load) {
                let file_data = ph.get_data(&elf).unwrap();
                if let SegmentData::Undefined(file_data) = file_data {
                    let memory_addr = VirtAddr::new(ph.virtual_addr()).unwrap();
                    let memory_slice: &mut [u8] = unsafe {
                        core::slice::from_raw_parts_mut(
                            memory_addr.as_mut_ptr(),
                            ph.mem_size() as usize,
                        )
                    };

                    memory_slice.fill(0);
                    (&mut memory_slice[0..ph.file_size() as usize]).copy_from_slice(file_data);
                }
            }
            if ph.get_type() == Ok(xmas_elf::program::Type::Phdr) {
                phinfo = Some(ph);
            }
        }

        let rtinfo_start = MAX_SIZE * RESERVED_STACK + RTINFO_OFFSET;
        let rtinfo_start = rtinfo_start as *mut RuntimeInfo;
        let min_start = MAX_SIZE * RESERVED_STACK + RTINFO_OFFSET + core::cmp::max(core::mem::size_of::<RuntimeInfo>(), core::mem::align_of::<MinimalInitInfo>());
        let min_start = min_start as *mut MinimalInitInfo;

        let (phdrs, nr_phdrs) = phinfo.map(|ph| (ph.virtual_addr(), ph.mem_size() as usize / elf.header.pt2.ph_entry_size() as usize)).unwrap_or((0, 0));
        let min_info = MinimalInitInfo {
            args: core::ptr::null_mut(),
            argc: 0,
            envp: core::ptr::null_mut(),
            phdrs: phdrs as *mut core::ffi::c_void,
            nr_phdrs,
        };

        let rt_info = RuntimeInfo {
            flags: 0,
            kind: RUNTIME_INIT_MIN,
            init_info: InitInfoPtrs { min: min_start },
        };

        unsafe {
            min_start.write(min_info);
            rtinfo_start.write(rt_info);
        }

        // remove permission mappings from text segment
        let page_tree = obj_text.lock_page_tree();
        for r in page_tree.range(0.into()..usize::MAX.into()) {
            let range = *r.0..r.0.offset(r.1.length);
            vm.invalidate_object(
                obj_text.id(),
                &range,
                crate::obj::InvalidateMode::WriteProtect,
            );
        }

        (rtinfo_start, elf.header.pt2.entry_point())
    };

    unsafe {
        crate::arch::jump_to_user(
            VirtAddr::new(entry).unwrap(),
            VirtAddr::new((MAX_SIZE * RESERVED_STACK + STACKTOP_OFFSET) as u64).unwrap(),
            rtinfo_start as u64,
        );
    }
}
