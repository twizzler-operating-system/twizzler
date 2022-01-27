use core::{
    alloc::Layout,
    intrinsics::{copy_nonoverlapping, write_bytes},
    ptr::{self, NonNull},
};

use crate::{
    llalloc::Heap,
    object::Protections,
    simple_mutex::Mutex,
    syscall::{
        sys_object_create, sys_object_map, BackingType, LifetimeType, MapFlags, ObjectCreate,
        ObjectCreateFlags,
    },
};

//static mut SCRATCH: [u8; 4096] = [0; 4096];
//static SCRATCH_PTR: AtomicUsize = AtomicUsize::new(0);

const NR_SLOTS: usize = 1024;
struct TwzGlobalAlloc {
    initial_slot: Option<AllocSlot>,
    other_slots: [Option<AllocSlot>; NR_SLOTS],
    slot_counter: usize,
}

struct AllocSlot {
    slot: usize,
    heap: Heap,
}

impl AllocSlot {
    pub fn new() -> Self {
        let slot = crate::slot::global_allocate();
        let create_spec = ObjectCreate::new(
            BackingType::Normal,
            LifetimeType::Volatile,
            None,
            ObjectCreateFlags::empty(),
        );
        let obj = sys_object_create(create_spec, &[], &[] /* TODO: ties */).unwrap();

        sys_object_map(
            obj,
            slot,
            Protections::READ | Protections::WRITE,
            MapFlags::empty(),
        )
        .unwrap();
        let (start, end) = crate::slot::to_vaddr_range(slot);
        let mut me = Self {
            slot,
            heap: Heap::empty(),
        };
        unsafe {
            me.heap.init(start, end);
        }
        me
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
        for slot in &mut self.other_slots {
            if let Some(ref mut slot) = slot {
                if slot.is_in(ptr) {
                    slot.free(ptr, layout);
                    return;
                }
            }
        }
        panic!("free for pointer that was not allocated by us");
    }

    pub fn allocate(&mut self, layout: Layout) -> *mut u8 {
        if self.initial_slot.is_none() {
            self.initial_slot = Some(AllocSlot::new());
        }
        let ptr = self.initial_slot.as_mut().unwrap().allocate(layout);
        if !ptr.is_null() {
            return ptr;
        }
        let mut count = 0;
        loop {
            if self.other_slots[self.slot_counter].is_none() {
                self.other_slots[self.slot_counter] = Some(AllocSlot::new());
            }
            let ptr = self.other_slots[self.slot_counter]
                .as_mut()
                .unwrap()
                .allocate(layout);
            if !ptr.is_null() {
                return ptr;
            }
            self.slot_counter += 1;
            count += 1;
            if count > NR_SLOTS {
                let ptr = self.initial_slot.as_mut().unwrap().allocate(layout);
                return ptr;
            }
        }
    }
}

unsafe impl Sync for TwzGlobalAlloc {}
unsafe impl Send for TwzGlobalAlloc {}

static mut TGA: TwzGlobalAlloc = TwzGlobalAlloc::new();
static TGA_LOCK: Mutex = Mutex::new();

fn adj_layout(layout: Layout) -> Layout {
    Layout::from_size_align(layout.size(), core::cmp::max(layout.align(), 16)).unwrap()
    //TODO
}

pub fn global_alloc(layout: Layout) -> *mut u8 {
    let layout = adj_layout(layout);
    /* crate::syscall::sys_kernel_console_write(
        b"alloc\n",
        crate::syscall::KernelConsoleWriteFlags::empty(),
    );
    unsafe {
        crate::arch::syscall::raw_syscall(
            crate::syscall::Syscall::Null,
            &[0, layout.size() as u64, layout.align() as u64],
        );
    }
    */
    let res = unsafe {
        TGA_LOCK.lock();
        let res = TGA.allocate(layout);
        TGA_LOCK.unlock();
        res
    };
    /*
    unsafe {
        crate::arch::syscall::raw_syscall(
            crate::syscall::Syscall::Null,
            &[res as u64, layout.size() as u64, layout.align() as u64],
        );
    }
    */
    res
    /*
    let start = SCRATCH_PTR.load(Ordering::SeqCst);
    let tstart = if start > 0 {
        ((start - 1) & !(layout.align() - 1)) + layout.align()
    } else {
        start
    };
    let nstart = tstart + core::cmp::max(layout.size(), layout.align());
    if SCRATCH_PTR
        .compare_exchange(start, nstart, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return global_alloc(layout);
    }
    if tstart + layout.size() >= 4096 {
        panic!("out of early memory");
    }
    return unsafe { SCRATCH.as_mut_ptr().add(start) };
    */
}

pub fn global_free(ptr: *mut u8, layout: Layout) {
    let layout = adj_layout(layout);
    /*crate::syscall::sys_kernel_console_write(
        b"free\n",
        crate::syscall::KernelConsoleWriteFlags::empty(),
    );
    */
    if ptr.is_null() {
        return;
    }
    unsafe {
        TGA_LOCK.lock();
        let res = TGA.free(ptr, layout);
        TGA_LOCK.unlock();
        res
    }
}

pub fn global_realloc(ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
    let layout = adj_layout(layout);
    let new_layout = Layout::from_size_align(new_size, layout.align()).unwrap();
    let new = global_alloc(new_layout);
    unsafe {
        if layout.size() < new_size {
            write_bytes(new.add(layout.size()), 0, new_size - layout.size());
            copy_nonoverlapping(ptr, new, layout.size());
        } else {
            copy_nonoverlapping(ptr, new, new_size);
        }
    }
    global_free(ptr, layout);
    new
}
