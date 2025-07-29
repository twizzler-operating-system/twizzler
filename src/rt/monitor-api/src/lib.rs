//! This crate exists to break a circular dependency between twz-rt and monitor. We use extern
//! symbols so that we can just call into the monitor without having to have it as an explicit
//! dependency.

#![feature(naked_functions)]
#![feature(linkage)]
#![feature(result_flattening)]
#![feature(thread_local)]
#![feature(pointer_is_aligned_to)]
#![feature(tuple_trait)]
use std::{
    alloc::Layout,
    cell::UnsafeCell,
    marker::{PhantomData, Tuple},
    ptr::NonNull,
    sync::{
        atomic::{AtomicPtr, AtomicU32, Ordering},
        OnceLock,
    },
};

pub use dynlink::{
    context::NewCompartmentFlags,
    tls::{Tcb, TlsRegion},
};
use secgate::{
    util::{Descriptor, Handle},
    Crossing, DynamicSecGate,
};
use twizzler_abi::object::{ObjID, MAX_SIZE, NULLPAGE_SIZE};

#[allow(unused_imports, unused_variables, unexpected_cfgs)]
mod gates {
    include! {"../../monitor/secapi/gates.rs"}
}

pub use gates::*;
use twizzler_rt_abi::{
    debug::{DlPhdrInfo, LinkMap, LoadedImageId},
    error::{ArgumentError, TwzError},
};

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
    pub root_library_id: Option<LoadedImageId>,
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

// Safety: this type is designed to pass pointers to thread-local memory across boundaries, so we
// assert this is safe.
unsafe impl Send for TlsTemplateInfo {}
unsafe impl Sync for TlsTemplateInfo {}

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
    /// The start address of range the library was loaded to
    pub start: *const u8,
    /// Length of range library was loaded to
    pub len: usize,
    /// The DlPhdrInfo for this library
    pub dl_info: DlPhdrInfo,
    /// The link_map structure for this library
    pub link_map: LinkMap,
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
            start: raw.start,
            len: raw.len,
            dl_info: raw.dl_info,
            slot: raw.slot,
            _pd: PhantomData,
            internal_name: name,
            link_map: raw.link_map,
        };
        this.dl_info.name = this.internal_name.as_ptr().cast();
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
        LibraryInfo::from_raw(gates::monitor_rt_get_library_info(self.desc).unwrap())
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
    pub fn load(&self) -> Result<LibraryHandle, TwzError> {
        let desc: Descriptor =
            gates::monitor_rt_load_library(self.comp.map(|comp| comp.desc).flatten(), self.id)?;
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
        CompartmentInfo::from_raw(gates::monitor_rt_get_compartment_info(self.desc).unwrap())
    }

    /// Get the descriptor for this handle, or None if the handle refers to the current compartment.
    pub fn desc(&self) -> Option<Descriptor> {
        self.desc
    }

    pub unsafe fn dynamic_gate<A: Tuple + Crossing + Copy, R: Crossing + Copy>(
        &self,
        name: &str,
    ) -> Result<DynamicSecGate<'_, A, R>, TwzError> {
        let name_len = lazy_sb::write_bytes_to_sb(name.as_bytes());
        let address = gates::monitor_rt_compartment_dynamic_gate(self.desc, name_len)?;
        Ok(DynamicSecGate::new(address))
    }
}

/// A builder-type for loading compartments.
pub struct CompartmentLoader {
    name: String,
    args: Vec<String>,
    env: Option<Vec<String>>,
    flags: NewCompartmentFlags,
}

impl CompartmentLoader {
    /// Make a new compartment loader.
    pub fn new(
        compname: impl ToString,
        libname: impl ToString,
        flags: NewCompartmentFlags,
    ) -> Self {
        Self {
            name: format!("{}::{}", compname.to_string(), libname.to_string()),
            flags,
            env: None,
            args: vec![],
        }
    }

    /// Append args to this compartment.
    pub fn args<S: ToString>(&mut self, args: impl IntoIterator<Item = S>) -> &mut Self {
        for arg in args.into_iter() {
            self.args.push(arg.to_string())
        }
        self
    }

    /// Set the environment for the compartment
    pub fn env<S: ToString>(&mut self, env: impl IntoIterator<Item = S>) -> &mut Self {
        self.env = Some(env.into_iter().map(|s| s.to_string()).collect());
        self
    }

    /// Load the compartment.
    pub fn load(&self) -> Result<CompartmentHandle, TwzError> {
        fn get_current_env() -> Vec<String> {
            std::env::vars()
                .map(|(var, val)| format!("{}={}", var, val))
                .collect()
        }
        let name_len = self.name.as_bytes().len();
        let args_len = self
            .args
            .iter()
            .fold(0, |acc, arg| acc + arg.as_bytes().len() + 1);
        let env = self.env.clone().unwrap_or_else(|| get_current_env());
        let envs_len = env
            .iter()
            .fold(0, |acc, arg| acc + arg.as_bytes().len() + 1);
        let mut bytes = self.name.as_bytes().to_vec();
        for arg in &self.args {
            bytes.extend_from_slice(arg.as_bytes());
            bytes.push(0);
        }
        for env in env {
            bytes.extend_from_slice(env.as_bytes());
            bytes.push(0);
        }
        let len = lazy_sb::write_bytes_to_sb(&bytes);
        if len < envs_len + args_len + name_len {
            return Err(ArgumentError::InvalidArgument.into());
        }
        let desc = gates::monitor_rt_load_compartment(
            name_len as u64,
            args_len as u64,
            envs_len as u64,
            self.flags.bits(),
        )?;
        Ok(CompartmentHandle { desc: Some(desc) })
    }
}

impl Handle for CompartmentHandle {
    type OpenError = TwzError;

    type OpenInfo = ObjID;

    fn open(info: Self::OpenInfo) -> Result<Self, Self::OpenError>
    where
        Self: Sized,
    {
        let desc = gates::monitor_rt_get_compartment_handle(info)?;
        Ok(CompartmentHandle { desc: Some(desc) })
    }

    fn release(&mut self) {
        if let Some(desc) = self.desc {
            let _ = gates::monitor_rt_drop_compartment_handle(desc);
        }
    }
}

impl Drop for CompartmentHandle {
    fn drop(&mut self) {
        self.release();
    }
}

impl Handle for LibraryHandle {
    type OpenError = TwzError;

    type OpenInfo = (Option<Descriptor>, usize);

    fn open(info: Self::OpenInfo) -> Result<Self, Self::OpenError>
    where
        Self: Sized,
    {
        let desc = gates::monitor_rt_get_library_handle(info.0, info.1)?;
        Ok(LibraryHandle { desc })
    }

    fn release(&mut self) {
        let _ = gates::monitor_rt_drop_library_handle(self.desc);
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
    /// Number of libraries
    pub nr_libs: usize,
    _pd: PhantomData<&'a ()>,
}

impl<'a> CompartmentInfo<'a> {
    fn from_raw(raw: gates::CompartmentInfo) -> Self {
        Self {
            name: lazy_sb::read_string_from_sb(raw.name_len),
            id: raw.id,
            sctx: raw.sctx,
            flags: CompartmentFlags::from_bits_truncate(raw.flags),
            nr_libs: raw.nr_libs,
            _pd: PhantomData,
        }
    }
}

impl CompartmentHandle {
    /// Get a handle to the current compartment.
    pub fn current() -> Self {
        Self { desc: None }
    }

    /// Lookup a compartment by name.
    pub fn lookup(name: impl AsRef<str>) -> Result<Self, TwzError> {
        let name_len = lazy_sb::write_bytes_to_sb(name.as_ref().as_bytes());
        Ok(Self {
            desc: Some(gates::monitor_rt_lookup_compartment(name_len)?),
        })
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

    /// Get an iterator over the libraries for this compartment.
    pub fn threads(&self) -> CompartmentThreadsIter<'_> {
        CompartmentThreadsIter::new(self)
    }

    pub fn wait(&self, flags: CompartmentFlags) -> CompartmentFlags {
        CompartmentFlags::from_bits_truncate(
            gates::monitor_rt_compartment_wait(self.desc(), flags.bits()).unwrap(),
        )
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

/// An iterator over a compartment's dependencies.
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
        let desc = gates::monitor_rt_get_compartment_deps(self.comp.desc, self.n).ok()?;
        self.n += 1;
        Some(CompartmentHandle { desc: Some(desc) })
    }

    fn nth(&mut self, n: usize) -> Option<Self::Item> {
        self.n += n;
        self.next()
    }
}

/// An iterator over a compartment's threads.
pub struct CompartmentThreadsIter<'a> {
    n: usize,
    comp: &'a CompartmentHandle,
}

impl<'a> CompartmentThreadsIter<'a> {
    fn new(comp: &'a CompartmentHandle) -> Self {
        Self { n: 0, comp }
    }
}

impl<'a> Iterator for CompartmentThreadsIter<'a> {
    type Item = ThreadInfo;

    fn next(&mut self) -> Option<Self::Item> {
        let info = gates::monitor_rt_get_compartment_thread(self.comp.desc, self.n).ok()?;
        self.n += 1;
        Some(info)
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
        /// Compartment is ready (loaded, reloacated, runtime started and ctors run).
        const READY = 0x1;
        /// Compartment is a binary, not a library.
        const IS_BINARY = 0x2;
        /// Compartment runtime thread may exit.
        const THREAD_CAN_EXIT = 0x4;
        /// Compartment thread has been started once.
        const STARTED = 0x8;
        /// Compartment destructors have run.
        const DESTRUCTED = 0x10;
        /// Compartment thread has exited.
        const EXITED = 0x20;
    }
}

/// Contains raw mapping addresses, for use when translating to object handles for the runtime.
#[derive(Copy, Clone, PartialEq, PartialOrd, Ord, Eq, Debug)]
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

/// Get stats from the monitor
pub fn stats() -> Option<gates::MonitorStats> {
    gates::monitor_rt_stats().ok()
}

mod lazy_sb {
    //! A per-thread per-compartment simple buffer used for transferring strings between
    //! compartments and the monitor. This is necessary because the monitor runs at too low of a
    //! level for us to use nice shared memory techniques. This is simpler and more secure.
    use std::cell::{OnceCell, RefCell};

    use secgate::util::SimpleBuffer;
    use twizzler_rt_abi::object::MapFlags;

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
                .expect("failed to get per-thread monitor simple buffer");
            let oh =
                twizzler_rt_abi::object::twz_rt_map_object(id, MapFlags::READ | MapFlags::WRITE)
                    .unwrap();
            SimpleBuffer::new(oh)
        }

        fn read(&mut self, buf: &mut [u8]) -> usize {
            let sb = self.sb.get_or_init(|| Self::init());
            sb.read(buf)
        }

        fn write(&mut self, buf: &[u8]) -> usize {
            if self.sb.get().is_none() {
                // Unwrap-Ok: we know it's empty.
                self.sb.set(Self::init()).unwrap();
            }
            let sb = self.sb.get_mut().unwrap();
            sb.write(buf)
        }
    }

    #[thread_local]
    static LAZY_SB: RefCell<LazyThreadSimpleBuffer> = RefCell::new(LazyThreadSimpleBuffer::new());

    pub(super) fn read_string_from_sb(len: usize) -> String {
        let mut buf = vec![0u8; len];
        let len = LAZY_SB.borrow_mut().read(&mut buf);
        String::from_utf8_lossy(&buf[0..len]).to_string()
    }

    pub(super) fn read_bytes_from_sb(len: usize) -> Vec<u8> {
        let mut buf = vec![0u8; len];
        let len = LAZY_SB.borrow_mut().read(&mut buf);
        buf.truncate(len);
        buf
    }

    pub(super) fn write_bytes_to_sb(buf: &[u8]) -> usize {
        LAZY_SB.borrow_mut().write(buf)
    }
}

pub const THREAD_STARTED: u32 = 1;
pub struct RuntimeThreadControl {
    // Need to keep a lock for the ID, though we don't expect to use it much.
    pub internal_lock: AtomicU32,
    pub flags: AtomicU32,
    pub id: UnsafeCell<u32>,
}

impl Default for RuntimeThreadControl {
    fn default() -> Self {
        Self::new(0)
    }
}

impl RuntimeThreadControl {
    pub const fn new(id: u32) -> Self {
        Self {
            internal_lock: AtomicU32::new(0),
            flags: AtomicU32::new(0),
            id: UnsafeCell::new(id),
        }
    }

    fn write_lock(&self) {
        loop {
            let old = self.internal_lock.fetch_or(1, Ordering::Acquire);
            if old == 0 {
                break;
            }
        }
    }

    fn write_unlock(&self) {
        self.internal_lock.fetch_and(!1, Ordering::Release);
    }

    fn read_lock(&self) {
        loop {
            let old = self.internal_lock.fetch_add(2, Ordering::Acquire);
            // If this happens, something has gone very wrong.
            if old > i32::MAX as u32 {
                twizzler_rt_abi::core::twz_rt_abort();
            }
            if old & 1 == 0 {
                break;
            }
        }
    }

    fn read_unlock(&self) {
        self.internal_lock.fetch_sub(2, Ordering::Release);
    }

    pub fn set_id(&self, id: u32) {
        self.write_lock();
        unsafe {
            *self.id.get().as_mut().unwrap() = id;
        }
        self.write_unlock();
    }

    pub fn id(&self) -> u32 {
        self.read_lock();
        let id = unsafe { *self.id.get().as_ref().unwrap() };
        self.read_unlock();
        id
    }
}

pub fn set_nameroot(root: ObjID) -> Result<(), TwzError> {
    gates::monitor_rt_set_nameroot(root)
}
