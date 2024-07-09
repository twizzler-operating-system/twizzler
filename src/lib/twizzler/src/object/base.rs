use crate::ptr::InvPtrBuilder;

pub trait BaseType {}

pub struct BaseRef<'a, Base: BaseType> {
    ptr: &'a Base,
}

impl<'a, Base: BaseType> From<BaseRef<'a, Base>> for InvPtrBuilder<Base> {
    fn from(value: BaseRef<'a, Base>) -> Self {
        todo!()
    }
}

impl<'a, Base: BaseType> std::ops::Deref for BaseRef<'a, Base> {
    type Target = Base;

    fn deref(&self) -> &Self::Target {
        self.ptr
    }
}
