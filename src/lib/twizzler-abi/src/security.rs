use bitflags::bitflags;

use crate::object::ObjID;

#[repr(C)]
pub struct SecurityContextBase {
    caps_data: ObjID,
    global_mask: Permissions,
}

bitflags! {
    pub struct Permissions : u32 {
        const READ = 1;
        const WRITE = 2;
        const EXEC = 4;
        const USE = 8;
    }
}
