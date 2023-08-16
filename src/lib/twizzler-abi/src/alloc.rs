//! Global allocation. Used by the Rust standard library as the global allocator. Similar to
//! malloc() and friends.
//!
//! Currently, we maintain a list of allocatable objects, adding as needed, that we can pull from.
//! We used a simple linked-list allocator to perform allocation within objects. This whole system
//! can be optimized dramatically.

/*
KANI_TODO
*/

use core::{
    alloc::Layout,
    intrinsics::{copy_nonoverlapping, write_bytes},
    ptr::{self, NonNull},
};

use crate::{
    llalloc::Heap,
    object::Protections,
    slot::get_context_object_mut,
    syscall::{
        sys_object_create, sys_object_map, BackingType, LifetimeType, MapFlags, ObjectCreate,
        ObjectCreateFlags,
    },
};

const NR_SLOTS: usize = 1024;
pub(crate) struct TwzGlobalAlloc {
    initial_slot: Option<AllocSlot>,
    other_slots: [Option<AllocSlot>; NR_SLOTS],
    slot_counter: usize,
}

struct AllocSlot {
    slot: usize,
    heap: Heap,
}

impl AllocSlot {
    pub fn new() -> Option<Self> {
        let slot = crate::slot::global_allocate()?;
        let create_spec = ObjectCreate::new(
            BackingType::Normal,
            LifetimeType::Volatile,
            None,
            ObjectCreateFlags::empty(),
        );
        let obj = sys_object_create(create_spec, &[], &[] /* TODO: ties */).ok()?;

        sys_object_map(
            None,
            obj,
            slot,
            Protections::READ | Protections::WRITE,
            MapFlags::empty(),
        )
        .ok()?;
        let (start, end) = crate::slot::to_vaddr_range(slot);
        let mut me = Self {
            slot,
            heap: Heap::empty(),
        };
        unsafe {
            me.heap.init(start, end);
        }
        Some(me)
    }

    pub fn allocate(&mut self, layout: Layout) -> *mut u8 {
        self.heap
            .allocate_first_fit(layout)
            .map_or(ptr::null_mut(), |p| p.as_ptr())
    }

    pub fn is_in(&self, p: *mut u8) -> bool {
        let (start, end) = crate::slot::to_vaddr_range(self.slot);
        p as usize >= start && (p as usize) < end
    }

    pub fn free(&mut self, p: *mut u8, layout: Layout) {
        if let Some(p) = NonNull::new(p) {
            unsafe {
                self.heap.deallocate(p, layout);
            }
        }
    }
}

impl TwzGlobalAlloc {
    pub const fn new() -> Self {
        const N: Option<AllocSlot> = None;
        Self {
            initial_slot: None,
            other_slots: [N; 1024],
            slot_counter: 0,
        }
    }

    pub fn free(&mut self, ptr: *mut u8, layout: Layout) {
        if let Some(ref mut s) = self.initial_slot {
            if s.is_in(ptr) {
                s.free(ptr, layout);
                return;
            }
        }
        for slot in &mut self.other_slots.iter_mut().flatten() {
            if slot.is_in(ptr) {
                slot.free(ptr, layout);
                return;
            }
        }
        panic!("free for pointer that was not allocated by us");
    }

    pub fn allocate(&mut self, layout: Layout) -> *mut u8 {
        if self.initial_slot.is_none() {
            self.initial_slot = AllocSlot::new();
            if self.initial_slot.is_none() {
                crate::print_err("failed to allocate initial allocation object");
                unsafe {
                    crate::internal_abort();
                }
            }
        }
        let ptr = crate::internal_unwrap(
            self.initial_slot.as_mut(),
            "failed to unwrap initial allocation object",
        )
        .allocate(layout);
        if !ptr.is_null() {
            return ptr;
        }
        let mut count = 0;
        loop {
            if self.other_slots[self.slot_counter].is_none() {
                self.other_slots[self.slot_counter] = AllocSlot::new();
                if self.other_slots[self.slot_counter].is_none() {
                    crate::print_err("failed to create allocation object");
                    unsafe {
                        crate::internal_abort();
                    }
                }
            }
            let ptr = crate::internal_unwrap(
                self.other_slots[self.slot_counter].as_mut(),
                "failed to unwrap allocation slot",
            )
            .allocate(layout);
            if !ptr.is_null() {
                return ptr;
            }
            self.slot_counter += 1;
            count += 1;
            if count > NR_SLOTS {
                let ptr = crate::internal_unwrap(
                    self.initial_slot.as_mut(),
                    "failed to unwrap initial slot object",
                )
                .allocate(layout);
                return ptr;
            }
        }
    }
}

unsafe impl Sync for TwzGlobalAlloc {}
unsafe impl Send for TwzGlobalAlloc {}

//static mut TGA: TwzGlobalAlloc = TwzGlobalAlloc::new();
//static TGA_LOCK: Mutex = Mutex::new();

fn adj_layout(layout: Layout) -> Layout {
    crate::internal_unwrap(
        Layout::from_size_align(layout.size(), core::cmp::max(layout.align(), 16)).ok(),
        "failed to crate Layout for allocation",
    )
}

/// Allocate a region of memory as specified by `layout`. Minimum 16-byte alignment. If we are out
/// of memory, return null.
///
/// # Safety
/// The caller must ensure that the returned memory is freed at the right time.
pub unsafe fn global_alloc(layout: Layout) -> *mut u8 {
    let layout = adj_layout(layout);
    /*
    let mut sz = layout.size();
    crate::syscall::sys_kernel_console_write(
        b"ALLOC: ",
        crate::syscall::KernelConsoleWriteFlags::empty(),
    );
    while sz > 0 {
        crate::syscall::sys_kernel_console_write(
            &[(sz & 0xf) as u8 + b'a', 0],
            crate::syscall::KernelConsoleWriteFlags::empty(),
        );
        sz = sz >> 4;
    }
    */
    let ctx = get_context_object_mut();
    ctx.alloc_lock.lock();
    let res = ctx.global_alloc.allocate(layout);
    ctx.alloc_lock.unlock();
    res
}

/// Free a region of previously allocated memory. If ptr is null, do nothing.
///
/// # Safety
/// The caller must ensure the prevention of use-after-free and double-free.
pub unsafe fn global_free(ptr: *mut u8, layout: Layout) {
    let layout = adj_layout(layout);
    /*crate::syscall::sys_kernel_console_write(
        b"free\n",
        crate::syscall::KernelConsoleWriteFlags::empty(),
    );
    */
    if ptr.is_null() {
        return;
    }
    let ctx = get_context_object_mut();
    ctx.alloc_lock.lock();
    let res = ctx.global_alloc.free(ptr, layout);
    ctx.alloc_lock.unlock();
    res
}

/// Reallocate a region of memory. Acts like realloc.
///
/// # Safety
/// The caller must prevent use-after-free and double-free for ptr, and it must track the returned
/// memory properly as in [global_alloc].
pub unsafe fn global_realloc(ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
    let layout = adj_layout(layout);
    let new_layout = crate::internal_unwrap(
        Layout::from_size_align(new_size, layout.align()).ok(),
        "failed to create Layout for realloc",
    );
    if ptr.is_null() {
        return global_alloc(new_layout);
    }
    let new = global_alloc(new_layout);
    if layout.size() < new_size {
        write_bytes(new.add(layout.size()), 0, new_size - layout.size());
        copy_nonoverlapping(ptr, new, layout.size());
    } else {
        copy_nonoverlapping(ptr, new, new_size);
    }
    global_free(ptr, layout);
    new
}
