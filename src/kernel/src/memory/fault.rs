use x86_64::VirtAddr;

use crate::{
    obj::{ObjectRef, PageNumber},
    thread::current_memory_context,
};

bitflags::bitflags! {
    pub struct PageFaultFlags : u32 {
        const USER = 1;
        const INVALID = 2;
        const PRESENT = 4;
    }
}

#[derive(Debug, Copy, Clone)]
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
    let obj = { vmc.lock().lookup_object(addr) };

    if let Some(obj) = obj {
        let page_number = PageNumber::from_address(addr);
        let obj_page_tree = obj.lock_page_tree();
        let page = obj_page_tree.get_page(page_number);
    } else {
        //TODO: fault
        panic!("page fault: no obj");
    }
}
