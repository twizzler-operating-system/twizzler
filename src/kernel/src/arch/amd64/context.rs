use core::arch::naked_asm;

use twizzler_abi::object::Protections;

use crate::{
    arch::{
        PhysAddr,
        memory::pagetables::{Entry, EntryFlags},
    },
    memory::{
        VirtAddr,
        frame::get_frame,
        pagetables::{
            Consistency, ContiguousProvider, MapReader, Mapper, MappingCursor, MappingFlags,
            MappingSettings, PhysAddrProvider,
        },
        tracker::{FrameAllocFlags, alloc_frame, free_frame},
    },
    obj::pagetables::ObjectPageTable,
    once::Once,
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
        let mut mapper = Mapper::new(
            alloc_frame(FrameAllocFlags::ZEROED | FrameAllocFlags::KERNEL).start_address(),
        );
        setup_mapper_with_kpages(&mut mapper);
        let target = ArchContextTarget(mapper.root_address().into());
        Self {
            target,
            inner: Spinlock::new(mapper),
        }
    }

    pub fn switch_to(&self) {
        unsafe { Self::switch_to_target(&self.target) }
    }

    pub fn with_mapper<R>(&self, f: impl FnOnce(&mut Mapper) -> R) -> R {
        let mut inner = self.inner.lock();
        let result = f(&mut inner);
        result
    }

    /// Switch to a given set of page tables.
    ///
    /// # Safety
    /// The specified target must be a root page table that will live as long as we are switched to
    /// it.
    pub unsafe fn switch_to_target(tgt: &ArchContextTarget) {
        unsafe {
            if tgt.0 != x86::controlregs::cr3() {
                x86::controlregs::cr3_write(tgt.0);
            }
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

    pub fn map(&self, cursor: MappingCursor, phys: &mut impl PhysAddrProvider) {
        let (mut consist, mut guard) = self.lock_with_consist(cursor);

        guard.map(cursor, phys, &mut consist).unwrap();
        consist.tlb_mut().finish();
        drop(guard);
        consist.into_deferred().run_all();
    }

    pub fn object_map(
        &self,
        cursor: MappingCursor,
        object_tables: &mut ObjectPageTable,
        settings: MappingSettings,
    ) {
        let (mut consist, mut guard) = self.lock_with_consist(cursor);
        guard
            .object_map(cursor, object_tables, settings, &mut consist)
            .unwrap();
        consist.tlb_mut().finish();
        drop(guard);
        consist.into_deferred().run_all();
    }

    pub fn ensure_object_mapped(
        &self,
        cursor: MappingCursor,
        object_tables: &mut ObjectPageTable,
        settings: MappingSettings,
    ) -> bool {
        let (mut consist, mut guard) = self.lock_with_consist(cursor);
        if !guard.is_object_mapped(cursor, settings) {
            guard
                .object_map(cursor, object_tables, settings, &mut consist)
                .unwrap();
            consist.tlb_mut().finish();
            drop(guard);
            consist.into_deferred().run_all();
            true
        } else {
            false
        }
    }

    pub fn change(&self, cursor: MappingCursor, settings: &MappingSettings) {
        let (mut consist, mut guard) = self.lock_with_consist(cursor);
        guard.change(cursor, settings, &mut consist).unwrap();
        consist.tlb_mut().finish();
        drop(guard);
        consist.into_deferred().run_all();
    }

    pub fn unmap(&self, cursor: MappingCursor) {
        let (mut consist, mut guard) = self.lock_with_consist(cursor);
        guard.unmap(cursor, &mut consist).unwrap();
        consist.tlb_mut().finish();
        drop(guard);
        consist.into_deferred().run_all();
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
    for idx in 256..512 {
        mapper.set_top_level_table(idx, km.get_top_level_table(idx));
    }
    let frame =
        alloc_frame(FrameAllocFlags::ZEROED | FrameAllocFlags::KERNEL | FrameAllocFlags::WAIT_OK);
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
        // Unmap all user memory to clear any allocated page tables.
        self.unmap(MappingCursor::new(
            VirtAddr::start_user_memory(),
            VirtAddr::end_user_memory() - VirtAddr::start_user_memory(),
        ));
        // Manually free the root.
        if let Some(frame) = get_frame(self.inner.lock().root_address()) {
            free_frame(frame);
        }
    }
}

/*
impl ArchContextInner {
    fn new() -> Self {
        let mut mapper = Mapper::new(
            alloc_frame(
                FrameAllocFlags::ZEROED | FrameAllocFlags::KERNEL | FrameAllocFlags::WAIT_OK,
            )
            .start_address(),
        );
    }

    fn map(
        &mut self,
        cursor: MappingCursor,
        phys: &mut impl PhysAddrProvider,
    ) -> Result<(), DeferredUnmappingOps> {
        let consist = Consistency::new(self.mapper.root_address());
        if cursor.start().raw() == 0 {
            let Some(cursor) = cursor.advance(0x1000) else {
                return Ok(());
            };
            phys.consume(0x1000);
            return self.mapper.map(cursor, phys, consist);
        }
        self.mapper.map(cursor, phys, consist)
    }

    fn change(
        &mut self,
        cursor: MappingCursor,
        settings: &MappingSettings,
        consist: &mut Consistency,
    ) {
        self.mapper.change(cursor, settings, consist);
    }

    fn unmap(&mut self, cursor: MappingCursor) -> DeferredUnmappingOps {
        if cursor.start().raw() == 0 {
            let Some(cursor) = cursor.advance(0x1000) else {
                return Consistency::new_full_global().into_deferred();
            };
            return self.mapper.unmap(cursor);
        }
        self.mapper.unmap(cursor)
    }

    fn object_map(
        &mut self,
        cursor: MappingCursor,
        object_tables: &mut ObjectPageTable,
        prot: Protections,
    ) -> DeferredUnmappingOps {
        self.mapper.object_map(cursor, object_tables, prot)
    }
}


*/
