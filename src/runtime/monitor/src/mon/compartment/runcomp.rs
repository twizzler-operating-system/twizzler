use std::{
    alloc::Layout,
    collections::HashMap,
    marker::PhantomData,
    ptr::NonNull,
    sync::atomic::{AtomicU64, Ordering},
};

use dynlink::compartment::CompartmentId;
use monitor_api::SharedCompConfig;
use talc::{ErrOnOom, Talc};
use twizzler_abi::syscall::{
    ThreadSync, ThreadSyncFlags, ThreadSyncOp, ThreadSyncReference, ThreadSyncSleep, ThreadSyncWake,
};
use twizzler_runtime_api::{MapError, ObjID};

use super::{comp_config::CompConfigObject, compthread::CompThread};
use crate::mon::space::{MapHandle, MapInfo};

pub const COMP_READY: u64 = 0x1;
pub const COMP_IS_BINARY: u64 = 0x2;
pub const COMP_THREAD_CAN_EXIT: u64 = 0x4;

pub struct RunComp {
    pub sctx: ObjID,
    pub instance: ObjID,
    pub name: String,
    pub compartment_id: CompartmentId,
    main: Option<CompThread>,
    deps: Vec<ObjID>,
    comp_config_object: CompConfigObject,
    alloc: Talc<ErrOnOom>,
    mapped_objects: HashMap<MapInfo, MapHandle>,
    flags: Box<AtomicU64>,
}

impl RunComp {
    /// Map an object into this compartment.
    pub fn map_object(&mut self, info: MapInfo) -> Result<MapHandle, MapError> {
        todo!()
    }

    /// Unmap and object from this compartment.
    pub fn unmap_object(&mut self, info: MapInfo) {
        let _ = self.mapped_objects.remove(&info);
        // Unmapping handled by dropping
    }

    /// Read the compartment config.
    pub fn comp_config(&self) -> SharedCompConfig {
        self.comp_config_object.read_comp_config()
    }

    /// Set the compartment config.
    pub fn set_comp_config(&mut self, scc: SharedCompConfig) {
        self.comp_config_object.write_config(scc)
    }

    /// Allocate some space in the compartment allocator, and initialize it.
    pub fn monitor_new<T: Copy + Sized + Send + Sync>(&mut self, data: T) -> Result<*mut T, ()> {
        unsafe {
            let place: NonNull<T> = self.alloc.malloc(Layout::new::<T>())?.cast();
            place.as_ptr().write(data);
            Ok(place.as_ptr() as *mut T)
        }
    }

    /// Allocate some space in the compartment allocator for a slice, and initialize it.
    pub fn monitor_new_slice<T: Copy + Sized + Send + Sync>(
        &mut self,
        data: &[T],
    ) -> Result<*mut T, ()> {
        unsafe {
            let place = self.alloc.malloc(Layout::array::<T>(data.len()).unwrap())?;
            let slice = core::slice::from_raw_parts_mut(place.as_ptr() as *mut T, data.len());
            slice.copy_from_slice(data);
            Ok(place.as_ptr() as *mut T)
        }
    }

    /// Set a flag on this compartment, and wakeup anyone waiting on flag change.
    pub fn set_flag(&self, val: u64) {
        self.flags.fetch_or(val, Ordering::SeqCst);
        let _ = twizzler_abi::syscall::sys_thread_sync(
            &mut [ThreadSync::new_wake(ThreadSyncWake::new(
                ThreadSyncReference::Virtual(&*self.flags),
                usize::MAX,
            ))],
            None,
        );
    }

    /// Check if a flag is set.
    pub fn has_flag(&self, flag: u64) -> bool {
        self.flags.load(Ordering::SeqCst) & flag != 0
    }

    /// Setup a [ThreadSyncSleep] for waiting until the flag is set. Returns None if the flag is
    /// already set.
    pub fn flag_waitable(&self, flag: u64) -> Option<ThreadSyncSleep> {
        let flags = self.flags.load(Ordering::SeqCst);
        if flags & flag == 0 {
            Some(ThreadSyncSleep::new(
                ThreadSyncReference::Virtual(&*self.flags),
                flags,
                ThreadSyncOp::Equal,
                ThreadSyncFlags::empty(),
            ))
        } else {
            None
        }
    }

    /// Return a waiter for this flag, allows calling .wait later to wait until a flag is set.
    pub fn waiter(&self, flag: u64) -> RunCompReadyWaiter<'_> {
        RunCompReadyWaiter {
            flag,
            sleep: self.flag_waitable(flag),
            _pd: PhantomData,
        }
    }
}

/// Allows waiting for a compartment to set a flag, sleeping the calling thread until the flag is
/// set.
pub struct RunCompReadyWaiter<'a> {
    flag: u64,
    sleep: Option<ThreadSyncSleep>,
    _pd: PhantomData<&'a ()>,
}

impl<'a> RunCompReadyWaiter<'a> {
    pub fn wait(&self) {
        loop {
            let Some(sleep) = self.sleep else { return };
            if sleep.ready() {
                return;
            }

            if let Err(e) =
                twizzler_abi::syscall::sys_thread_sync(&mut [ThreadSync::new_sleep(sleep)], None)
            {
                tracing::warn!("thread sync error: {:?}", e);
            }
        }
    }
}
