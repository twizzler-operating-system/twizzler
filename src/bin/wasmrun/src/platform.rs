//! Wasmtime custom platform callbacks for Twizzler.
//!
//! When wasmtime is built with the `custom-virtual-memory` feature, it calls
//! these extern functions instead of using OS-level mmap/mprotect.
//!
//! Since Twizzler enforces NX on heap pages, we cannot use the standard
//! allocator for JIT code. Instead, we use Twizzler's object system to create
//! memory with READ+WRITE+EXEC permissions. All wasmtime allocations go
//! through this path since wasmtime may first allocate as RW and later
//! mprotect to RX — and Twizzler has no mprotect equivalent.
//!
//! Reference: wasmtime's `crates/wasmtime/src/runtime/vm/sys/custom/capi.rs`

use std::collections::HashMap;
use std::ffi::c_void;
use std::cell::Cell;
use std::sync::Mutex;

use twizzler_abi::object::Protections;
use twizzler_abi::syscall::{
    sys_object_create, BackingType, LifetimeType, ObjectCreate, ObjectCreateFlags,
};
use twizzler_rt_abi::object::{twz_rt_map_object, MapFlags, ObjectHandle};

const PAGE_SIZE: usize = 4096;

/// Track all object-backed allocations so we can keep handles alive (preventing
/// unmap) and clean up on munmap.
static ALLOCATIONS: Mutex<Option<HashMap<usize, ObjectHandle>>> = Mutex::new(None);

fn track_handle(ptr: *mut u8, handle: ObjectHandle) {
    let mut guard = ALLOCATIONS.lock().unwrap();
    let map = guard.get_or_insert_with(HashMap::new);
    map.insert(ptr as usize, handle);
}

fn release_handle(ptr: *mut u8) {
    let mut guard = ALLOCATIONS.lock().unwrap();
    if let Some(map) = guard.as_mut() {
        // Dropping the ObjectHandle triggers its refcount decrement and
        // eventual unmap when it hits zero.
        map.remove(&(ptr as usize));
    }
}

// ---------------------------------------------------------------------------
// Virtual memory
// ---------------------------------------------------------------------------

/// Allocate `size` bytes of zeroed memory. We create a Twizzler object with
/// RWX permissions so that JIT-compiled code can execute from it.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn wasmtime_mmap_new(
    size: usize,
    _prot_flags: u32,
    ret: &mut *mut u8,
) -> i32 {
    // Create a volatile object with full RWX default protections.
    let spec = ObjectCreate::new(
        BackingType::Normal,
        LifetimeType::Volatile,
        None,
        ObjectCreateFlags::empty(),
        Protections::READ | Protections::WRITE | Protections::EXEC,
    );

    let id = match sys_object_create(spec, &[], &[]) {
        Ok(id) => id,
        Err(_) => return -1,
    };

    // Map it with READ+WRITE+EXEC into our address space.
    let handle = match twz_rt_map_object(
        id.into(),
        MapFlags::READ | MapFlags::WRITE | MapFlags::EXEC | MapFlags::NO_NULLPAGE,
    ) {
        Ok(h) => h,
        Err(_) => return -1,
    };

    let ptr = handle.start();

    // Zero the requested region (the kernel may or may not zero object pages).
    unsafe {
        core::ptr::write_bytes(ptr, 0, size);
    }

    // Keep the handle alive so the mapping persists.
    track_handle(ptr, handle);

    *ret = ptr;
    0
}

/// Remap: in Linux this is mmap(MAP_FIXED) to replace pages with fresh zeroed
/// anonymous memory. We just zero the region since our object already has the
/// needed permissions.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn wasmtime_mmap_remap(
    addr: *mut u8,
    size: usize,
    _prot_flags: u32,
) -> i32 {
    unsafe {
        core::ptr::write_bytes(addr, 0, size);
    }
    0
}

/// Free a region. Drop the ObjectHandle which decrements its refcount and
/// eventually unmaps the object.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn wasmtime_munmap(ptr: *mut u8, _size: usize) -> i32 {
    release_handle(ptr);
    0
}

/// Change memory protection. This is a no-op because all our allocations
/// already have RWX permissions from creation.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn wasmtime_mprotect(
    _ptr: *mut u8,
    _size: usize,
    _prot_flags: u32,
) -> i32 {
    0
}

/// Return the system page size.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn wasmtime_page_size() -> usize {
    PAGE_SIZE
}

// ---------------------------------------------------------------------------
// Memory images (not supported — return NULL so wasmtime uses fallback)
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn wasmtime_memory_image_new(
    _ptr: *const u8,
    _len: usize,
    ret: &mut *mut c_void,
) -> i32 {
    *ret = core::ptr::null_mut();
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn wasmtime_memory_image_map_at(
    _image: *mut c_void,
    _addr: *mut u8,
    _len: usize,
) -> i32 {
    -1
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn wasmtime_memory_image_free(_image: *mut c_void) {}

// ---------------------------------------------------------------------------
// Thread-local storage (per-thread for correct multi-thread support)
// ---------------------------------------------------------------------------

thread_local! {
    static WASMTIME_TLS: Cell<*mut u8> = const { Cell::new(core::ptr::null_mut()) };
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn wasmtime_tls_get() -> *mut u8 {
    WASMTIME_TLS.with(|c| c.get())
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn wasmtime_tls_set(ptr: *mut u8) {
    WASMTIME_TLS.with(|c| c.set(ptr));
}
