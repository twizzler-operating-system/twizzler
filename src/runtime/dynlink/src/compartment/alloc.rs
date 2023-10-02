use std::alloc::Layout;

use talc::Span;
use twizzler_abi::object::{MAX_SIZE, NULLPAGE_SIZE};
use twizzler_object::Object;

use super::Compartment;

pub struct ObjectAllocator {
    alloc: Box<dyn FnMut() -> Option<Object<u8>>>,
}

impl Compartment {
    fn add_alloc_object(&mut self, alloc: &mut ObjectAllocator) {
        if let Some(obj) = (alloc.alloc)() {
            unsafe {
                let memory = Span::new(
                    obj.base_mut_unchecked(),
                    (obj.base_mut_unchecked() as *mut u8).add(MAX_SIZE - NULLPAGE_SIZE),
                );
                // We ensure that we do not meet the conditions for this to return Err.
                let _ = self.allocator.claim(memory);
            }
            self.alloc_objects.insert(0, obj);
        }
    }

    pub(crate) unsafe fn alloc(
        &mut self,
        layout: Layout,
        alloc: &mut ObjectAllocator,
    ) -> Option<*mut u8> {
        if let Some(alloc) = self.allocator.malloc(layout).ok() {
            Some(alloc.as_ptr())
        } else {
            self.add_alloc_object(alloc);
            self.allocator.malloc(layout).ok().map(|p| p.as_ptr())
        }
    }
}
