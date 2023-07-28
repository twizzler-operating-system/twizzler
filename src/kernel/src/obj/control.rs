use core::ptr::NonNull;

use twizzler_abi::{device::CacheType, marker::BaseType, object::Protections};

use crate::{
    memory::{
        context::{
            kernel_context, KernelMemoryContext, KernelObject, KernelObjectHandle,
            ObjectContextInfo,
        },
        frame::{alloc_frame, FrameRef, PhysicalFrameFlags},
    },
    obj::{pages::Page, ObjectRef, PageNumber},
    userinit::create_blank_object,
};

struct QuickBase<Base> {
    base_ptr: NonNull<Base>,
    base_frame: FrameRef,
}

enum QuickOrKernel<Base> {
    Quick(QuickBase<Base>),
    Kernel(KernelObject<Base>),
}

pub struct ControlObjectCacher<Base> {
    object: ObjectRef,
    quick_or_kernel: QuickOrKernel<Base>,
}

unsafe impl<Base> Send for ControlObjectCacher<Base> {}

impl<Base: BaseType> ControlObjectCacher<Base> {
    pub fn new(base: Base) -> Self {
        let object = create_blank_object();
        let qok = if core::mem::size_of::<Base>() > PageNumber::PAGE_SIZE {
            let kobj = kernel_context().insert_kernel_object(ObjectContextInfo::new(
                object.clone(),
                Protections::READ | Protections::WRITE,
                CacheType::WriteBack,
            ));
            QuickOrKernel::Kernel(kobj)
        } else {
            let frame = alloc_frame(PhysicalFrameFlags::ZEROED);
            let page = Page::new_wired(frame.start_address(), CacheType::WriteBack);
            let base_ptr = unsafe {
                let ptr = page.get_mut_to_val(0);
                *(core::mem::transmute::<_, &mut Base>(ptr)) = base;
                ptr
            };
            object.add_page(PageNumber::base_page(), page);
            QuickOrKernel::Quick(QuickBase {
                base_ptr: NonNull::new(base_ptr).unwrap(),
                base_frame: frame,
            })
        };
        Self {
            object,
            quick_or_kernel: qok,
        }
    }

    pub unsafe fn base(&self) -> &Base {
        match &self.quick_or_kernel {
            QuickOrKernel::Quick(quick) => quick.base_ptr.as_ref(),
            QuickOrKernel::Kernel(kobj) => kobj.base(),
        }
    }
}
