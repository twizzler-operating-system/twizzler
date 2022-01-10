use x86_64::VirtAddr;

use crate::obj::{ObjectRef, PageNumber};

bitflags::bitflags! {
    pub struct PageFaultFlags : u32 {
        const USER = 1;
        const INVALID = 2;
    }
}

pub enum PageFaultCause {
    InstructionFetch,
    Read,
    Write,
}

fn get_object_from_vaddr(addr: VirtAddr) -> ObjectRef {
    todo!()
}

pub fn page_fault(addr: VirtAddr, cause: PageFaultCause, flags: PageFaultFlags) {
    let obj = get_object_from_vaddr(addr);

    let page_number = PageNumber::from_address(addr);
    let obj_page_tree = obj.lock_page_tree();
    let page = obj_page_tree.get_page(page_number);
}
