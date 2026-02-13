//! Twizzler-native fiber stacks for wasmtime async support.
//!
//! Uses Twizzler's object system to allocate fiber stacks, with the
//! object null page serving as an MMU-enforced guard page. This is
//! the idiomatic Twizzler approach: the guard page is a first-class
//! architectural feature, not an ad-hoc PROT_NONE trick.

use std::ops::Range;
use std::sync::Arc;

use twizzler_abi::object::{Protections, NULLPAGE_SIZE};
use twizzler_abi::syscall::{
    sys_object_create, BackingType, LifetimeType, ObjectCreate, ObjectCreateFlags,
};
use twizzler_rt_abi::object::{twz_rt_map_object, MapFlags, ObjectHandle};

use wasmtime::{StackCreator, StackMemory};

/// A fiber stack backed by a Twizzler object.
///
/// The object is mapped without `NO_NULLPAGE`, so the first page
/// (0x1000 bytes) is the null page — any access faults with
/// `ObjectMemoryError::NullPageAccess`, acting as a guard page.
///
/// ```text
/// handle.start() + 0x0000  ┌───────────────────┐
///                           │ Null page (guard)  │  ← faults on access
/// handle.start() + 0x1000  ├───────────────────┤
///                           │                    │
///                           │  Stack (RW)        │  ← grows downward
///                           │                    │
/// handle.start() + 0x1000  ├───────────────────┤
///   + stack_size            │  top of stack      │
///                           └───────────────────┘
/// ```
struct TwizzlerFiberStack {
    handle: ObjectHandle,
    stack_size: usize,
}

unsafe impl Send for TwizzlerFiberStack {}
unsafe impl Sync for TwizzlerFiberStack {}

unsafe impl StackMemory for TwizzlerFiberStack {
    fn top(&self) -> *mut u8 {
        unsafe { self.handle.start().add(NULLPAGE_SIZE + self.stack_size) }
    }

    fn range(&self) -> Range<usize> {
        let base = unsafe { self.handle.start().add(NULLPAGE_SIZE) } as usize;
        base..base + self.stack_size
    }

    fn guard_range(&self) -> Range<*mut u8> {
        let base = self.handle.start();
        base..unsafe { base.add(NULLPAGE_SIZE) }
    }
}

/// Creates fiber stacks backed by Twizzler objects with null page guards.
pub struct TwizzlerStackCreator;

unsafe impl StackCreator for TwizzlerStackCreator {
    fn new_stack(&self, size: usize, _zeroed: bool) -> anyhow::Result<Box<dyn StackMemory>> {
        // Round up to page alignment.
        let page_size = NULLPAGE_SIZE;
        let size = if size == 0 {
            page_size
        } else {
            (size + (page_size - 1)) & !(page_size - 1)
        };

        let spec = ObjectCreate::new(
            BackingType::Normal,
            LifetimeType::Volatile,
            None,
            ObjectCreateFlags::empty(),
            Protections::READ | Protections::WRITE,
        );

        let id = sys_object_create(spec, &[], &[])
            .map_err(|e| anyhow::anyhow!("failed to create fiber stack object: {:?}", e))?;

        // Map WITHOUT NO_NULLPAGE — the null page IS the guard page.
        let handle = twz_rt_map_object(id.into(), MapFlags::READ | MapFlags::WRITE)
            .map_err(|e| anyhow::anyhow!("failed to map fiber stack object: {:?}", e))?;

        // Zero the stack region (demand-paged, only touched pages use physical memory).
        unsafe {
            core::ptr::write_bytes(handle.start().add(NULLPAGE_SIZE), 0, size);
        }

        Ok(Box::new(TwizzlerFiberStack {
            handle,
            stack_size: size,
        }))
    }
}

/// Create an `Arc<dyn StackCreator>` for use with `Config::with_host_stack`.
pub fn twizzler_stack_creator() -> Arc<dyn StackCreator> {
    Arc::new(TwizzlerStackCreator)
}
