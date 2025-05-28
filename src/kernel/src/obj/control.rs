//! Defines a control object caching mechanism, useful for control objects whose base type
//! is updated frequently. Since these objects tend to also be small and use only one page
//! for the base, we optimize a bit by avoiding creating a kernel object handle if the base
//! type fits in one page.

use core::ptr::NonNull;

use twizzler_abi::{device::CacheType, object::Protections};

use crate::{
    memory::{
        context::{
            kernel_context, KernelMemoryContext, KernelObject, KernelObjectHandle,
            ObjectContextInfo,
        },
        frame::FrameRef,
        tracker::{alloc_frame, FrameAllocFlags},
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

/// Manages a kernel control object, allowing access to the base type, while accelerating
/// that access for the common case.
pub struct ControlObjectCacher<Base> {
    object: ObjectRef,
    quick_or_kernel: QuickOrKernel<Base>,
}

unsafe impl<Base> Send for ControlObjectCacher<Base> {}
unsafe impl<Base> Sync for ControlObjectCacher<Base> {}

impl<Base> ControlObjectCacher<Base> {
    /// Create a new control object cacher, making a new, blank object for it. Initialize the base
    /// with the provided initial data.
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
            // TODO: this should probably use the frame Page variant.
            let frame = alloc_frame(
                FrameAllocFlags::ZEROED | FrameAllocFlags::WAIT_OK | FrameAllocFlags::KERNEL,
            );
            let page = Page::new_wired(frame.start_address(), frame.size(), CacheType::WriteBack);
            let base_ptr = unsafe {
                let ptr = page.get_mut_to_val::<Base>(0);
                ptr.write(base);
                ptr
            };
            object.add_page(PageNumber::base_page(), page, None);
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

    /// Get a reference to the base of this object.
    ///
    /// # Safety
    /// The caller must ensure that the base type is not aliased in a way that leads to unsoundness
    /// for this type.
    pub fn base(&self) -> &Base {
        match &self.quick_or_kernel {
            QuickOrKernel::Quick(quick) => unsafe { quick.base_ptr.as_ref() },
            QuickOrKernel::Kernel(kobj) => kobj.base(),
        }
    }

    /// Get the handle to the underlying object.
    pub fn object(&self) -> &ObjectRef {
        &self.object
    }
}
