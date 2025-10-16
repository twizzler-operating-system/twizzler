/* From https://github.com/manenko/cassander/blob/6b1326d5b27c2fbe7176c2b62be0432ac65daa50/src/allocator.rs */

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
    size: uintptr_t, 
    prot_flags: uint32_t, 
    **ret:uint8_t
) -> *mut ::core::ffi::c_void {
    let Ok(layout) = core::alloc::Layout::from_size_align(sz, DEFAULT_ALIGNMENT) else {
      return core::ptr::null_mut();
    };
    unsafe {
      // always zero memory
      let block_start = OUR_RUNTIME.default_allocator().alloc_zeroed(layout).cast();
      if (block_start.is_null()) {
        return core::ptr::null_mut(); 
      }

      store_layout(layout, block_start) as *mut ::core::ffi::c_void
    }
}

 #[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn wasmtime_mmap_remap(
    addr: *uint8_t, 
    size: *uintptr_t, 
    prot_flags: *uint32_t) {

    // TODO: handle prot_flags

    let new_size = size + LAYOUT_DATA_SIZE;

    let (block_start, layout) = restore_layout(addr as *const u8);
    let Ok(new_layout) = core::alloc::Layout
      ::from_size_align(new_size, layout.align()) else {
        return core::ptr::null_mut();
    };

    unsafe {
      let new_block_start = OUR_RUNTIME
             .default_allocator()
             .realloc(block_start, layout, new_size)
             .cast()
      if (block_start.is_null()) {
          return core::ptr::null_mut(); 
      }
      store_layout(new_layout, new_block_start) as *mut c_void
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn wasmtime_munmap(
    ptr: *uint8_t, 
    size: uintptr_t) {
    let (_, layout) = restore_layout(ptr as *const u8);
    unsafe {
        OUR_RUNTIME.default_allocator().dealloc(ptr.cast(), layout);
    }
}

// int wasmtime_mprotect(uint8_t *ptr, uintptr_t size, uint32_t prot_flags) {
//   int rc = mprotect(ptr, size, wasmtime_to_mmap_prot_flags(prot_flags));
//   if (rc != 0)
//     return errno;
//   return 0;
// }