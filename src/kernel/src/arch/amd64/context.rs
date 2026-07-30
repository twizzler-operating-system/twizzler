use core::{arch::naked_asm, sync::atomic::Ordering};

use twizzler_abi::object::Protections;

use crate::{
    arch::{
        PhysAddr,
        memory::pagetables::{Entry, EntryFlags},
    },
    memory::{
        VirtAddr,
        frame::{PHYS_LEVEL_LAYOUTS, get_frame},
        pagetables::{
            Consistency, ContiguousProvider, MapReader, Mapper, MappingCursor, MappingFlags,
            MappingSettings, PhysAddrProvider,
        },
        tracker::{FrameAllocFlags, FrameAllocator, alloc_frame, free_frame},
    },
    obj::pagetables::ObjectPageTable,
    once::Once,
    processor::Processor,
    spinlock::{SpinLockGuard, Spinlock},
};

pub struct ArchContext {
    pub target: ArchContextTarget,
    inner: Spinlock<Mapper>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
#[repr(transparent)]
pub struct ArchContextTarget(u64);

impl ArchContextTarget {
    pub fn paddr(&self) -> PhysAddr {
        PhysAddr::new(self.0).unwrap()
    }
}

static KERNEL_MAPPER: Once<Spinlock<Mapper>> = Once::new();

fn kernel_mapper() -> &'static Spinlock<Mapper> {
    KERNEL_MAPPER.call_once(|| {
        let frame = alloc_frame(FrameAllocFlags::ZEROED | FrameAllocFlags::KERNEL);
        frame.inc_refcount();
        frame.set_pt(true);
        let mut m = Mapper::new(frame.start_address());
        for idx in 256..512 {
            let frame = alloc_frame(FrameAllocFlags::ZEROED | FrameAllocFlags::KERNEL);
            frame.inc_refcount();
            frame.set_pt(true);
            m.set_top_level_table(
                idx,
                Entry::new(frame.start_address(), EntryFlags::intermediate()),
            );
        }
        Spinlock::new(m)
    })
}

impl Default for ArchContext {
    fn default() -> Self {
        Self::new()
    }
}

impl ArchContext {
    pub fn new_kernel() -> Self {
        Self::new()
    }

    pub fn new() -> Self {
        let frame = alloc_frame(FrameAllocFlags::ZEROED | FrameAllocFlags::KERNEL);
        frame.set_pt(true);
        frame.inc_refcount();
        let mut mapper = Mapper::new(frame.start_address());
        setup_mapper_with_kpages(&mut mapper);
        let target = ArchContextTarget(mapper.root_address().into());
        Self {
            target,
            inner: Spinlock::new(mapper),
        }
    }

    pub fn switch_to(&self, proc: Option<&Processor>) {
        unsafe { Self::switch_to_target(&self.target, proc) }
    }

    pub fn with_mapper<R>(&self, f: impl FnOnce(&mut Mapper) -> R) -> R {
        let mut inner = self.inner.lock();
        let result = f(&mut inner);
        result
    }

    /// Switch to a given set of page tables.
    ///
    /// `proc` should be the current processor, if TLS/the processor registry is up yet
    /// (it isn't during the very early boot switch from `memory::init()`) -- passing it
    /// explicitly keeps this low-level function from having to do its own TLS lookup.
    ///
    /// # Safety
    /// The specified target must be a root page table that will live as long as we are switched to
    /// it.
    pub unsafe fn switch_to_target(tgt: &ArchContextTarget, proc: Option<&Processor>) {
        unsafe {
            if tgt.0 != x86::controlregs::cr3() {
                x86::controlregs::cr3_write(tgt.0);
            }
        }
        if let Some(proc) = proc {
            proc.arch.active_cr3.store(tgt.0, Ordering::Release);
        }
    }

    fn lock_with_consist(&self, cursor: MappingCursor) -> (Consistency, SpinLockGuard<'_, Mapper>) {
        let consist = if cursor.start().is_kernel() {
            Consistency::new_full_global()
        } else {
            Consistency::new(self.target.paddr())
        };
        let guard = self.inner.lock();
        (consist, guard)
    }

    pub fn map(
        &self,
        cursor: MappingCursor,
        phys: &mut impl PhysAddrProvider,
        fa: &mut FrameAllocator,
    ) {
        let (mut consist, mut guard) = self.lock_with_consist(cursor);

        guard.map(cursor, phys, &mut consist, fa).unwrap();
        consist.tlb_mut().finish();
        drop(guard);
        consist.into_deferred().run_all();
    }

    pub fn object_map(
        &self,
        cursor: MappingCursor,
        object_tables: &mut ObjectPageTable,
        settings: MappingSettings,
        fa: &mut FrameAllocator,
    ) {
        let (mut consist, mut guard) = self.lock_with_consist(cursor);
        let took_ref = guard
            .object_map(cursor, object_tables, settings, &mut consist, fa)
            .unwrap();
        consist.tlb_mut().finish();
        drop(guard);
        consist.into_deferred().run_all();
        // Only count a map if we actually took a new reference, so that the single dec_map_count
        // done on unmap stays symmetric.
        if took_ref {
            object_tables.inc_map_count();
        }
    }

    pub fn ensure_object_mapped(
        &self,
        cursor: MappingCursor,
        object_tables: &mut ObjectPageTable,
        settings: MappingSettings,
        fa: &mut FrameAllocator,
    ) -> bool {
        let (mut consist, mut guard) = self.lock_with_consist(cursor);
        if !guard.is_object_mapped(cursor, settings) {
            let took_ref = guard
                .object_map(cursor, object_tables, settings, &mut consist, fa)
                .unwrap();
            consist.tlb_mut().finish();
            drop(guard);
            consist.into_deferred().run_all();
            if took_ref {
                object_tables.inc_map_count();
            }
            true
        } else {
            false
        }
    }

    pub fn change(
        &self,
        cursor: MappingCursor,
        settings: &MappingSettings,
        fa: &mut FrameAllocator,
    ) {
        let (mut consist, mut guard) = self.lock_with_consist(cursor);
        guard.change(cursor, settings, &mut consist, fa).unwrap();
        consist.tlb_mut().finish();
        drop(guard);
        consist.into_deferred().run_all();
    }

    pub fn unmap(&self, cursor: MappingCursor, fa: &mut FrameAllocator) -> bool {
        let (mut consist, mut guard) = self.lock_with_consist(cursor);
        let r = guard.unmap(cursor, &mut consist, fa).unwrap();
        consist.tlb_mut().finish();
        drop(guard);
        consist.into_deferred().run_all();
        r
    }

    pub fn readmap<R>(&self, cursor: MappingCursor, f: impl Fn(MapReader) -> R) -> R {
        let r = if cursor.start().is_kernel() {
            f(kernel_mapper().lock().readmap(cursor))
        } else {
            f(self.inner.lock().readmap(cursor))
        };
        r
    }
}

#[unsafe(naked)]
#[allow(named_asm_labels)]
unsafe extern "C" fn trampoline_trap() {
    naked_asm!(
        "push rbp",
        "mov rbp, rsp",
        "xor rdi, rdi",
        "xor rsi, rsi",
        "xor rax, rax",
        "syscall",
        "__here:",
        "jmp __here",
        "pop rbp",
        "ret"
    );
}

#[unsafe(no_mangle)]
unsafe extern "C-unwind" fn trap_entry() {
    panic!("hit trap entry");
}

fn setup_mapper_with_kpages(mapper: &mut Mapper) {
    let km = kernel_mapper().lock();
    let mut fa = FrameAllocator::new(
        FrameAllocFlags::ZEROED | FrameAllocFlags::KERNEL,
        PHYS_LEVEL_LAYOUTS[0],
    );
    for idx in 256..512 {
        mapper.set_top_level_table(idx, km.get_top_level_table(idx));
    }
    let frame = fa.try_allocate().unwrap();
    let mut z = ContiguousProvider::new(
        frame.start_address(),
        0x1000,
        MappingSettings::new(
            Protections::READ | Protections::EXEC,
            twizzler_abi::device::CacheType::WriteBack,
            MappingFlags::USER,
        ),
    );
    let mut consist = Consistency::new_full_global();
    mapper
        .map(
            MappingCursor::new(VirtAddr::new(0).unwrap(), 0x1000),
            &mut z,
            &mut consist,
            &mut fa,
        )
        .unwrap();
    consist.tlb_mut().finish();
    consist.into_deferred().run_all();
    let start = trampoline_trap as *const u8;
    let len = 0x100;
    #[allow(invalid_null_arguments)]
    let dest = frame.start_address().kernel_vaddr().as_mut_ptr::<u8>();
    unsafe { dest.copy_from(start, len) };
}

impl Drop for ArchContext {
    fn drop(&mut self) {
        let mut fa = FrameAllocator::new(
            FrameAllocFlags::ZEROED | FrameAllocFlags::KERNEL,
            PHYS_LEVEL_LAYOUTS[0],
        );
        // Unmap all user memory to clear any allocated page tables.
        self.unmap(
            MappingCursor::new(
                VirtAddr::start_user_memory(),
                VirtAddr::end_user_memory() - VirtAddr::start_user_memory(),
            ),
            &mut fa,
        );
        // Manually free the root.
        if let Some(frame) = get_frame(self.inner.lock().root_address()) {
            frame.set_pt(false);
            if frame.dec_refcount() == 0 {
                free_frame(frame);
            }
        }
    }
}
