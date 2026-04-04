//! Internal thread struct routines.

use std::{
    alloc::{GlobalAlloc, Layout},
    ffi::{CStr, CString},
    os::raw::c_char,
    sync::Mutex,
};

use dynlink::tls::Tcb;
use monitor_api::RuntimeThreadControl;
use tracing::trace;
use twizzler_abi::{object::NULLPAGE_SIZE, thread::ThreadRepr};
use twizzler_rt_abi::{object::ObjectHandle, thread::ThreadSpawnArgs};

use crate::runtime::{thread::MIN_STACK_ALIGN, OUR_RUNTIME};

/// Internal representation of a thread, tracking the resources
/// allocated for this thread.
pub struct InternalThread {
    repr_handle: ObjectHandle,
    stack_addr: usize,
    stack_size: usize,
    args_box: usize,
    pub(super) id: u32,
    _tls: *mut Tcb<RuntimeThreadControl>,
    name: Mutex<Option<CString>>,
}

impl InternalThread {
    pub(super) fn new(
        repr_handle: ObjectHandle,
        stack_addr: usize,
        stack_size: usize,
        args_box: usize,
        id: u32,
        tls: *mut Tcb<RuntimeThreadControl>,
    ) -> Self {
        Self {
            repr_handle,
            stack_addr,
            stack_size,
            args_box,
            id,
            _tls: tls,
            name: Mutex::new(None),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn repr(&self) -> &ThreadRepr {
        // Safety: repr_handle ensures that the start memory will be alive, and that it contains
        // the thread repr struct at the base.
        unsafe {
            (self.repr_handle.start().add(NULLPAGE_SIZE) as *const ThreadRepr)
                .as_ref()
                .unwrap()
        }
    }

    pub fn repr_handle(&self) -> &ObjectHandle {
        &self.repr_handle
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
            // Stack is manually allocated, just free it directly.
            OUR_RUNTIME.dealloc(
                self.stack_addr as *mut u8,
                Layout::from_size_align(self.stack_size, MIN_STACK_ALIGN).unwrap(),
            );
            // Args is allocated by a box.
            let _args = Box::from_raw(self.args_box as *mut ThreadSpawnArgs);
            drop(_args);
            tracing::debug!("TODO: drop TLS");
        }
    }
}
