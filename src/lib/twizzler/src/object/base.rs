use std::ptr::addr_of;

use crate::ptr::{GlobalPtr, InvPtrBuilder};

pub trait BaseType {}

impl<Base: BaseType> From<&Base> for InvPtrBuilder<Base> {
    fn from(value: &Base) -> Self {
        InvPtrBuilder::from_global(GlobalPtr::from_va(addr_of!(*value)).unwrap())
    }
}
