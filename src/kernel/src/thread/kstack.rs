//! Kernel stack allocation.
//!
//! Stacks are carved from chunks taken straight from the kernel memory context, the way ferroc's
//! base allocator takes its, rather than from the heap. A stack is a fixed-size, page-aligned,
//! long-lived object, so a general-purpose allocator has nothing to contribute -- and routing it
//! through `alloc_zeroed` cost a full [`THREAD_STACK_SIZE`] memset on every thread creation, which
//! measured as ~87% of the in-kernel half of a spawn.
//!
//! Freed stacks go on a free list and are handed back out as-is. That is the point: a recycled
//! stack needs no work at all beyond [`STACK_TOP_ZERO`] bytes at the top.

use core::{
    alloc::Layout,
    ptr::NonNull,
    sync::atomic::{AtomicPtr, Ordering},
};

use crate::{
    memory::context::{KernelMemoryContext, kernel_context},
    processor::{KERNEL_STACK_SIZE, THREAD_STACK_SIZE},
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

/// Stacks are page-aligned so that a guard page can be unmapped below one.
const STACK_ALIGN: usize = 0x1000;

/// An unmapped page below each stack, so an overflow faults (a double fault, reported by the x86
/// handler) instead of silently writing into whatever sits below. aarch64 has no separate
/// exception stack to report it from yet, so it keeps the old layout.
#[cfg(target_arch = "x86_64")]
const GUARD: usize = 0x1000;
#[cfg(not(target_arch = "x86_64"))]
const GUARD: usize = 0;
const STRIDE: usize = GUARD + THREAD_STACK_SIZE;

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

/// An owned kernel stack, returned to the free list when dropped or [detached](Self::detach).
pub struct KernelStack {
    base: AtomicPtr<u8>,
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
                base.as_ptr().add(THREAD_STACK_SIZE - STACK_TOP_ZERO),
                0,
                STACK_TOP_ZERO,
            );
        }
        Self {
            base: AtomicPtr::new(base.as_ptr()),
        }
    }

    /// The low address of the stack, or null once detached. It grows down from
    /// `as_ptr() + THREAD_STACK_SIZE`.
    pub fn as_ptr(&self) -> *mut u8 {
        self.base.load(Ordering::Acquire)
    }

    /// Take the stack out of this handle, which is then empty. The caller returns it with
    /// [`release`] once nothing can be running on it.
    pub fn detach(&self) -> Option<NonNull<u8>> {
        NonNull::new(self.base.swap(core::ptr::null_mut(), Ordering::AcqRel))
    }
}

impl Drop for KernelStack {
    fn drop(&mut self) {
        if let Some(base) = NonNull::new(*self.base.get_mut()) {
            push(base);
        }
    }
}

/// Return a stack taken with [`KernelStack::detach`].
pub fn release(base: NonNull<u8>) {
    push(base);
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
    let layout = Layout::from_size_align(STACKS_PER_CHUNK * STRIDE, STACK_ALIGN).unwrap();
    let chunk = kernel_context()
        .allocate_chunk(layout)
        .expect("failed to allocate a chunk of kernel stacks");
    // Not during bring-up: the shootdown would wait on secondaries parked with interrupts off
    // until the boot cpu, which is the one allocating their stacks, releases them. Those few
    // stacks go without a guard.
    #[cfg(target_arch = "x86_64")]
    if crate::processor::mp::secondaries_released() {
        let mut fa = crate::memory::tracker::FrameAllocator::new(
            crate::memory::tracker::FrameAllocFlags::KERNEL
                | crate::memory::tracker::FrameAllocFlags::ZEROED,
            crate::memory::frame::PHYS_LEVEL_LAYOUTS[0],
        );
        for i in 0..STACKS_PER_CHUNK {
            let guard =
                crate::memory::VirtAddr::new(chunk.as_ptr() as u64 + (i * STRIDE) as u64).unwrap();
            kernel_context().with_arch(crate::security::KERNEL_SCTX, |arch| {
                arch.unmap_page(guard, &mut fa)
            });
        }
    }
    // Safety: within the chunk just allocated, and stack-aligned by construction.
    let stack =
        |i: usize| unsafe { NonNull::new_unchecked(chunk.as_ptr().add(i * STRIDE + GUARD)) };
    for i in 1..STACKS_PER_CHUNK {
        push(stack(i));
    }
    stack(0)
}

/// The guard page below `stack`, for the double-fault report.
pub fn guard_below(stack: *mut u8) -> core::ops::Range<u64> {
    let base = stack as u64;
    base.saturating_sub(GUARD as u64)..base
}

/// A [`KERNEL_STACK_SIZE`] stack that is never given back, for the per-cpu stacks that live as
/// long as the kernel. Carved during bring-up, so unguarded, and kept at full size for that.
pub fn leak_one() -> *mut u8 {
    let layout = Layout::from_size_align(KERNEL_STACK_SIZE, STACK_ALIGN).unwrap();
    kernel_context()
        .allocate_chunk(layout)
        .expect("failed to allocate a per-cpu kernel stack")
        .as_ptr()
}

#[cfg(all(test, target_arch = "x86_64"))]
mod test {
    use twizzler_kernel_macros::kernel_test;

    use super::*;
    use crate::memory::{VirtAddr, pagetables::MappingCursor};

    fn mapped_pages(start: u64) -> usize {
        let cursor = MappingCursor::new(VirtAddr::new(start).unwrap(), GUARD);
        kernel_context().with_arch(crate::security::KERNEL_SCTX, |arch| {
            arch.readmap(cursor, |r| r.count())
        })
    }

    /// From a fresh chunk: the free list can hand out a stack carved during bring-up, unguarded.
    #[kernel_test]
    fn stack_guard_is_unmapped() {
        assert!(crate::processor::mp::secondaries_released());
        logln!("thread kernel stack: {} KiB", THREAD_STACK_SIZE / 1024);
        let stack = refill();
        let guard = guard_below(stack.as_ptr());
        assert_eq!(mapped_pages(guard.start), 0);
        assert_eq!(mapped_pages(guard.end), 1);
        push(stack);
    }
}
