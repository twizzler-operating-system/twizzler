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
use twizzler_runtime_api::{AddrRange, DlPhdrInfo, LibraryId, MapFlags, ObjectHandle};

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

pub use gates::LibraryInfo as LibraryInfoRaw;

/// Contains information about a library loaded into the address space.
#[derive(Clone, Debug)]
pub struct LibraryInfo {
    /// The library's name
    pub name: String,
    /// Global library ID
    pub id: LibraryId,
    /// The compartment of the library
    pub compartment_id: ObjID,
    /// The object ID that the library was loaded from
    pub objid: ObjID,
    /// The address range the library was loaded to
    pub range: AddrRange,
    /// The DlPhdrInfo for this library
    pub dl_info: DlPhdrInfo,
    /// The slot of the library text.
    pub slot: usize,
}

impl LibraryInfo {
    fn from_raw(raw: LibraryInfoRaw) -> Self {
        Self {
            name: todo!(),
            id: raw.id,
            compartment_id: raw.compartment_id,
            objid: raw.objid,
            range: raw.range,
            dl_info: raw.dl_info,
            slot: raw.slot,
        }
    }
}

/// A handle to a loaded library. On drop, the library may unload.
pub struct LibraryHandle {
    handle: ObjectHandle,
    id: LibraryId,
}

impl LibraryHandle {
    /// Get the library info.
    pub fn info(&self) -> LibraryInfo {
        LibraryInfo::from_raw(
            gates::monitor_rt_get_library_info(0.into(), self.id.0 as usize)
                .ok()
                .flatten()
                .unwrap(),
        )
    }

    /// Get the handle for this library.
    pub fn handle(&self) -> &ObjectHandle {
        &self.handle
    }

    /// Get the ID of this library.
    pub fn library_id(&self) -> LibraryId {
        self.id
    }
}

/// A builder-type for loading libraries.
pub struct LibraryLoader {
    id: ObjID,
}

impl LibraryLoader {
    /// Make a new LibraryLoader.
    pub fn new(id: ObjID) -> Self {
        Self { id }
    }

    // TODO: err
    /// Load the library.
    pub fn load(&self) -> Result<LibraryHandle, ()> {
        let info: LibraryInfoRaw = gates::monitor_rt_load_library(self.id)
            .ok()
            .flatten()
            .ok_or(())?;
        let runtime = twizzler_runtime_api::get_runtime();
        Ok(LibraryHandle {
            handle: runtime.map_object(info.objid, MapFlags::READ).unwrap(),
            id: info.id,
        })
    }
}

/// A compartment handle. On drop, the compartment may be unloaded.
pub struct CompartmentHandle {
    id: ObjID,
}

impl CompartmentHandle {
    /// Get the compartment info.
    pub fn info(&self) -> CompartmentInfo {
        CompartmentInfo::get(self.id).unwrap()
    }
}

/// A builder-type for loading compartments.
pub struct CompartmentLoader {
    id: ObjID,
}

impl CompartmentLoader {
    /// Make a new compartment loader.
    pub fn new(id: ObjID) -> Self {
        Self { id }
    }

    // TODO: err
    /// Load the compartment.
    pub fn load(&self) -> Result<CompartmentHandle, ()> {
        let info: gates::CompartmentInfo = gates::monitor_rt_load_compartment(self.id)
            .ok()
            .flatten()
            .ok_or(())?;
        Ok(CompartmentHandle { id: info.id })
    }
}

impl Drop for CompartmentHandle {
    fn drop(&mut self) {
        let _ = gates::monitor_rt_drop_compartment_handle(self.id)
            .ok()
            .flatten();
    }
}

impl Drop for LibraryHandle {
    fn drop(&mut self) {
        let _ = gates::monitor_rt_drop_library_handle(self.id)
            .ok()
            .flatten();
    }
}

/// Information about a compartment.
pub struct CompartmentInfo {
    /// The name of the compartment.
    pub name: String,
    /// The instance ID.
    pub id: ObjID,
    /// The security context.
    pub sctx: ObjID,
    /// The compartment flags and status.
    pub flags: CompartmentFlags,
}

impl CompartmentInfo {
    fn from_raw(raw: gates::CompartmentInfo) -> Self {
        Self {
            name: todo!(),
            id: raw.id,
            sctx: raw.sctx,
            flags: CompartmentFlags::from_bits_truncate(raw.flags),
        }
    }
    /// Get compartment info for a specified ID. A value of 0 will return infomation about the
    /// current compartment.
    pub fn get(id: ObjID) -> Option<Self> {
        let raw = gates::monitor_rt_get_compartment_info(id).ok().flatten()?;
        Some(Self::from_raw(raw))
    }

    /// Get the current compartment's info.
    pub fn current() -> Self {
        Self::get(0.into()).unwrap()
    }

    /// Get an iterator over this compartment's dependencies.
    pub fn deps(&self) -> CompartmentDepsIter {
        CompartmentDepsIter::new(self.id)
    }

    /// Get the root library for this compartment.
    pub fn root(&self) -> LibraryInfo {
        self.libs().next().unwrap()
    }

    /// Get an iterator over the libraries for this compartment.
    pub fn libs(&self) -> LibraryIter {
        LibraryIter::new()
    }
}

/// An iterator over libraries in a compartment.
pub struct LibraryIter {
    n: usize,
}

impl LibraryIter {
    pub fn new() -> Self {
        Self { n: 0 }
    }
}

impl Iterator for LibraryIter {
    type Item = LibraryInfo;

    fn next(&mut self) -> Option<Self::Item> {
        let info = gates::monitor_rt_get_library_info(0.into(), self.n)
            .ok()
            .flatten()?;
        self.n += 1;
        Some(LibraryInfo::from_raw(info))
    }
}

/// An iterator over a compartmen's dependencies.
pub struct CompartmentDepsIter {
    n: usize,
    id: ObjID,
}

impl CompartmentDepsIter {
    pub fn new(id: ObjID) -> Self {
        Self { n: 0, id }
    }
}

impl Iterator for CompartmentDepsIter {
    type Item = CompartmentInfo;

    fn next(&mut self) -> Option<Self::Item> {
        let info = gates::monitor_rt_get_compartment_deps(self.id, self.n)
            .ok()
            .flatten()?;
        self.n += 1;
        Some(CompartmentInfo::from_raw(info))
    }

    fn nth(&mut self, n: usize) -> Option<Self::Item> {
        self.n += n;
        self.next()
    }
}

bitflags::bitflags! {
    /// Compartment state flags.
    pub struct CompartmentFlags : u32 {
        const READY = 0x1;
    }
}

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
