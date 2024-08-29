//! This crate exists to break a circular dependency between twz-rt and monitor. We use extern
//! symbols so that we can just call into the monitor without having to have it as an explicit
//! dependency.

#![feature(naked_functions)]
#![feature(pointer_byte_offsets)]
#![feature(pointer_is_aligned)]
use std::{
    alloc::Layout,
    ptr::NonNull,
    sync::{
        atomic::{AtomicPtr, Ordering},
        OnceLock,
    },
};

use dynlink::tls::{Tcb, TlsRegion};
use twizzler_abi::object::{ObjID, MAX_SIZE, NULLPAGE_SIZE};

mod gates {
    include! {"../../monitor/secapi/gates.rs"}
}

pub use gates::*;
use twizzler_runtime_api::LibraryId;

/// Shared data between the monitor and a compartment runtime. Written to by the monitor, and
/// read-only from the compartment.
#[repr(C)]
pub struct SharedCompConfig {
    /// The security context that this compartment derives from. Read-only, will not be
    /// overwritten.
    pub sctx: ObjID,
    // Pointer to the current TLS template. Read-only by compartment, writable by monitor.
    tls_template: AtomicPtr<TlsTemplateInfo>,
    /// The root library ID for this compartment. May be None if no libraries have been loaded.
    pub root_library_id: Option<LibraryId>,
}

struct CompConfigFinder {
    config: *const SharedCompConfig,
}

// Safety: the compartment config address is stable over the life of the compartment and doesn't
// change after init.
unsafe impl Sync for CompConfigFinder {}
unsafe impl Send for CompConfigFinder {}

static COMP_CONFIG: OnceLock<CompConfigFinder> = OnceLock::new();

/// Get a reference to this compartment's [SharedCompConfig].
pub fn get_comp_config() -> &'static SharedCompConfig {
    unsafe {
        COMP_CONFIG
            .get_or_init(|| CompConfigFinder {
                config: monitor_rt_get_comp_config().unwrap() as *const _,
            })
            .config
            .as_ref()
            .unwrap()
    }
}

/// Tries to set the comp config pointer. May fail, as this can only be set once.
/// The comp config pointer is automatically determined if [get_comp_config] is called
/// without comp config being set, by cross-compartment call into monitor.
pub fn set_comp_config(cfg: &'static SharedCompConfig) -> Result<(), ()> {
    COMP_CONFIG
        .set(CompConfigFinder { config: cfg })
        .map_err(|_| ())
}

/// Information about a monitor-generated TLS template.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct TlsTemplateInfo {
    pub gen: u64,
    pub layout: Layout,
    pub alloc_base: NonNull<u8>,
    pub tp_offset: usize,
    pub dtv_offset: usize,
    pub num_dtv_entries: usize,
    pub module_top_offset: usize,
}

impl From<TlsRegion> for TlsTemplateInfo {
    fn from(value: TlsRegion) -> Self {
        let offset = |ptr: NonNull<u8>| -> usize {
            unsafe {
                ptr.as_ptr()
                    .byte_offset_from(value.alloc_base.as_ptr())
                    .try_into()
                    .unwrap()
            }
        };
        Self {
            gen: value.gen,
            layout: value.layout,
            alloc_base: value.alloc_base,
            num_dtv_entries: value.num_dtv_entries,
            tp_offset: offset(value.thread_pointer),
            dtv_offset: offset(value.dtv.cast()),
            module_top_offset: offset(value.module_top),
        }
    }
}

impl TlsTemplateInfo {
    /// Initialize a newly allocated memory region with a TLS template and TCB data.
    ///
    /// # Safety
    /// The new pointer must point to a memory region that meets the requirements in self.layout.
    pub unsafe fn init_new_tls_region<T>(&self, new: *mut u8, tcb_data: T) -> *mut Tcb<T> {
        assert!(new.is_aligned_to(self.layout.align()));
        // Step 1: copy the template to the new memory.
        core::ptr::copy_nonoverlapping(self.alloc_base.as_ptr(), new, self.layout.size());

        let tcb = new.add(self.tp_offset) as *mut Tcb<T>;
        let dtv_ptr = new.add(self.dtv_offset) as *mut *mut u8;
        let dtv = core::slice::from_raw_parts_mut(dtv_ptr, self.num_dtv_entries);

        // Step 2a: "relocate" the pointers inside the DTV. First entry is the gen count, so skip
        // that.
        for entry in dtv.iter_mut().skip(1) {
            let offset = (*entry).byte_offset_from(self.alloc_base.as_ptr());
            *entry = new.byte_offset(offset);
        }

        // Step 2b: DTV[0] holds the TLS generation ID.
        let dtv_0 = dtv_ptr as *mut u64;
        *dtv_0 = self.gen;

        // Step 3: Update the TCB data, including pointer to DTV and self.
        {
            let tcb = tcb.as_mut().unwrap();
            tcb.dtv = dtv_ptr as *const usize;
            tcb.self_ptr = tcb;
            tcb.runtime_data = tcb_data;
        }

        tcb
    }
}

impl SharedCompConfig {
    pub fn new(sctx: ObjID, tls_template: *mut TlsTemplateInfo) -> Self {
        Self {
            sctx,
            tls_template: AtomicPtr::new(tls_template),
            root_library_id: None,
        }
    }

    /// Set the current TLS template for a compartment. Only the monitor can call this.
    pub fn set_tls_template(&self, ptr: *mut TlsTemplateInfo) {
        self.tls_template.store(ptr, Ordering::SeqCst);
    }

    /// Get the current TLS template for the compartment.
    pub fn get_tls_template(&self) -> *const TlsTemplateInfo {
        self.tls_template.load(Ordering::SeqCst)
    }
}

pub use gates::LibraryInfo;

/// Contains raw mapping addresses, for use when translating to object handles for the runtime.
#[derive(Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
pub struct MappedObjectAddrs {
    pub slot: usize,
    pub start: usize,
    pub meta: usize,
}

impl MappedObjectAddrs {
    pub fn new(slot: usize) -> Self {
        Self {
            start: slot * MAX_SIZE,
            meta: (slot + 1) * MAX_SIZE - NULLPAGE_SIZE,
            slot,
        }
    }
}
