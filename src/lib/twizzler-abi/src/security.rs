use bitflags::bitflags;

use crate::{marker::BaseType, object::ObjID};

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

impl BaseType for SecurityContextBase {
    fn init<T>(_t: T) -> Self {
        todo!()
    }

    fn tags() -> &'static [(crate::marker::BaseVersion, crate::marker::BaseTag)] {
        todo!()
    }
}
