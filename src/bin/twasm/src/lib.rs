/* From https://github.com/manenko/cassander/blob/6b1326d5b27c2fbe7176c2b62be0432ac65daa50/src/allocator.rs */

use std::alloc::{alloc_zeroed, realloc, dealloc, Layout};
use std::ffi::c_void;
use std::mem::size_of;
use std::ptr::{
    copy_nonoverlapping,
    from_mut,
    from_ref,
};

use std::sync::atomic::{AtomicPtr,Ordering};

const LAYOUT_DATA_SIZE: usize = size_of::<usize>() + size_of::<usize>();
const DEFAULT_ALIGNMENT: usize = 128;

fn store_layout(layout: Layout, start: *mut u8) -> *mut u8 {
    let size_start = start as *mut usize;
    unsafe { copy_nonoverlapping(from_ref(&layout.size()), size_start, 1) };

    let align_start = unsafe { size_start.add(1) };
    unsafe {
        copy_nonoverlapping(from_ref(&DEFAULT_ALIGNMENT), align_start, 1)
    };
    unsafe { start.add(LAYOUT_DATA_SIZE) }
}

/// Restores the layout from the memory block and returns the layout and the
/// real start of the allocated memory block, i.e. the start of the allocation
/// data.
fn restore_layout(ptr: *const u8) -> (*mut u8, Layout) {
    let layout_start = unsafe { ptr.sub(LAYOUT_DATA_SIZE) };
    let size_start = layout_start as *const usize;
    let mut size = 0usize;
    unsafe { copy_nonoverlapping(size_start, from_mut(&mut size), 1) };

    let align_start = unsafe { size_start.add(1) };
    let mut align = 0usize;
    unsafe { copy_nonoverlapping(align_start, from_mut(&mut align), 1) };

    let layout =
        Layout::from_size_align(size, align).expect("invalid memory layout");

    (layout_start as *mut u8, layout)
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn wasmtime_mmap_new(
    size: usize, 
    _prot_flags: u32, 
    _ret: &mut *mut u8
) -> i32 {
    let Ok(layout) = Layout::from_size_align(size, DEFAULT_ALIGNMENT) else {
      return -1;
    };
    unsafe {
      // always zero memory
      let block_start = alloc_zeroed(layout);
      if block_start.is_null() {
        return -1;
      }

      store_layout(layout, block_start) as *mut ::core::ffi::c_void;
      return 0;
    }
}

 #[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn wasmtime_mmap_remap(
    addr: *mut u8, 
    size: usize, 
    _prot_flags: u32) -> i32 {

    // TODO: handle prot_flags

    let new_size = size + LAYOUT_DATA_SIZE;

    let (block_start, layout) = restore_layout(addr as *const u8);
    let Ok(new_layout) = Layout
      ::from_size_align(new_size, layout.align()) else {
       return -1;
    };

    unsafe {
      let new_block_start = realloc(block_start, layout, new_size)
             .cast();
      if block_start.is_null() {
          return -1;
      }
      store_layout(new_layout, new_block_start) as *mut c_void;
      return 0;
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn wasmtime_munmap(
    ptr: *mut u8, 
    _size: usize) -> i32 {
    let (_, layout) = restore_layout(ptr as *const u8);
    unsafe {
        dealloc(ptr.cast(), layout);
        return 0;
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn wasmtime_page_size() -> usize {
    twizzler_abi::syscall::sys_info().page_size() 
}

/**
 * Indicates that the memory region should be readable.
 */
const WASMTIME_PROT_READ:u32  = 1 << 0;
const WASMTIME_PROT_WRITE:u32 = 1 << 1;
const WASMTIME_PROT_EXEC:u32  = 1 << 2;

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn 
    wasmtime_mprotect(_ptr: *mut u8, _size: usize, prot_flags: u32) -> i32 {
    // TODO: handle prot_flags
    if prot_flags == WASMTIME_PROT_READ 
    || prot_flags == WASMTIME_PROT_WRITE
    || prot_flags == WASMTIME_PROT_EXEC {
        return 0;
    }
    -1
}

// The wasmtime_memory_image APIs are not yet supported.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn 
wasmtime_memory_image_new(
    _ptr: *const u8,
    _len: usize,
    ret: &mut *mut c_void,
) -> i32 {
    *ret = core::ptr::null_mut();
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn 
wasmtime_memory_image_map_at(
    _image: *mut c_void,
    _addr: *mut u8,
    _len: usize,
) -> i32 {
    /* This should never be called because wasmtime_memory_image_new
     * returns NULL */
    panic!("wasmtime_memory_image_map_at");
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn 
wasmtime_memory_image_free(_image: *mut c_void) {
    /* This should never be called because wasmtime_memory_image_new
     * returns NULL */
    panic!("wasmtime_memory_image_free");
}

// Pretend that this platform doesn't have threads where storing in a static is
// ok.

/* Because we only have a single thread in the guest at the moment, we
 * don't need real thread-local storage. */
static FAKE_TLS: AtomicPtr<u8> = AtomicPtr::new(core::ptr::null_mut());

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn 
wasmtime_tls_get() -> *mut u8 {
    FAKE_TLS.load(Ordering::Acquire)
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn 
wasmtime_tls_set(ptr: *mut u8) {
    FAKE_TLS.store(ptr, Ordering::Release)
}

#[macro_use]
extern crate alloc;

use alloc::string::ToString;
use anyhow::Result;
use core::ptr;
use wasmtime::{Engine, Instance, Linker, Module, Store};



/// Entrypoint of this embedding.
///
/// This takes a number of parameters which are the precompiled module AOT
/// images that are run for each of the various tests below. The first parameter
/// is also where to put an error string, if any, if anything fails.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn run(
    error_buf: *mut u8,
    error_size: usize,
    smoke_module: *const u8,
    smoke_size: usize,
    simple_add_module: *const u8,
    simple_add_size: usize,
    simple_host_fn_module: *const u8,
    simple_host_fn_size: usize,
) -> usize {
    unsafe {
        let buf = core::slice::from_raw_parts_mut(error_buf, error_size);
        let smoke = core::slice::from_raw_parts(smoke_module, smoke_size);
        let simple_add = core::slice::from_raw_parts(simple_add_module, simple_add_size);
        let simple_host_fn =
            core::slice::from_raw_parts(simple_host_fn_module, simple_host_fn_size);
        match run_result(smoke, simple_add, simple_host_fn) {
            Ok(()) => 0,
            Err(e) => {
                let msg = format!("{e:?}");
                let len = buf.len().min(msg.len());
                buf[..len].copy_from_slice(&msg.as_bytes()[..len]);
                len
            }
        }
    }
}

fn run_result(
    smoke_module: &[u8],
    simple_add_module: &[u8],
    simple_host_fn_module: &[u8],
) -> Result<()> {
    eprintln!("Running smoke?, {}", "eh");
    smoke(smoke_module)?;
    eprintln!("Running add module?, {}", "eh");
    simple_add(simple_add_module)?;
    eprintln!("Running add host_fn_module?, {}", "eh");
    simple_host_fn(simple_host_fn_module)?;
    eprintln!("OK?, {}", "eh");
    Ok(())
}

fn smoke(module: &[u8]) -> Result<()> {
    let engine = Engine::default();
    let module = match deserialize(&engine, module)? {
        Some(module) => module,
        None => return Ok(()),
    };
    Instance::new(&mut Store::new(&engine, ()), &module, &[])?;
    Ok(())
}

fn simple_add(module: &[u8]) -> Result<()> {
    let engine = Engine::default();
    let module = match deserialize(&engine, module)? {
        Some(module) => module,
        None => return Ok(()),
    };
    let mut store = Store::new(&engine, ());
    let instance = Linker::new(&engine).instantiate(&mut store, &module)?;
    let func = instance.get_typed_func::<(u32, u32), u32>(&mut store, "add")?;
    assert_eq!(func.call(&mut store, (2, 3))?, 5);
    Ok(())
}

fn simple_host_fn(module: &[u8]) -> Result<()> {
    let engine = Engine::default();
    let module = match deserialize(&engine, module)? {
        Some(module) => module,
        None => return Ok(()),
    };
    let mut linker = Linker::<()>::new(&engine);
    linker.func_wrap("host", "multiply", |a: u32, b: u32| a.saturating_mul(b))?;
    let mut store = Store::new(&engine, ());
    let instance = linker.instantiate(&mut store, &module)?;
    let func = instance.get_typed_func::<(u32, u32, u32), u32>(&mut store, "add_and_mul")?;
    assert_eq!(func.call(&mut store, (2, 3, 4))?, 10);
    Ok(())
}

fn deserialize(engine: &Engine, module: &[u8]) -> Result<Option<Module>> {
    // NOTE: deserialize_raw avoids creating a copy of the module code.  See the
    // safety notes before using in your embedding.
    let memory_ptr = ptr::slice_from_raw_parts(module.as_ptr(), module.len());
    let module_memory = ptr::NonNull::new(memory_ptr.cast_mut()).unwrap();
    match unsafe { Module::deserialize_raw(engine, module_memory) } {
        Ok(module) => Ok(Some(module)),
        Err(e) => {
            // Currently if custom signals/virtual memory are disabled then this
            // example is expected to fail to load since loading native code
            // requires virtual memory. In the future this will go away as when
            // signals-based-traps is disabled then that means that the
            // interpreter should be used which should work here.
            if !cfg!(feature = "default")
                && e.to_string()
                    .contains("requires virtual memory to be enabled")
            {
                Ok(None)
            } else {
                Err(e)
            }
        }
    }
}