use core::{alloc::Layout, sync::atomic::{AtomicUsize, Ordering}};

static mut SCRATCH: [u8; 4096] = [0; 4096];
static SCRATCH_PTR: AtomicUsize = AtomicUsize::new(0);

pub fn global_alloc(layout: Layout) -> *mut u8 {
            let start = SCRATCH_PTR.load(Ordering::SeqCst);
            let tstart = if start > 0 {
               ((start - 1) & !(layout.align() - 1)) + layout.align()
            } else {
                start
            };
            let nstart = tstart + core::cmp::max(layout.size(), layout.align());
            if 
                SCRATCH_PTR.compare_exchange(start, nstart, Ordering::SeqCst, Ordering::SeqCst).is_err() {
                    return global_alloc(layout);
            }
            if tstart + layout.size() >= 4096 {
                panic!("out of early memory");
            }
            return unsafe{SCRATCH.as_mut_ptr().add(start)};

}

pub fn global_free(_ptr: *mut u8, _layout: Layout) {

}

pub fn global_realloc(_ptr: *mut u8, _layout: Layout, _new_size: usize) -> *mut u8 {
    loop{}
}