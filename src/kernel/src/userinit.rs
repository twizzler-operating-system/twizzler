use alloc::sync::Arc;
use twizzler_abi::{
    aux::{AuxEntry, KernelInitInfo, KernelInitName},
    object::Protections,
};
use xmas_elf::program::SegmentData;

use crate::{
    initrd::get_boot_objects, memory::VirtAddr, obj::ObjectRef, thread::current_memory_context,
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
    /* We need this scope to drop everything before we jump to user */
    let (aux_start, entry) = {
        let vm = current_memory_context().unwrap();
        let boot_objects = get_boot_objects();

        let obj_text = create_blank_object();
        let obj_data = create_blank_object();
        let obj_stack = create_blank_object();
        let obj_name = create_name_object();
        crate::operations::map_object_into_context(
            twizzler_abi::slot::RESERVED_TEXT,
            obj_text,
            vm.clone(),
            Protections::READ | Protections::EXEC,
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
            vm,
            Protections::READ,
        )
        .unwrap();
        let init_obj = boot_objects.init.as_ref().expect("no init found");
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

        fn append_aux(aux: *mut AuxEntry, entry: AuxEntry) -> *mut AuxEntry {
            unsafe {
                *aux = entry;
                aux.add(1)
            }
        }

        let aux_start: u64 = (1 << 30) * 2 + 0x300000;
        let aux_start = aux_start as *mut twizzler_abi::aux::AuxEntry;
        let mut aux = aux_start;

        if let Some(phinfo) = phinfo {
            aux = append_aux(
                aux,
                AuxEntry::ProgramHeaders(
                    phinfo.virtual_addr(),
                    phinfo.mem_size() as usize / elf.header.pt2.ph_entry_size() as usize,
                ),
            )
        }

        aux = append_aux(aux, AuxEntry::ExecId(init_obj.id()));
        append_aux(aux, AuxEntry::Null);
        (aux_start, elf.header.pt2.entry_point())
    };

    unsafe {
        crate::arch::jump_to_user(
            VirtAddr::new(entry).unwrap(),
            VirtAddr::new((1 << 30) * 2 + 0x200000).unwrap(),
            aux_start as u64,
        );
    }
}
