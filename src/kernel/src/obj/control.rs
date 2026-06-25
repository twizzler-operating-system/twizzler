//! Defines a control object caching mechanism, useful for control objects whose base type
//! is updated frequently. Since these objects tend to also be small and use only one page
//! for the base, we optimize a bit by avoiding creating a kernel object handle if the base
//! type fits in one page.

use core::ptr::NonNull;

use twizzler_abi::{device::CacheType, object::Protections, syscall::MapFlags};

use crate::{
    memory::{
        context::{
            KernelMemoryContext, KernelObject, KernelObjectHandle, ObjectContextInfo,
            kernel_context,
        },
        frame::FrameRef,
        tracker::{FrameAllocFlags, alloc_frame},
    },
    obj::{ObjectRef, PageNumber},
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
            object.write_base(&base).unwrap();
            let kobj = kernel_context().insert_kernel_object(ObjectContextInfo::new(
                object.clone(),
                Protections::READ | Protections::WRITE,
                CacheType::WriteBack,
                MapFlags::empty(),
            ));
            QuickOrKernel::Kernel(kobj)
        } else {
            let frame = alloc_frame(
                FrameAllocFlags::ZEROED | FrameAllocFlags::WAIT_OK | FrameAllocFlags::KERNEL,
            );
            frame.set_wired(true);
            object.add_frame(PageNumber::base_page(), frame);
            let base_ptr = frame.virtaddr().as_mut_ptr::<Base>();
            unsafe { base_ptr.write(base) };
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
