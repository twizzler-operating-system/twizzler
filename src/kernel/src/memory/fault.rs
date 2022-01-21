use x86_64::VirtAddr;

use crate::{memory::context::MappingPerms, obj::PageNumber, thread::current_memory_context};

bitflags::bitflags! {
    pub struct PageFaultFlags : u32 {
        const USER = 1;
        const INVALID = 2;
        const PRESENT = 4;
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum PageFaultCause {
    InstructionFetch,
    Read,
    Write,
}

pub fn page_fault(addr: VirtAddr, cause: PageFaultCause, flags: PageFaultFlags) {
    logln!(
        "page fault at {:?} cause {:?} flags {:?}",
        addr,
        cause,
        flags
    );
    if !flags.contains(PageFaultFlags::USER) {
        panic!("kernel page fault")
    }
    let vmc = current_memory_context();
    if vmc.is_none() {
        panic!("page fault in thread with no memory context");
    }
    let vmc = vmc.unwrap();
    let mapping = { vmc.lock().lookup_object(addr) };

    if let Some(mapping) = mapping {
        let objid = mapping.obj.id();
        let page_number = PageNumber::from_address(addr);
        let mut obj_page_tree = mapping.obj.lock_page_tree();
        let is_write = cause == PageFaultCause::Write;
        let (page, cow) = obj_page_tree
            .get_page(page_number, is_write)
            .expect("TODO: non present page");

        {
            let mut vmc = vmc.lock();
            /* check if mappings changed */
            if vmc.lookup_object(addr).map_or(0, |o| o.obj.id()) != objid {
                drop(vmc);
                drop(obj_page_tree);
                return page_fault(addr, cause, flags);
            }
            let v: *const u8 = page.as_virtaddr().as_ptr();
            unsafe {
                let _slice = core::slice::from_raw_parts(v, 0x1000);
                //logln!("{:?}", slice);
            }

            //TODO: get these perms from the second lookup
            let perms = if cow {
                mapping.perms & MappingPerms::WRITE.complement()
            } else {
                mapping.perms
            };
            vmc.map_object_page(addr, page, perms);
        }
    } else {
        //TODO: fault
        panic!("page fault: no obj");
    }
}
