use std::mem::size_of;

use twizzler_abi::object::ObjID;

use crate::Object;

#[derive(Debug, Clone, Copy)]
#[repr(C)]
struct FotName {
    name: u64,
    resolver: u64,
}

#[repr(C)]
union FotRef {
    id: ObjID,
    name: FotName,
}

#[repr(C)]
pub(crate) struct FotEntry {
    outgoing: FotRef,
    flags: u64,
    info: u64,
}

impl<T> Object<T> {
    pub(crate) fn get_fote(&self, idx: usize) -> &FotEntry {
        let end = self.slot.vaddr_meta();
        let off = idx * size_of::<FotEntry>();
        unsafe {
            (((end - off) + twizzler_abi::object::NULLPAGE_SIZE / 2) as *const FotEntry)
                .as_ref()
                .unwrap()
        }
    }
}
