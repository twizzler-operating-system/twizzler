use core::arch::{global_asm, naked_asm};

use twizzler_abi::object::Protections;

use crate::{
    arch::memory::pagetables::{Entry, EntryFlags},
    memory::{
        VirtAddr,
        frame::get_frame,
        pagetables::{
            Consistency, ContiguousProvider, DeferredUnmappingOps, MapReader, Mapper,
            MappingCursor, MappingFlags, MappingSettings, PhysAddrProvider, SharedPageTable,
        },
        tracker::{FrameAllocFlags, alloc_frame, free_frame},
    },
    mutex::Mutex,
    once::Once,
    spinlock::Spinlock,
};

pub struct ArchContextInner {
    mapper: Mapper,
}

pub struct ArchContext {
    pub target: ArchContextTarget,
    inner: Mutex<ArchContextInner>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
#[repr(transparent)]
pub struct ArchContextTarget(u64);

static KERNEL_MAPPER: Once<Spinlock<Mapper>> = Once::new();

fn kernel_mapper() -> &'static Spinlock<Mapper> {
    KERNEL_MAPPER.call_once(|| {
        let mut m = Mapper::new(
            alloc_frame(FrameAllocFlags::ZEROED | FrameAllocFlags::KERNEL).start_address(),
        );
        for idx in 256..512 {
            m.set_top_level_table(
                idx,
                Entry::new(
                    alloc_frame(FrameAllocFlags::ZEROED | FrameAllocFlags::KERNEL).start_address(),
                    EntryFlags::intermediate(),
                ),
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
        let inner = ArchContextInner::new();
        let target = ArchContextTarget(inner.mapper.root_address().into());
        Self {
            target,
            inner: Mutex::new(inner),
        }
    }

    pub fn switch_to(&self) {
        unsafe { Self::switch_to_target(&self.target) }
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

    pub fn map(&self, cursor: MappingCursor, phys: &mut impl PhysAddrProvider) {
        let ops = if cursor.start().is_kernel() {
            let consist = Consistency::new_full_global();
            kernel_mapper().lock().map(cursor, phys, consist)
        } else {
            self.inner.lock().map(cursor, phys)
        };
        if let Err(ops) = ops {
            ops.run_all();
        }
    }

    pub fn shared_map(&self, cursor: MappingCursor, spt: &SharedPageTable) {
        let ops = if cursor.start().is_kernel() {
            panic!("cannot map kernel memory with shared page tables")
        } else {
            self.inner.lock().shared_map(cursor, spt)
        };
        ops.run_all();
    }

    pub fn change(&self, cursor: MappingCursor, settings: &MappingSettings) {
        if cursor.start().is_kernel() {
            kernel_mapper().lock().change(cursor, settings);
        } else {
            self.inner.lock().change(cursor, settings);
        }
    }

    pub fn unmap(&self, cursor: MappingCursor) {
        let ops = if cursor.start().is_kernel() {
            kernel_mapper().lock().unmap(cursor)
        } else {
            self.inner.lock().unmap(cursor)
        };
        ops.run_all();
    }

    pub fn readmap<R>(&self, cursor: MappingCursor, f: impl Fn(MapReader) -> R) -> R {
        let r = if cursor.start().is_kernel() {
            f(kernel_mapper().lock().readmap(cursor))
        } else {
            f(self.inner.lock().mapper.readmap(cursor))
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

impl ArchContextInner {
    fn new() -> Self {
        let mut mapper = Mapper::new(
            alloc_frame(
                FrameAllocFlags::ZEROED | FrameAllocFlags::KERNEL | FrameAllocFlags::WAIT_OK,
            )
            .start_address(),
        );
        let km = kernel_mapper().lock();
        for idx in 256..512 {
            mapper.set_top_level_table(idx, km.get_top_level_table(idx));
        }
        let frame = alloc_frame(
            FrameAllocFlags::ZEROED | FrameAllocFlags::KERNEL | FrameAllocFlags::WAIT_OK,
        );
        let mut z = ContiguousProvider::new(
            frame.start_address(),
            0x1000,
            MappingSettings::new(
                Protections::READ | Protections::EXEC,
                twizzler_abi::device::CacheType::WriteBack,
                MappingFlags::GLOBAL | MappingFlags::USER,
            ),
        );
        mapper
            .map(
                MappingCursor::new(VirtAddr::new(0).unwrap(), 0x1000),
                &mut z,
                Consistency::new_full_global(),
            )
            .unwrap();
        let start = trampoline_trap as *const u8;
        let len = 0x100;
        #[allow(invalid_null_arguments)]
        let dest = frame.start_address().kernel_vaddr().as_mut_ptr::<u8>();
        unsafe { dest.copy_from(start, len) };
        Self { mapper }
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

    fn change(&mut self, cursor: MappingCursor, settings: &MappingSettings) {
        self.mapper.change(cursor, settings);
    }

    fn unmap(&mut self, mut cursor: MappingCursor) -> DeferredUnmappingOps {
        if cursor.start().raw() == 0 {
            let Some(cursor) = cursor.advance(0x1000) else {
                return Consistency::new_full_global().into_deferred();
            };
            return self.mapper.unmap(cursor);
        }
        self.mapper.unmap(cursor)
    }

    fn shared_map(&mut self, cursor: MappingCursor, spt: &SharedPageTable) -> DeferredUnmappingOps {
        self.mapper.shared_map(cursor, spt)
    }
}

impl Drop for ArchContextInner {
    fn drop(&mut self) {
        // Unmap all user memory to clear any allocated page tables.
        self.mapper
            .unmap(MappingCursor::new(
                VirtAddr::start_user_memory(),
                VirtAddr::end_user_memory() - VirtAddr::start_user_memory(),
            ))
            .run_all();
        // Manually free the root.
        if let Some(frame) = get_frame(self.mapper.root_address()) {
            free_frame(frame);
        }
    }
}
