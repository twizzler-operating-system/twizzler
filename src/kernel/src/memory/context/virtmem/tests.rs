#[cfg(test)]
mod test {
    use alloc::sync::Arc;

    use twizzler_abi::object::Protections;
    use twizzler_kernel_macros::kernel_test;

    use crate::memory::context::{
        kernel_context, KernelMemoryContext, KernelObjectHandle, ObjectContextInfo,
    };

    struct Foo {
        x: u32,
    }

    #[kernel_test]
    fn test_kernel_object() {
        let obj = crate::obj::Object::new_kernel();
        let obj = Arc::new(obj);
        crate::obj::register_object(obj.clone());

        let ctx = kernel_context();
        let mut handle = ctx.insert_kernel_object(ObjectContextInfo::new(
            obj,
            Protections::READ | Protections::WRITE,
            twizzler_abi::device::CacheType::WriteBack,
        ));

        *handle.base_mut() = Foo { x: 42 };
    }
}
