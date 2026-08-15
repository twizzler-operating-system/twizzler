//! Kernel stack allocation.
//!
//! Stacks are carved from chunks taken straight from the kernel memory context, the way ferroc's
//! base allocator takes its, rather than from the heap. A stack is a fixed-size, page-aligned,
//! long-lived object, so a general-purpose allocator has nothing to contribute -- and routing it
//! through `alloc_zeroed` cost a full [`KERNEL_STACK_SIZE`] memset on every thread creation, which
//! measured as ~87% of the in-kernel half of a spawn.
//!
//! Freed stacks go on a free list and are handed back out as-is. That is the point: a recycled
//! stack needs no work at all beyond [`STACK_TOP_ZERO`] bytes at the top.

use core::{alloc::Layout, ptr::NonNull};

use crate::{
    memory::context::{KernelMemoryContext, kernel_context},
    processor::KERNEL_STACK_SIZE,
    spinlock::Spinlock,
};

/// Stacks per chunk. Chunk allocation maps and zeroes memory, so this is how far that cost is
/// amortized the first time through; afterwards the free list serves everything.
const STACKS_PER_CHUNK: usize = 4;

/// How much of the top of a stack to zero on handout.
///
/// A stack is used from the top down and nothing reads a slot before writing it: `init_va` writes
/// the initial frame at the top, and every frame below it is written by its own prologue. So only
/// the first page touched needs defined contents, and the rest can carry whatever the previous
/// owner of a recycled stack left behind.
const STACK_TOP_ZERO: usize = 0x1000;

/// Stacks are page-aligned so that a guard page can eventually be unmapped below one.
const STACK_ALIGN: usize = 0x1000;

struct FreeList {
    /// Head of a list threaded through the free stacks themselves: the first word of a free stack
    /// holds the next one. Nothing is allocated to track free stacks, which matters because this
    /// list is pushed to from a `Thread`'s drop path.
    head: *mut u8,
}

// Safety: these point into the kernel's global mapping, which is valid for the life of the kernel,
// and a stack is on this list only when it has exactly no other owner.
unsafe impl Send for FreeList {}

static FREE: Spinlock<FreeList> = Spinlock::new(FreeList {
    head: core::ptr::null_mut(),
});

/// An owned kernel stack, returned to the free list when dropped.
pub struct KernelStack {
    base: NonNull<u8>,
}

// Safety: as for `FreeList`, plus: the memory is only ever reachable through this handle, which is
// moved into the owning `Thread`.
unsafe impl Send for KernelStack {}
unsafe impl Sync for KernelStack {}

impl KernelStack {
    pub fn new() -> Self {
        let base = pop().unwrap_or_else(refill);
        // Safety: `base` names a whole stack that nothing else holds.
        unsafe {
            core::ptr::write_bytes(
                base.as_ptr().add(KERNEL_STACK_SIZE - STACK_TOP_ZERO),
                0,
                STACK_TOP_ZERO,
            );
        }
        Self { base }
    }

    /// The low address of the stack. It grows down from `as_ptr() + KERNEL_STACK_SIZE`.
    pub fn as_ptr(&self) -> *mut u8 {
        self.base.as_ptr()
    }
}

impl Drop for KernelStack {
    fn drop(&mut self) {
        push(self.base);
    }
}

fn pop() -> Option<NonNull<u8>> {
    let mut free = FREE.lock();
    let head = NonNull::new(free.head)?;
    // Safety: a stack on the list holds its link in its first word, written by `push`.
    free.head = unsafe { head.as_ptr().cast::<*mut u8>().read() };
    Some(head)
}

fn push(base: NonNull<u8>) {
    let mut free = FREE.lock();
    // Safety: the stack has no other owner, so its first word is ours to write.
    unsafe { base.as_ptr().cast::<*mut u8>().write(free.head) };
    free.head = base.as_ptr();
}

/// Take a fresh chunk and carve it into stacks, keeping all but the one returned.
///
/// Deliberately not under [`FREE`]: chunk allocation maps pages and allocates frames, and holding a
/// spinlock across that would stall every other thread creation and exit. Two cpus racing here just
/// means one extra chunk, whose stacks land on the free list and get used.
fn refill() -> NonNull<u8> {
    let layout =
        Layout::from_size_align(STACKS_PER_CHUNK * KERNEL_STACK_SIZE, STACK_ALIGN).unwrap();
    let chunk = kernel_context()
        .allocate_chunk(layout)
        .expect("failed to allocate a chunk of kernel stacks");
    for i in 1..STACKS_PER_CHUNK {
        // Safety: within the chunk just allocated, and stack-aligned by construction.
        push(unsafe { NonNull::new_unchecked(chunk.as_ptr().add(i * KERNEL_STACK_SIZE)) });
    }
    chunk
}

/// Take a stack and never give it back, for the per-cpu stacks that live as long as the kernel.
pub fn leak_one() -> *mut u8 {
    core::mem::ManuallyDrop::new(KernelStack::new()).as_ptr()
}
