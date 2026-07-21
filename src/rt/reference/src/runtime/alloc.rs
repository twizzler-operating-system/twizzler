use std::{
    alloc::GlobalAlloc,
    ptr::NonNull,
    sync::{
        atomic::{AtomicBool, Ordering},
        OnceLock,
    },
};

use twizzler_abi::{object::ObjID, syscall::sys_thread_gettls};

use super::{ReferenceRuntime, RuntimeState};

mod ferroc;
mod talc;

pub use talc::LOCAL_ALLOCATOR;

static COMP_NAME: OnceLock<String> = OnceLock::new();
static COMP_NAME_READY: AtomicBool = AtomicBool::new(false);

#[thread_local]
static COMP_NAME_SKIP: AtomicBool = AtomicBool::new(false);

#[allow(dead_code)]
#[allow(unused)]
fn print_comp_name(layout: std::alloc::Layout, is_free: bool) {
    return;
    if sys_thread_gettls() == 0 {
        return;
    }
    if !COMP_NAME_SKIP.load(Ordering::SeqCst) {
        COMP_NAME_SKIP.store(true, Ordering::SeqCst);
        let comp_name = if COMP_NAME_READY.swap(true, Ordering::SeqCst) {
            COMP_NAME.get()
        } else {
            let comp = monitor_api::CompartmentHandle::current();
            if let Ok(raw) = monitor_api::monitor_rt_get_compartment_info(None) {
                if raw.name_len == 6 {
                    let info = comp.info().unwrap();
                    let name = info.name.clone();
                    std::mem::forget(info);
                    Some(COMP_NAME.get_or_init(|| name))
                } else {
                    None
                }
            } else {
                None
            }
        };

        if comp_name.is_some_and(|s| s.as_str() == "naming") {
            twizzler_abi::klog_println!(
                "{:?}: alloc: {} bytes, align = {}",
                comp_name,
                layout.size(),
                layout.align()
            );
            if !is_free {
                let b = std::backtrace::Backtrace::force_capture();
                for frame in b.frames().iter().take(7).enumerate() {
                    twizzler_abi::klog_println!("frame: {:?}", frame);
                }
            }
        }
        COMP_NAME_SKIP.store(false, Ordering::SeqCst);
    }
}

unsafe impl GlobalAlloc for ReferenceRuntime {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        if !self.state().contains(RuntimeState::READY)
            || self.state().contains(RuntimeState::IS_MONITOR)
        {
            return LOCAL_ALLOCATOR.alloc(layout);
        }

        print_comp_name(layout, false);
        //let start_time = Instant::now();
        let r = ferroc::TwzFerroc
            .allocate(layout)
            .map(|nn| nn.as_ptr())
            .unwrap_or(core::ptr::null_mut())
            .cast::<u8>();
        //let end_time = Instant::now();
        //trace_runtime_alloc(r.addr(), layout, end_time - start_time, false);
        r
    }

    unsafe fn alloc_zeroed(&self, layout: std::alloc::Layout) -> *mut u8 {
        if !self.state().contains(RuntimeState::READY)
            || self.state().contains(RuntimeState::IS_MONITOR)
        {
            return LOCAL_ALLOCATOR.alloc_zeroed(layout);
        }

        print_comp_name(layout, false);
        //let start_time = Instant::now();
        let r = ferroc::TwzFerroc
            .allocate_zeroed(layout)
            .map(|nn| nn.as_ptr())
            .unwrap_or(core::ptr::null_mut())
            .cast::<u8>();
        //let end_time = Instant::now();
        //trace_runtime_alloc(r.addr(), layout, end_time - start_time, false);
        r
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
        if !self.state().contains(RuntimeState::READY) {
            return;
        }

        if self.state().contains(RuntimeState::IS_MONITOR) {
            return LOCAL_ALLOCATOR.dealloc(ptr, layout);
        }

        if LOCAL_ALLOCATOR.get_id_from_ptr(ptr).is_some() {
            return;
        }

        if let Some(ptr) = NonNull::new(ptr) {
            //let start_time = Instant::now();
            print_comp_name(layout, true);
            ferroc::TwzFerroc.deallocate(ptr, layout);
            //let end_time = Instant::now();
            //trace_runtime_alloc(ptr.addr().into(), layout, end_time - start_time, true);
        }
    }
}

impl ReferenceRuntime {
    pub(crate) fn register_bootstrap_alloc(&self, slot: usize) {
        LOCAL_ALLOCATOR
            .bootstrap_alloc_slot
            .store(slot, Ordering::SeqCst);
    }

    pub fn get_id_from_heap_ptr(&self, ptr: *const u8) -> Option<ObjID> {
        LOCAL_ALLOCATOR.get_id_from_ptr(ptr)
    }

    pub fn heap_gc(&self) {
        ferroc::TwzFerroc.collect(true);
    }
}
