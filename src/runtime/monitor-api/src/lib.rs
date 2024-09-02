//! This crate exists to break a circular dependency between twz-rt and monitor. We use extern
//! symbols so that we can just call into the monitor without having to have it as an explicit
//! dependency.

#![feature(naked_functions)]
#![feature(pointer_byte_offsets)]
#![feature(pointer_is_aligned)]
#![feature(result_flattening)]
#![feature(thread_local)]
use std::{
    alloc::Layout,
    marker::PhantomData,
    ptr::NonNull,
    sync::{
        atomic::{AtomicPtr, Ordering},
        OnceLock,
    },
};

use dynlink::tls::{Tcb, TlsRegion};
use secgate::util::{Descriptor, Handle};
use twizzler_abi::object::{ObjID, MAX_SIZE, NULLPAGE_SIZE};

mod gates {
    include! {"../../monitor/secapi/gates.rs"}
}

pub use gates::*;
use twizzler_runtime_api::{AddrRange, DlPhdrInfo, LibraryId};

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
#[derive(Debug)]
pub struct LibraryInfo<'a> {
    /// The library's name
    pub name: String,
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
    _pd: PhantomData<&'a ()>,
    internal_name: Vec<u8>,
}

impl<'a> LibraryInfo<'a> {
    fn from_raw(raw: LibraryInfoRaw) -> Self {
        let name = lazy_sb::read_bytes_from_sb(raw.name_len);
        let mut this = Self {
            name: lazy_sb::read_string_from_sb(raw.name_len),
            compartment_id: raw.compartment_id,
            objid: raw.objid,
            range: raw.range,
            dl_info: raw.dl_info,
            slot: raw.slot,
            _pd: PhantomData,
            internal_name: name,
        };
        this.dl_info.name = this.internal_name.as_ptr();
        this
    }
}

/// A handle to a loaded library. On drop, the library may unload.
#[derive(Debug)]
pub struct LibraryHandle {
    desc: Descriptor,
}

impl LibraryHandle {
    /// Get the library info.
    pub fn info(&self) -> LibraryInfo<'_> {
        LibraryInfo::from_raw(
            gates::monitor_rt_get_library_info(self.desc)
                .ok()
                .flatten()
                .unwrap(),
        )
    }

    /// Get the descriptor for this handle.
    pub fn desc(&self) -> Descriptor {
        self.desc
    }
}

/// A builder-type for loading libraries.
pub struct LibraryLoader<'a> {
    id: ObjID,
    comp: Option<&'a CompartmentHandle>,
}

impl<'a> LibraryLoader<'a> {
    /// Make a new LibraryLoader.
    pub fn new(id: ObjID) -> Self {
        Self { id, comp: None }
    }

    /// Load the library in the given compartment.
    pub fn in_compartment(&'a mut self, comp: &'a CompartmentHandle) -> &'a mut Self {
        self.comp = Some(comp);
        self
    }

    /// Load the library.
    pub fn load(&self) -> Result<LibraryHandle, gates::LoadLibraryError> {
        let desc: Descriptor =
            gates::monitor_rt_load_library(self.comp.map(|comp| comp.desc).flatten(), self.id)
                .ok()
                .ok_or(gates::LoadLibraryError::Unknown)
                .flatten()?;
        Ok(LibraryHandle { desc })
    }
}

/// A compartment handle. On drop, the compartment may be unloaded.
pub struct CompartmentHandle {
    desc: Option<Descriptor>,
}

impl CompartmentHandle {
    /// Get the compartment info.
    pub fn info(&self) -> CompartmentInfo<'_> {
        CompartmentInfo::from_raw(
            gates::monitor_rt_get_compartment_info(self.desc)
                .ok()
                .flatten()
                .unwrap(),
        )
    }

    /// Get the descriptor for this handle, or None if the handle refers to the current compartment.
    pub fn desc(&self) -> Option<Descriptor> {
        self.desc
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

    /// Load the compartment.
    pub fn load(&self) -> Result<CompartmentHandle, gates::LoadCompartmentError> {
        let desc = gates::monitor_rt_load_compartment(self.id)
            .ok()
            .ok_or(gates::LoadCompartmentError::Unknown)
            .flatten()?;
        Ok(CompartmentHandle { desc: Some(desc) })
    }
}

impl Handle for CompartmentHandle {
    type OpenError = ();

    type OpenInfo = ObjID;

    fn open(info: Self::OpenInfo) -> Result<Self, Self::OpenError>
    where
        Self: Sized,
    {
        let desc = gates::monitor_rt_get_compartment_handle(info)
            .ok()
            .flatten()
            .ok_or(())?;
        Ok(CompartmentHandle { desc: Some(desc) })
    }

    fn release(&mut self) {
        if let Some(desc) = self.desc {
            let _ = gates::monitor_rt_drop_compartment_handle(desc).ok();
        }
    }
}

impl Handle for LibraryHandle {
    type OpenError = ();

    type OpenInfo = (Option<Descriptor>, usize);

    fn open(info: Self::OpenInfo) -> Result<Self, Self::OpenError>
    where
        Self: Sized,
    {
        let desc = gates::monitor_rt_get_library_handle(info.0, info.1)
            .ok()
            .flatten()
            .ok_or(())?;
        Ok(LibraryHandle { desc })
    }

    fn release(&mut self) {
        let _ = gates::monitor_rt_drop_library_handle(self.desc).ok();
    }
}

impl Drop for CompartmentHandle {
    fn drop(&mut self) {
        self.release();
    }
}

impl Drop for LibraryHandle {
    fn drop(&mut self) {
        self.release()
    }
}

/// Information about a compartment.
#[derive(Clone, Debug)]
pub struct CompartmentInfo<'a> {
    /// The name of the compartment.
    pub name: String,
    /// The instance ID.
    pub id: ObjID,
    /// The security context.
    pub sctx: ObjID,
    /// The compartment flags and status.
    pub flags: CompartmentFlags,
    _pd: PhantomData<&'a ()>,
}

impl<'a> CompartmentInfo<'a> {
    fn from_raw(raw: gates::CompartmentInfo) -> Self {
        Self {
            name: lazy_sb::read_string_from_sb(raw.name_len),
            id: raw.id,
            sctx: raw.sctx,
            flags: CompartmentFlags::from_bits_truncate(raw.flags),
            _pd: PhantomData,
        }
    }
}

impl CompartmentHandle {
    /// Get a handle to the current compartment.
    pub fn current() -> Self {
        Self { desc: None }
    }

    /// Get an iterator over this compartment's dependencies.
    pub fn deps(&self) -> CompartmentDepsIter {
        CompartmentDepsIter::new(self)
    }

    /// Get the root library for this compartment.
    pub fn root(&self) -> LibraryHandle {
        self.libs().next().unwrap()
    }

    /// Get an iterator over the libraries for this compartment.
    pub fn libs(&self) -> LibraryIter<'_> {
        LibraryIter::new(self)
    }
}

/// An iterator over libraries in a compartment.
pub struct LibraryIter<'a> {
    n: usize,
    comp: &'a CompartmentHandle,
}

impl<'a> LibraryIter<'a> {
    fn new(comp: &'a CompartmentHandle) -> Self {
        Self { n: 0, comp }
    }
}

impl<'a> Iterator for LibraryIter<'a> {
    type Item = LibraryHandle;

    fn next(&mut self) -> Option<Self::Item> {
        let handle = LibraryHandle::open((self.comp.desc, self.n)).ok();
        if handle.is_some() {
            self.n += 1;
        }
        handle
    }
}

/// An iterator over a compartmen's dependencies.
pub struct CompartmentDepsIter<'a> {
    n: usize,
    comp: &'a CompartmentHandle,
}

impl<'a> CompartmentDepsIter<'a> {
    fn new(comp: &'a CompartmentHandle) -> Self {
        Self { n: 0, comp }
    }
}

impl<'a> Iterator for CompartmentDepsIter<'a> {
    type Item = CompartmentHandle;

    fn next(&mut self) -> Option<Self::Item> {
        let desc = gates::monitor_rt_get_compartment_deps(self.comp.desc, self.n)
            .ok()
            .flatten()?;
        self.n += 1;
        Some(CompartmentHandle { desc: Some(desc) })
    }

    fn nth(&mut self, n: usize) -> Option<Self::Item> {
        self.n += n;
        self.next()
    }
}

bitflags::bitflags! {
    /// Compartment state flags.
    #[derive(Clone, Debug, Copy, PartialEq, PartialOrd, Ord, Eq, Hash)]
    pub struct CompartmentFlags : u64 {
        /// Compartment is ready (libraries relocated and constructors run).
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

mod lazy_sb {
    //! A per-thread per-compartment simple buffer used for transferring strings between
    //! compartments and the monitor. This is necessary because the monitor runs at too low of a
    //! level for us to use nice shared memory techniques. This is simpler and more secure.
    use std::cell::OnceCell;

    use secgate::util::SimpleBuffer;
    use twizzler_runtime_api::MapFlags;

    struct LazyThreadSimpleBuffer {
        sb: OnceCell<SimpleBuffer>,
    }

    impl LazyThreadSimpleBuffer {
        const fn new() -> Self {
            Self {
                sb: OnceCell::new(),
            }
        }

        fn init() -> SimpleBuffer {
            let id = super::gates::monitor_rt_get_thread_simple_buffer()
                .ok()
                .flatten()
                .expect("failed to get per-thread monitor simple buffer");
            let oh = twizzler_runtime_api::get_runtime()
                .map_object(id, MapFlags::READ | MapFlags::WRITE)
                .unwrap();
            SimpleBuffer::new(oh)
        }

        fn read(&mut self, buf: &mut [u8]) -> usize {
            let sb = self.sb.get_or_init(|| Self::init());
            sb.read(buf)
        }

        fn _write(&mut self, buf: &[u8]) -> usize {
            if self.sb.get().is_none() {
                // Unwrap-Ok: we know it's empty.
                self.sb.set(Self::init()).unwrap();
            }
            let sb = self.sb.get_mut().unwrap();
            sb.write(buf)
        }
    }

    #[thread_local]
    static mut LAZY_SB: LazyThreadSimpleBuffer = LazyThreadSimpleBuffer::new();

    pub(super) fn read_string_from_sb(len: usize) -> String {
        let mut buf = vec![0u8; len];
        // Safety: this is per thread, and we only ever create the reference here or in the other
        // read function below.
        let len = unsafe { LAZY_SB.read(&mut buf) };
        String::from_utf8_lossy(&buf[0..len]).to_string()
    }

    pub(super) fn read_bytes_from_sb(len: usize) -> Vec<u8> {
        let mut buf = vec![0u8; len];
        // Safety: see above.
        let len = unsafe { LAZY_SB.read(&mut buf) };
        buf.truncate(len);
        buf
    }
}
