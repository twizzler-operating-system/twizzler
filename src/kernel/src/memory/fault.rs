use x86_64::VirtAddr;

use crate::{
    memory::context::MappingPerms,
    obj::{pages::Page, PageNumber},
    thread::{current_memory_context, current_thread_ref},
};

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

pub fn page_fault(addr: VirtAddr, cause: PageFaultCause, flags: PageFaultFlags, ip: VirtAddr) {
    if false {
        logln!(
            "(thrd {}) page fault at {:?} cause {:?} flags {:?}, at {:?}",
            current_thread_ref().map(|t| t.id()).unwrap_or(0),
            addr,
            cause,
            flags,
            ip
        );
    }
    /* TODO: null page */
    if !flags.contains(PageFaultFlags::USER) && addr.as_u64() >= 0xffff000000000000
    /*TODO */
    {
        panic!("kernel page fault")
    }
    let vmc = current_memory_context();
    if vmc.is_none() {
        panic!("page fault in thread with no memory context");
    }
    let vmc = vmc.unwrap();
    let mapping = { vmc.inner().lookup_object(addr) };

    if let Some(mapping) = mapping {
        let objid = mapping.obj.id();
        let page_number = PageNumber::from_address(addr);
        let mut obj_page_tree = mapping.obj.lock_page_tree();
        let is_write = cause == PageFaultCause::Write;

        if let Some((page, cow)) = obj_page_tree.get_page(page_number, is_write) {
            let mut vmc = vmc.inner();
            /* check if mappings changed */
            if vmc.lookup_object(addr).map_or(0.into(), |o| o.obj.id()) != objid {
                drop(vmc);
                drop(obj_page_tree);
                return page_fault(addr, cause, flags, ip);
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
            if false {
                logln!(
                    "  => mapping {:?} page {:?} {:?}",
                    objid,
                    page_number,
                    page.physical_address()
                );
            }
            vmc.map_object_page(addr, page, perms);
            if flags.contains(PageFaultFlags::PRESENT) {
                unsafe {
                    // TODO
                    asm!("mov rax, cr3", "mov cr3, rax", lateout("rax") _);
                }
            }
        } else {
            let page = Page::new();
            obj_page_tree.add_page(page_number, page);
            drop(obj_page_tree);
            page_fault(addr, cause, flags, ip);
        }
    } else {
        //TODO: fault
        if let Some(th) = current_thread_ref() {
            logln!("user fs {:?}", th.arch.user_fs);
        }
        panic!("page fault: no obj");
    }
}
