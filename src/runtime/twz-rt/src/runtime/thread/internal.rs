use std::{
    alloc::Layout,
    ffi::{CStr, CString},
    sync::Mutex,
};

use dynlink::tls::TlsRegion;
use tracing::trace;
use twizzler_abi::{object::NULLPAGE_SIZE, thread::ThreadRepr};
use twizzler_runtime_api::{CoreRuntime, ObjectHandle, ThreadSpawnArgs};

use crate::{
    monitor::get_monitor_actions,
    runtime::{thread::MIN_STACK_ALIGN, OUR_RUNTIME},
};

pub struct InternalThread {
    repr_handle: ObjectHandle,
    stack_addr: usize,
    stack_size: usize,
    args_box: usize,
    pub(super) id: u32,
    tls: TlsRegion,
    name: Mutex<CString>,
}

impl InternalThread {
    pub(super) fn new(
        repr_handle: ObjectHandle,
        stack_addr: usize,
        stack_size: usize,
        args_box: usize,
        id: u32,
        tls: TlsRegion,
    ) -> Self {
        Self {
            repr_handle,
            stack_addr,
            stack_size,
            args_box,
            id,
            tls,
            name: Mutex::new(CString::default()),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn repr(&self) -> &ThreadRepr {
        unsafe {
            (self.repr_handle.start.add(NULLPAGE_SIZE) as *const ThreadRepr)
                .as_ref()
                .unwrap()
        }
    }

    pub fn repr_handle(&self) -> &ObjectHandle {
        &self.repr_handle
    }

    pub fn set_name(&self, name: &CStr) {
        *self.name.lock().unwrap() = name.to_owned();
    }
}

impl Drop for InternalThread {
    fn drop(&mut self) {
        trace!("dropping InternalThread {}", self.id);
        unsafe {
            let alloc = OUR_RUNTIME.default_allocator();
            alloc.dealloc(
                self.stack_addr as *mut u8,
                Layout::from_size_align(self.stack_size, MIN_STACK_ALIGN).unwrap(),
            );
            let _args = Box::from_raw(self.args_box as *mut ThreadSpawnArgs);
            drop(_args);
            get_monitor_actions().free_tls_region(self.tls.dont_drop());
        }
    }
}
