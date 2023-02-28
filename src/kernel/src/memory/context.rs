//! A memory context is the primary abstraction the kernel uses for manipulating whatever memory system this machine
//! has. This includes both kernel memory management (kernel memory allocator) and management of userland resources. The
//! rest of the kernel can interact with the functions in [UserContext] to operate on userland-visible memory state
//! (e.g. objects' slots mappings in x86), and the functions in [KernelMemoryContext] to operate on kernel memory state
//! (e.g. the allocator and kernel mappings in the higher-half on x86).

use core::alloc::Layout;
use core::ops::Range;
use core::ptr::NonNull;

use alloc::sync::Arc;
use twizzler_abi::marker::BaseType;
use twizzler_abi::object::ObjID;
use twizzler_abi::{device::CacheType, object::Protections};

use crate::obj::ObjectRef;
use crate::obj::{InvalidateMode, PageNumber};

use crate::syscall::object::ObjectHandle;

impl ObjectHandle for ContextRef {
    fn create_with_handle(_obj: ObjectRef) -> Self {
        Arc::new(Context::new())
    }
}

pub mod virtmem;

/// The context type for this system (e.g. [virtmem::VirtContext] for x86).
pub type Context = virtmem::VirtContext;
/// The [Context] type wrapped in an [Arc].
pub type ContextRef = Arc<Context>;

/// A trait that defines the operations expected by higher-level object management routines. An architecture-dependent
/// type can be created that implements Context, which can then be used by the rest of the kernel to manage objects in a
/// context (e.g. an address space).
pub trait UserContext {
    /// The type that is expected for upcall information (e.g. an entry address).
    type UpcallInfo;
    /// The type that is expected for informing the context how to map the object (e.g. a slot number).
    type MappingInfo;

    /// Set the context's upcall information.
    fn set_upcall(&self, target: Self::UpcallInfo);
    /// Retrieve the context's upcall information.
    fn get_upcall(&self) -> Option<Self::UpcallInfo>;
    /// Switch to this context.
    fn switch_to(&self);
    /// Insert a range of an object into the context. The implementation may choose to use start and len as hints, but
    /// should keep in mind that calls to `insert_object` may be generated by faults, and so should strive to resolve
    /// the fault by correctly mapping the object as requested.
    fn insert_object(
        self: &Arc<Self>,
        mapping_info: Self::MappingInfo,
        object_info: &ObjectContextInfo,
    ) -> Result<(), InsertError>;
    /// Lookup an object within this context. Once this function returns, no guarantees are made about if the object
    /// remains mapped as is.
    fn lookup_object(&self, info: Self::MappingInfo) -> Option<ObjectContextInfo>;
    /// Invalidate any mappings for a particular object.
    fn invalidate_object(&self, obj: ObjID, range: &Range<PageNumber>, mode: InvalidateMode);
    /// Remove an object from the context.
    fn remove_object(&self, info: Self::MappingInfo);
}

/// A struct containing information about how an object is inserted within a context.
pub struct ObjectContextInfo {
    object: ObjectRef,
    perms: Protections,
    cache: CacheType,
}

impl ObjectContextInfo {
    pub fn new(object: ObjectRef, perms: Protections, cache: CacheType) -> Self {
        Self {
            object,
            perms,
            cache,
        }
    }

    /// The object.
    pub fn object(&self) -> &ObjectRef {
        &self.object
    }

    /// The protections.
    pub fn prot(&self) -> Protections {
        self.perms
    }

    /// The caching type.
    pub fn cache(&self) -> CacheType {
        self.cache
    }
}

/// Errors for inserting objects into a [Context].
pub enum InsertError {
    Occupied,
}

/// A trait for kernel-related memory context actions.
pub trait KernelMemoryContext {
    type Handle<T: BaseType>: KernelObjectHandle<T>;
    /// Called once during initialization, after which calls to the other function in this trait may be called.
    fn init_allocator(&self);
    /// Allocate a contiguous chunk of memory. This is not expected to be good for small allocations, this should be
    /// used to grab large chunks of memory to then serve pieces of using an actual allocator. Returns a pointer to the
    /// allocated memory and the size of the allocation (must be greater than layout's size).
    fn allocate_chunk(&self, layout: Layout) -> NonNull<u8>;
    /// Deallocate a previously allocated chunk.
    ///
    /// # Safety
    /// The call must ensure that the passed in pointer came from a call to [Self::allocate_chunk] and has the same
    /// layout data as was passed to that allocation call.
    unsafe fn deallocate_chunk(&self, layout: Layout, ptr: NonNull<u8>);
    /// Called once after all secondary processors have been booted and are waiting at their main barrier. Should finish
    /// any setup needed in the kernel context before all CPUs can freely use this context.
    fn prep_smp(&self);
    /// Insert object into kernel space. The context need only support a small number of kernel-memory-mapped objects.
    /// The mapping is released when the returned handle is dropped.
    fn insert_object<T: BaseType>(&self, info: ObjectContextInfo) -> Self::Handle<T>;
}

pub trait KernelObjectHandle<T> {
    fn base(&self) -> &T;
    fn base_mut(&mut self) -> &mut T;
    fn lea_raw<R>(&self, iptr: *const R) -> Option<&R>;
    fn lea_raw_mut<R>(&mut self, iptr: *mut R) -> Option<&mut R>;
}

lazy_static::lazy_static! {
    static ref KERNEL_CONTEXT: ContextRef = {
        let c = virtmem::VirtContext::new_kernel();
        c.init_kernel_context();
        Arc::new(c)
    };
}

/// Return a reference to the kernel context. The kernel context is the default context that a thread is in if it's not
/// a userland thread. It's the main context used during init and during secondary processor initialization. It may be
/// used to manipulate kernel memory mappings the same as any other context.
pub fn kernel_context() -> &'static ContextRef {
    &KERNEL_CONTEXT
}
