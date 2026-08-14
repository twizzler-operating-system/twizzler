//! Internal thread struct routines.

use std::{
    alloc::{GlobalAlloc, Layout},
    ffi::{CStr, CString},
    sync::Mutex,
};

use dynlink::tls::Tcb;
use monitor_api::RuntimeThreadControl;
use tracing::trace;
use twizzler_abi::{
    object::{ObjID, NULLPAGE_SIZE},
    thread::ThreadRepr,
};
use twizzler_rt_abi::{object::ObjectHandle, thread::ThreadSpawnArgs};

use crate::runtime::{
    alloc::LOCAL_ALLOCATOR,
    thread::{mgr::stackpool, tcb::tlspool, MIN_STACK_ALIGN},
    OUR_RUNTIME,
};

/// Internal representation of a thread, tracking the resources
/// allocated for this thread.
pub struct InternalThread {
    /// `None` if the repr object could not be mapped at spawn time -- see `impl_spawn`. The id is
    /// tracked separately so the thread is still identifiable, and so join can retry the map.
    repr_handle: Option<ObjectHandle>,
    repr_id: ObjID,
    stack_addr: usize,
    stack_size: usize,
    args_box: usize,
    pub(super) id: u32,
    pub(super) tls: *mut Tcb<RuntimeThreadControl>,
    tls_alloc_base: *mut u8,
    tls_layout: Layout,
    name: Mutex<Option<CString>>,
}

impl InternalThread {
    pub(super) fn new(
        repr_handle: Option<ObjectHandle>,
        repr_id: ObjID,
        stack_addr: usize,
        stack_size: usize,
        args_box: usize,
        id: u32,
        tls: *mut Tcb<RuntimeThreadControl>,
        tls_alloc_base: *mut u8,
        tls_layout: Layout,
    ) -> Self {
        Self {
            repr_handle,
            repr_id,
            stack_addr,
            stack_size,
            args_box,
            id,
            tls,
            name: Mutex::new(None),
            tls_alloc_base,
            tls_layout,
        }
    }

    pub(crate) fn objid(&self) -> ObjID {
        self.repr_id
    }

    #[allow(dead_code)]
    pub(crate) fn repr(&self) -> Option<&ThreadRepr> {
        // Safety: repr_handle ensures that the start memory will be alive, and that it contains
        // the thread repr struct at the base.
        let handle = self.repr_handle.as_ref()?;
        unsafe { (handle.start().add(NULLPAGE_SIZE) as *const ThreadRepr).as_ref() }
    }

    pub fn repr_handle(&self) -> Option<&ObjectHandle> {
        self.repr_handle.as_ref()
    }

    pub(super) fn set_repr_handle(&mut self, handle: ObjectHandle) {
        self.repr_handle = Some(handle);
    }

    pub fn set_name(&self, name: &CStr) {
        let name = name.to_owned();
        *self.name.lock().unwrap() = Some(name);
    }

    pub fn get_name(&self, name: &mut [u8]) -> usize {
        let th = self.name.lock().unwrap();
        match &*th {
            Some(n) => {
                let len = name.len().min(n.as_bytes_with_nul().len());
                name[..len].copy_from_slice(&n.as_bytes_with_nul()[..len]);
                len
            }
            None => 0,
        }
    }
}

impl Drop for InternalThread {
    fn drop(&mut self) {
        trace!("dropping InternalThread {}", self.id);
        unsafe {
            // Stack is manually allocated, so hand it to the next spawn, or free it directly if
            // the pool is full. Recycling needs exactly what freeing here already needs: this
            // thread is gone, so nothing is running on it.
            if self.stack_addr != 0 && !stackpool::put(self.stack_addr, self.stack_size) {
                OUR_RUNTIME.dealloc(
                    self.stack_addr as *mut u8,
                    Layout::from_size_align(self.stack_size, MIN_STACK_ALIGN).unwrap(),
                );
            }
            if self.args_box != 0 {
                // Args is allocated by a box.
                let _args = Box::from_raw(self.args_box as *mut ThreadSpawnArgs);
                drop(_args);
            }
            // Same deal as the stack above: hand the TLS region to the next spawn, or free it if
            // the pool is full. See `tcb::tlspool` for why recycling it needs nothing that freeing
            // it here did not already need.
            if !tlspool::put(self.tls_alloc_base, self.tls_layout) {
                LOCAL_ALLOCATOR.dealloc(self.tls_alloc_base, self.tls_layout);
            }
        }
    }
}
