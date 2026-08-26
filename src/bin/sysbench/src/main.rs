//! Microbenchmarks for core system paths (syscall entry, mapping, thread sync, page faults,
//! file open, pager round trips), single-threaded and under multi-thread contention. Run with
//! `cargo start-qemu --bench sysbench` or as part of `--benches`.
#![feature(test)]

fn main() {
    println!("sysbench: run via `cargo start-qemu --bench sysbench` (or --benches)");
}

#[cfg(test)]
mod benches {
    extern crate test;

    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    use test::Bencher;
    use twizzler::object::{Object, ObjectBuilder, RawObject};
    use twizzler_abi::{
        object::{MAX_SIZE, NULLPAGE_SIZE, ObjID, Protections},
        syscall::{
            ClockSource, MapControlCmd, MapFlags, ReadClockFlags, ThreadSync, ThreadSyncFlags,
            ThreadSyncOp, ThreadSyncReference, ThreadSyncSleep, ThreadSyncWake, UnmapFlags,
            sys_map_ctrl, sys_object_map, sys_object_unmap, sys_read_clock_info,
            sys_thread_self_id, sys_thread_sync,
        },
    };
    use twizzler_abi::syscall::{
        DeleteFlags, ObjectControlCmd, ObjectCreate, sys_object_create, sys_object_ctrl,
    };
    use twizzler_rt_abi::{
        io::IoCtx,
        object::{MapFlags as RtMapFlags, ObjectCmd},
    };

    /// The bench bodies would still execute once each under `--test`, and the contended/pager
    /// ones exhaust the pager's request slots and flood the serial log with kernel WARNs
    /// mid-suite (~19 MB in one boot), so every bench is a no-op outside bench mode. The argv
    /// flag is the authority: unittest runs benches as `<bin> --bench` and tests as
    /// `<bin> --test`. TWZ_TEST_MODE cannot distinguish the two -- init exports it around the
    /// whole unittest run (init/main.rs), bench children included -- so it is reported for
    /// auditing but not consulted. The one-shot line goes through the kernel console directly:
    /// libtest captures stdout/stderr inside test bodies and only replays it for failures.
    fn bench_mode() -> bool {
        static ONCE: std::sync::Once = std::sync::Once::new();
        let bench_flag = std::env::args().any(|a| a == "--bench");
        ONCE.call_once(|| {
            let line = format!(
                "sysbench: bench_flag={} TWZ_TEST_MODE={} -> {}\n",
                bench_flag,
                std::env::var_os("TWZ_TEST_MODE").is_some(),
                if bench_flag {
                    "running benches"
                } else {
                    "skipping bench bodies"
                }
            );
            twizzler_abi::syscall::sys_kernel_console_write(
                twizzler_abi::syscall::KernelConsoleSource::Console,
                line.as_bytes(),
                twizzler_abi::syscall::KernelConsoleWriteFlags::DONT_BUFFER,
            );
        });
        bench_flag
    }

    /// Brackets one bench so the kernel's per-phase profile deltas can be attributed by name.
    ///
    /// The console writes and the kernel's own `PERFMARK` lines go to the same serial stream in
    /// order, so the name does not have to be handed to the kernel. Only the interval boundaries
    /// matter: nothing is reset, so a mark that lands in the wrong place costs a line of output.
    ///
    /// It also counts the kernel events the bench actually caused, which is not decoration.
    /// `sysbench.md`'s own method note -- "Count the events; do not infer them from the total" --
    /// was written after two Linux numbers turned out to be measuring 16x fewer faults than the
    /// per-touch divisor claimed. The fault benches here have the same exposure from the other
    /// side: `page_fault_soft`'s doc comment *asserted* that each iteration takes one fault, and
    /// nothing has ever checked it. `page_fault_count` and `tlb_shootdown_count` come from
    /// `MemoryStats`, are maintained whether or not `FAULT_PROFILE` is on, and cost two syscalls
    /// per bench rather than per iteration -- so this is readable in a timing arm without
    /// perturbing it.
    ///
    /// Two things to know when reading the numbers. They are **system-wide**, so another
    /// compartment faulting during the interval is included; on an otherwise-idle bench boot that
    /// is small, but it means a nonzero count near zero is not proof the bench itself faulted.
    /// And `iters` is only nonzero for benches that call [`Mark::tick`] in their loop -- libtest
    /// does not expose its iteration count, so a per-iteration rate cannot be had any other way.
    ///
    /// **`shootdowns` and `flushes` are not the same question**, and reading only the first led me
    /// to a wrong conclusion once already. `tlb_shootdown_inc_count` bumps `flushes` on every
    /// invalidation it is asked to perform, and `shootdowns` only when the remote target count is
    /// nonzero. So on a single-threaded bench `shootdowns` reads ~0 no matter how much local
    /// invalidation work happened, and it is `flushes` that says whether an invalidation did
    /// anything at all.
    struct Mark {
        name: &'static str,
        faults: usize,
        shootdowns: usize,
        flushes: usize,
        switch_flush: usize,
        switch_noflush: usize,
        iters: AtomicU64,
    }

    impl Mark {
        fn new(name: &'static str) -> Self {
            console(&format!("SYSBENCH-MARK begin {}\n", name));
            twizzler_abi::syscall::sys_debug_perfmark(true);
            let m = twizzler_abi::syscall::sys_memory_stats();
            Self {
                name,
                faults: m.page_fault_count,
                shootdowns: m.tlb_shootdown_count,
                flushes: m.tlb_flush_count,
                switch_flush: m.aspace_switch_flush_count,
                switch_noflush: m.aspace_switch_noflush_count,
                iters: AtomicU64::new(0),
            }
        }

        /// Count one iteration of the bench body, so the event deltas have a measured denominator.
        fn tick(&self) {
            self.iters.fetch_add(1, Ordering::Relaxed);
        }
    }

    impl Drop for Mark {
        fn drop(&mut self) {
            let m = twizzler_abi::syscall::sys_memory_stats();
            console(&format!(
                "SYSBENCH-EVENTS {} iters={} faults={} shootdowns={} flushes={} sw_flush={} sw_noflush={}\n",
                self.name,
                self.iters.load(Ordering::Relaxed),
                m.page_fault_count.saturating_sub(self.faults),
                m.tlb_shootdown_count.saturating_sub(self.shootdowns),
                m.tlb_flush_count.saturating_sub(self.flushes),
                m.aspace_switch_flush_count.saturating_sub(self.switch_flush),
                m.aspace_switch_noflush_count.saturating_sub(self.switch_noflush),
            ));
            console(&format!("SYSBENCH-MARK end {}\n", self.name));
            twizzler_abi::syscall::sys_debug_perfmark(false);
        }
    }

    fn console(s: &str) {
        twizzler_abi::syscall::sys_kernel_console_write(
            twizzler_abi::syscall::KernelConsoleSource::Console,
            s.as_bytes(),
            twizzler_abi::syscall::KernelConsoleWriteFlags::DONT_BUFFER,
        );
    }

    fn wake(word: &AtomicU64, count: usize) {
        let mut ops = [ThreadSync::new_wake(ThreadSyncWake::new(
            ThreadSyncReference::Virtual(word),
            count,
        ))];
        sys_thread_sync(&mut ops, None).unwrap();
    }

    /// Sleep while `*word == val`.
    fn wait(word: &AtomicU64, val: u64) {
        let mut ops = [ThreadSync::new_sleep(ThreadSyncSleep::new(
            ThreadSyncReference::Virtual(word),
            val,
            ThreadSyncOp::Equal,
            ThreadSyncFlags::empty(),
        ))];
        sys_thread_sync(&mut ops, None).unwrap();
    }

    /// An object that is deleted when dropped.
    struct BenchObj(Option<Object<()>>);

    impl BenchObj {
        fn new_volatile() -> Self {
            Self(Some(ObjectBuilder::default().build(()).unwrap()))
        }

        fn new_persistent() -> Self {
            Self(Some(
                ObjectBuilder::default().persist(true).build(()).unwrap(),
            ))
        }

        fn obj(&self) -> &Object<()> {
            self.0.as_ref().unwrap()
        }

        /// First data page past the null and base pages.
        fn data(&self) -> *mut u8 {
            unsafe { self.obj().handle().start().add(2 * NULLPAGE_SIZE) }
        }

        fn sync(&self) {
            self.obj()
                .handle()
                .cmd(ObjectCmd::Sync, core::ptr::null_mut::<()>())
                .unwrap();
        }
    }

    impl Drop for BenchObj {
        fn drop(&mut self) {
            self.0
                .take()
                .unwrap()
                .into_handle()
                .cmd(ObjectCmd::Delete, core::ptr::null_mut::<()>())
                .unwrap();
        }
    }

    /// Bench `op` on this thread while one worker thread per remaining CPU hammers the same
    /// operation on its own state. On a single-CPU boot one worker still contends via
    /// preemption.
    fn contended<S: Send>(
        b: &mut Bencher,
        make: impl Fn() -> S + Sync,
        op: impl Fn(&mut S) + Sync,
    ) {
        let workers = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
            .saturating_sub(1)
            .max(1);
        let stop = AtomicBool::new(false);
        // Set on every exit path, including unwinds: a panic that skips the store would leave
        // the workers spinning and hang the scope join (and with it the whole bench run).
        struct StopGuard<'a>(&'a AtomicBool);
        impl Drop for StopGuard<'_> {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Relaxed);
            }
        }
        std::thread::scope(|s| {
            let _guard = StopGuard(&stop);
            for _ in 0..workers {
                s.spawn(|| {
                    let _guard = StopGuard(&stop);
                    let mut state = make();
                    while !stop.load(Ordering::Relaxed) {
                        op(&mut state);
                    }
                });
            }
            let mut state = make();
            b.iter(|| op(&mut state));
        });
    }

    /// State for zero-fill fault benches: touch never-touched pages, rotating to a fresh object
    /// once per WINDOW so frames are reclaimed (amortized).
    struct ZeroFill {
        obj: BenchObj,
        i: usize,
    }

    impl ZeroFill {
        const WINDOW: usize = 1024;

        fn new() -> Self {
            Self {
                obj: BenchObj::new_volatile(),
                i: 0,
            }
        }

        fn touch(&mut self) {
            if self.i == Self::WINDOW {
                self.i = 0;
                self.obj = BenchObj::new_volatile();
            }
            unsafe {
                self.obj
                    .data()
                    .add(self.i * NULLPAGE_SIZE)
                    .write_volatile(1)
            };
            self.i += 1;
        }
    }

    /// State for the `map_ctrl_invalidate` benches.
    ///
    /// **Renamed from `SoftFault`/`page_fault_soft` on 2026-08-23, because that name documented a
    /// claim the bench does not satisfy.** The old doc said "each op is one MapCtrl syscall plus
    /// one fault". Measured with `SYSBENCH-EVENTS`: **8 page faults across 3,855,601 iterations**.
    /// `MapControlCmd::Invalidate` reaches `ObjectPageTable::invalidate`, which builds TLB
    /// invalidations and removes no page-table entry, so the write afterwards re-walks a still
    /// valid translation and does not fault. `tlb_flush_count` moves by exactly 1.000 per
    /// iteration, so the invalidation itself is real and local.
    ///
    /// What the op measures is therefore: one `sys_map_ctrl`, one local TLB invalidation over the
    /// object's mapped range, and one non-faulting write. See `pageperf.md` §2.
    ///
    /// The name was changed rather than the bench fixed because **Twizzler has no per-page soft
    /// fault to provoke**: a whole object is mapped by one object-table entry, so the minor fault
    /// Linux takes per page happens at most once per (object, security context). Numbers under the
    /// old name in `reapbatch.md`, `reapqueue.md`, `ocdperf.md` and `perf-inprogress.md` are
    /// measurements of *this* operation and are not comparable to anyone's page-fault figure.
    struct MapCtrlInvalidate {
        obj: BenchObj,
    }

    impl MapCtrlInvalidate {
        fn new() -> Self {
            let this = Self {
                obj: BenchObj::new_volatile(),
            };
            unsafe { this.obj.data().write_volatile(1) };
            this
        }

        fn fault(&mut self) {
            let start = self.obj.obj().handle().start();
            sys_map_ctrl(start, MAX_SIZE, MapControlCmd::Invalidate, 0).unwrap();
            unsafe { self.obj.data().write_volatile(1) };
        }

        /// Same op with a *read* instead of a write.
        ///
        /// Added to price `maybe_cow_at`, which `handle_fault` runs only for
        /// `MemoryAccessKind::Write`. It answered the question by coming out **equal** (970 vs 922
        /// ns, overlapping): the COW check cannot be costing anything here because no fault
        /// happens at all. Kept as the standing control for that -- a write and a read diverging
        /// in future would mean the access started faulting again.
        fn fault_read(&mut self) {
            let start = self.obj.obj().handle().start();
            sys_map_ctrl(start, MAX_SIZE, MapControlCmd::Invalidate, 0).unwrap();
            std::hint::black_box(unsafe { self.obj.data().read_volatile() });
        }

        /// The syscall alone, without the write.
        ///
        /// This is what showed that the write contributes almost nothing: 846 ns here against 922
        /// for the pair. `invls` is *not* empty between iterations -- `flushes` reads 1.000 per
        /// iteration in both -- so this is a real invalidation, not an early return.
        fn invalidate_only(&mut self) {
            let start = self.obj.obj().handle().start();
            sys_map_ctrl(start, MAX_SIZE, MapControlCmd::Invalidate, 0).unwrap();
        }
    }

    /// State for raw map/unmap benches: an object and a slot we own. The initial runtime handle
    /// is leaked so the runtime's slot allocator never reuses the slot underneath us.
    struct MapSlot {
        id: ObjID,
        slot: usize,
    }

    impl MapSlot {
        fn new() -> Self {
            let obj = ObjectBuilder::<()>::default().build(()).unwrap();
            let id = obj.id();
            let slot = obj.handle().start() as usize / MAX_SIZE;
            std::mem::forget(obj);
            Self { id, slot }
        }

        fn cycle(&mut self) {
            sys_object_unmap(None, self.slot, UnmapFlags::empty()).unwrap();
            sys_object_map(
                None,
                self.id,
                self.slot,
                Protections::READ | Protections::WRITE,
                MapFlags::empty(),
            )
            .unwrap();
        }
    }

    /// State for pager sync benches: dirty one page, then sync it to disk.
    struct DirtySync {
        obj: BenchObj,
    }

    impl DirtySync {
        fn new() -> Self {
            Self {
                obj: BenchObj::new_persistent(),
            }
        }

        fn sync(&mut self) {
            unsafe { self.obj.data().write_volatile(1) };
            self.obj.sync();
        }
    }

    /// CORRECTNESS PROBE, not a perf bench. Proves whether an object unmap fails to evict the
    /// local CPU's leaf TLB entries for touched pages other than the slot's base page.
    ///
    /// Mechanism under test: unmapping an object mapping removes the object-table *link* and
    /// enqueues exactly one non-terminal `invlpg` at the slot base (offset 0). A page touched at
    /// any other offset keeps its leaf TLB entry on the CPU that ran the target PCID. Mapping a
    /// different object into the same slot installs a new link but enqueues nothing (not-present ->
    /// present), so a read of the previously-touched VA can hit the stale entry and return the
    /// *old* object's frame.
    ///
    /// **Read-only on the test slot, deliberately.** A/B keep a permanent mapping in their own
    /// slots (Sa/Sb), where their sentinels at OFF are written once and never disturbed. The test
    /// slot S only ever *reads* — so a stale result is unambiguously a stale read, never a write
    /// that landed in the wrong frame through an already-stale entry (which would prove the same
    /// bug but muddy the mechanism). Each iteration: map A into S, read OFF (returns SA, caches
    /// VA_S(OFF) -> A's frame), unmap A from S, map B into S, read OFF again. Correct == SB (walk
    /// reached B's frame); stale == SA (TLB shortcut to A's frame). A and B map to distinct frames,
    /// so SA vs SB is a clean discriminator.
    ///
    /// Deterministic on smp1 (no migration); may be probabilistic on smp>1, so the count is printed
    /// before the assert. Result is independent of host CPU load — a functional test, not a timing
    /// one. Expected to FAIL on a kernel without the unmap flush/granularity fix; pass after it.
    #[bench]
    fn tlb_stale_slot_reuse(_b: &mut Bencher) {
        if !bench_mode() {
            return;
        }
        let _mark = Mark::new("tlb_stale_slot_reuse");
        // Twizzler backs object data with 2 MiB large frames, so a touched page shares a huge-page
        // TLB entry with everything else in its 2 MiB region -- and the unmap's single invlpg lands
        // at the slot base (region 0). An offset inside region 0 is covered by that invlpg whether
        // or not the bug exists (a false negative). These offsets each sit in a DISTINCT 2 MiB
        // region well past the base, so the base invlpg cannot cover them; a stale read at any of
        // them is the bug. Multiple offsets so one accidentally-covered region can't hide it.
        const TWO_MIB: usize = 0x20_0000;
        const OFFS: [usize; 5] = [2 * TWO_MIB, 6 * TWO_MIB, 10 * TWO_MIB, 20 * TWO_MIB, 40 * TWO_MIB];
        const SA: u64 = 0xA1A1_A1A1_A1A1_A1A1;
        const SB: u64 = 0xB2B2_B2B2_B2B2_B2B2;
        let map = |id: ObjID, slot: usize| {
            sys_object_map(
                None,
                id,
                slot,
                Protections::READ | Protections::WRITE,
                MapFlags::empty(),
            )
            .unwrap();
        };
        let unmap = |slot: usize| sys_object_unmap(None, slot, UnmapFlags::empty()).unwrap();
        // Claim a slot we own by leaking a throwaway mapping's handle, then unmap it so the index
        // is free for our explicit maps (mirrors MapSlot's slot ownership).
        let claim_slot = || {
            let owner = ObjectBuilder::<()>::default().build(()).unwrap();
            let slot = owner.handle().start() as usize / MAX_SIZE;
            std::mem::forget(owner);
            unmap(slot);
            slot
        };

        let a = BenchObj::new_volatile();
        let b = BenchObj::new_volatile();
        let (a_id, b_id) = (a.obj().id(), b.obj().id());

        // A and B each keep a permanent mapping in their own slot; their OFF sentinels are written
        // once here and never touched again, so their frames stay pristine for the whole run.
        let (sa_slot, sb_slot, s) = (claim_slot(), claim_slot(), claim_slot());
        map(a_id, sa_slot);
        map(b_id, sb_slot);
        for &off in &OFFS {
            unsafe {
                ((sa_slot * MAX_SIZE + off) as *mut u64).write_volatile(SA);
                ((sb_slot * MAX_SIZE + off) as *mut u64).write_volatile(SB);
            }
        }

        let iters = 1024usize;
        let (mut stale, mut other, mut setup_bad) = (0usize, 0usize, 0usize);
        let mut stale_by_off = [0usize; OFFS.len()];
        for _ in 0..iters {
            map(a_id, s);
            // Read (not write) each offset: caches VA_S(off) -> A's frame; returns SA.
            let mut ra_ok = true;
            for &off in &OFFS {
                let ra = unsafe { ((s * MAX_SIZE + off) as *const u64).read_volatile() };
                ra_ok &= ra == SA;
            }
            if !ra_ok {
                setup_bad += 1;
            }
            unmap(s);
            map(b_id, s);
            for (k, &off) in OFFS.iter().enumerate() {
                let got = unsafe { ((s * MAX_SIZE + off) as *const u64).read_volatile() };
                if got == SA {
                    stale += 1;
                    stale_by_off[k] += 1;
                } else if got != SB {
                    other += 1;
                }
            }
            unmap(s);
        }
        console(&format!(
            "SYSBENCH-TLBSTALE stale={} other={} setup_bad={} / {} iters x {} offs; by_off={:?}\n",
            stale,
            other,
            setup_bad,
            iters,
            OFFS.len(),
            stale_by_off
        ));
        unmap(sa_slot);
        unmap(sb_slot);
        drop(a);
        drop(b);
        assert_eq!(
            stale, 0,
            "stale TLB leaf entry survived slot reuse ({} reads returned the unmapped object's frame; by_off={:?})",
            stale, stale_by_off
        );
    }

    /// Minimal syscall round trip. `sys_null` logs in-kernel, so ThreadCtrl::GetSelfId is the
    /// cheapest quiet entry/exit path.
    #[bench]
    fn syscall_simple(b: &mut Bencher) {
        if !bench_mode() {
            return;
        }
        let _mark = Mark::new("syscall_simple");
        b.iter(|| std::hint::black_box(sys_thread_self_id()));
    }

    #[bench]
    fn syscall_simple_contended(b: &mut Bencher) {
        if !bench_mode() {
            return;
        }
        let _mark = Mark::new("syscall_simple_contended");
        contended(
            b,
            || (),
            |_| {
                std::hint::black_box(sys_thread_self_id());
            },
        );
    }

    /// `Instant::now()`: the runtime's monotonic clock. Should not enter the kernel at all --
    /// the runtime calibrates once against the tick source and reads the CPU tick counter
    /// thereafter, so this measures a tick-counter read plus a multiply and a shift.
    #[bench]
    fn time_monotonic(b: &mut Bencher) {
        if !bench_mode() {
            return;
        }
        let _mark = Mark::new("time_monotonic");
        b.iter(|| std::hint::black_box(std::time::Instant::now()));
    }

    /// The same from every cpu at once. A userspace clock that reads the tick counter has nothing
    /// to contend on, so this should track its uncontended sibling; if it does not, something on
    /// the path is shared. It used to be `sys_read_clock_info` under one global spinlock.
    #[bench]
    fn time_monotonic_contended(b: &mut Bencher) {
        if !bench_mode() {
            return;
        }
        let _mark = Mark::new("time_monotonic_contended");
        contended(
            b,
            || (),
            |_| {
                std::hint::black_box(std::time::Instant::now());
            },
        );
    }

    /// `SystemTime::now()`: the realtime clock, which syscalled on *every* call before the same
    /// calibrate-once treatment was applied to it.
    #[bench]
    fn time_system(b: &mut Bencher) {
        if !bench_mode() {
            return;
        }
        let _mark = Mark::new("time_system");
        b.iter(|| std::hint::black_box(std::time::SystemTime::now()));
    }

    #[bench]
    fn time_system_contended(b: &mut Bencher) {
        if !bench_mode() {
            return;
        }
        let _mark = Mark::new("time_system_contended");
        contended(
            b,
            || (),
            |_| {
                std::hint::black_box(std::time::SystemTime::now());
            },
        );
    }

    /// The kernel clock syscall itself, as the control: it is what the two benches above used to
    /// cost, and it is still what anything that cannot calibrate pays. Contended, it also measures
    /// whether the handler still serializes on `TICK_SOURCES`.
    #[bench]
    fn time_clock_syscall(b: &mut Bencher) {
        if !bench_mode() {
            return;
        }
        let _mark = Mark::new("time_clock_syscall");
        b.iter(|| {
            std::hint::black_box(sys_read_clock_info(
                ClockSource::BestMonotonic,
                ReadClockFlags::empty(),
            ))
        });
    }

    #[bench]
    fn time_clock_syscall_contended(b: &mut Bencher) {
        if !bench_mode() {
            return;
        }
        let _mark = Mark::new("time_clock_syscall_contended");
        contended(
            b,
            || (),
            |_| {
                std::hint::black_box(sys_read_clock_info(
                    ClockSource::BestMonotonic,
                    ReadClockFlags::empty(),
                ));
            },
        );
    }

    /// Full `File::open` path: name resolution through naming-srv plus object mapping. The close
    /// (drop) is included in each iteration.
    #[bench]
    fn file_open(b: &mut Bencher) {
        if !bench_mode() {
            return;
        }
        let _mark = Mark::new("file_open");
        let path = "sysbench.dat";
        std::fs::write(path, b"sysbench").unwrap();
        b.iter(|| {
            std::hint::black_box(std::fs::File::open(path).unwrap());
        });
        let _ = std::fs::remove_file(path);
    }

    /// `File::open` of a file staged on the disk image: name resolution goes through
    /// naming-srv's external namespace, backed by the pager's on-disk store.
    #[bench]
    fn file_open_external(b: &mut Bencher) {
        if !bench_mode() {
            return;
        }
        let _mark = Mark::new("file_open_external");
        let entry = std::fs::read_dir("/pkg/twizzler/bin")
            .unwrap()
            .flatten()
            .next()
            .unwrap()
            .path();
        b.iter(|| {
            std::hint::black_box(std::fs::File::open(&entry).unwrap());
        });
    }

    /// Raw ObjectUnmap + ObjectMap syscall pair on a slot we own.
    #[bench]
    fn object_map_unmap_syscall(b: &mut Bencher) {
        if !bench_mode() {
            return;
        }
        let _mark = Mark::new("object_map_unmap_syscall");
        let mut state = MapSlot::new();
        b.iter(|| state.cycle());
    }

    #[bench]
    fn object_map_unmap_syscall_contended(b: &mut Bencher) {
        if !bench_mode() {
            return;
        }
        let _mark = Mark::new("object_map_unmap_syscall_contended");
        contended(b, MapSlot::new, MapSlot::cycle);
    }

    /// The map path applications actually hit: runtime handle-cache lookup for an
    /// already-mapped object.
    #[bench]
    fn object_map_runtime_cached(b: &mut Bencher) {
        if !bench_mode() {
            return;
        }
        let _mark = Mark::new("object_map_runtime_cached");
        let obj = BenchObj::new_volatile();
        let id = obj.obj().id();
        b.iter(|| {
            let o = Object::<()>::map(id, RtMapFlags::READ | RtMapFlags::WRITE).unwrap();
            std::hint::black_box(&o);
        });
    }

    /// ThreadSync wake on a word nobody is sleeping on: syscall + sleep-queue lookup, no
    /// scheduling.
    #[bench]
    fn thread_sync_wake_no_waiters(b: &mut Bencher) {
        if !bench_mode() {
            return;
        }
        let _mark = Mark::new("thread_sync_wake_no_waiters");
        let word = AtomicU64::new(0);
        b.iter(|| wake(&word, 1));
    }

    /// Same wake path with every other CPU waking the same word: contention on the object's
    /// sleep queue.
    #[bench]
    fn thread_sync_wake_contended(b: &mut Bencher) {
        if !bench_mode() {
            return;
        }
        let _mark = Mark::new("thread_sync_wake_contended");
        let word = AtomicU64::new(0);
        contended(b, || &word, |w| wake(w, 1));
    }

    /// ThreadSync sleep whose condition is already unsatisfied, so it returns without blocking.
    #[bench]
    fn thread_sync_sleep_ready(b: &mut Bencher) {
        if !bench_mode() {
            return;
        }
        let _mark = Mark::new("thread_sync_sleep_ready");
        let word = AtomicU64::new(1);
        b.iter(|| {
            // *word (1) != 0, so the request is immediately ready.
            wait(&word, 0);
        });
    }

    /// Cross-thread wake/sleep round trip: each iteration blocks until a partner thread has been
    /// woken, run, and woken us back. Measures the full sleep + wake + schedule path twice.
    #[bench]
    fn thread_sync_ping_pong(b: &mut Bencher) {
        if !bench_mode() {
            return;
        }
        let _mark = Mark::new("thread_sync_ping_pong");
        const STOP: u64 = u64::MAX;
        let words: &'static (AtomicU64, AtomicU64) =
            Box::leak(Box::new((AtomicU64::new(0), AtomicU64::new(0))));
        let (ping, pong) = (&words.0, &words.1);
        let partner = std::thread::spawn(move || {
            let mut last = 0;
            loop {
                while ping.load(Ordering::Acquire) == last {
                    wait(ping, last);
                }
                let v = ping.load(Ordering::Acquire);
                if v == STOP {
                    break;
                }
                last = v;
                pong.store(v, Ordering::Release);
                wake(pong, 1);
            }
        });
        let mut seq = 0u64;
        b.iter(|| {
            seq += 1;
            ping.store(seq, Ordering::Release);
            wake(ping, 1);
            loop {
                let v = pong.load(Ordering::Acquire);
                if v == seq {
                    break;
                }
                wait(pong, v);
            }
        });
        ping.store(STOP, Ordering::Release);
        wake(ping, 1);
        partner.join().unwrap();
    }

    #[bench]
    fn map_ctrl_invalidate(b: &mut Bencher) {
        if !bench_mode() {
            return;
        }
        let mark = Mark::new("map_ctrl_invalidate");
        let mut state = MapCtrlInvalidate::new();
        b.iter(|| {
            mark.tick();
            state.fault()
        });
    }

    /// The same op from every CPU at once, each on its own object. Unlike the uncontended
    /// version this *does* generate cross-CPU shootdowns -- `tlb_shootdown_count` moves here and
    /// not there -- because the other cpus have the contexts loaded.
    #[bench]
    fn map_ctrl_invalidate_contended(b: &mut Bencher) {
        if !bench_mode() {
            return;
        }
        let _mark = Mark::new("map_ctrl_invalidate_contended");
        contended(b, MapCtrlInvalidate::new, MapCtrlInvalidate::fault);
    }

    /// Control: the same op with a read. Equal to the write version, which is the evidence that
    /// no fault (and so no COW check) is in this path.
    #[bench]
    fn map_ctrl_invalidate_read(b: &mut Bencher) {
        if !bench_mode() {
            return;
        }
        let mark = Mark::new("map_ctrl_invalidate_read");
        let mut state = MapCtrlInvalidate::new();
        b.iter(|| {
            mark.tick();
            state.fault_read()
        });
    }

    /// The `MapControlCmd::Invalidate` syscall on its own: 846 of the pair's 922 ns.
    #[bench]
    fn map_ctrl_invalidate_only(b: &mut Bencher) {
        if !bench_mode() {
            return;
        }
        let mark = Mark::new("map_ctrl_invalidate_only");
        let mut state = MapCtrlInvalidate::new();
        b.iter(|| {
            mark.tick();
            state.invalidate_only()
        });
    }

    /// Page-fault handling for never-touched pages: zero-fill frame allocation + mapping.
    #[bench]
    fn page_fault_zero_fill(b: &mut Bencher) {
        if !bench_mode() {
            return;
        }
        let mark = Mark::new("page_fault_zero_fill");
        let mut state = ZeroFill::new();
        b.iter(|| {
            mark.tick();
            state.touch()
        });
    }

    #[bench]
    fn page_fault_zero_fill_contended(b: &mut Bencher) {
        if !bench_mode() {
            return;
        }
        let _mark = Mark::new("page_fault_zero_fill_contended");
        contended(b, ZeroFill::new, ZeroFill::touch);
    }

    /// Create + delete with **no mapping at all**: the two syscalls, without the map, base write
    /// and unmap that `object_create_delete` bundles around them.
    ///
    /// The pair exists so the create path and the map path can be told apart from userspace.
    /// Splitting them previously needed a kernel-side stage profile, and the answer mattered:
    /// `sys_object_create` and `sys_object_map` are separately ~15 us and ~10 us, with different
    /// causes. A never-mapped object is also the one case that must be reap-eligible the instant
    /// it is marked, so if reaping regresses this bench retains every object it makes and
    /// `PERFMARK-MEM`'s `page=` says so.
    #[bench]
    fn object_create_delete_nomap(b: &mut Bencher) {
        if !bench_mode() {
            return;
        }
        let _mark = Mark::new("object_create_delete_nomap");
        let spec = ObjectCreate::default();
        b.iter(|| {
            let id = sys_object_create(spec, &[], &[]).unwrap();
            sys_object_ctrl(id, ObjectControlCmd::Delete(DeleteFlags::empty()), 0, 0).unwrap();
            std::hint::black_box(id);
        });
    }

    /// Create + map + delete from every CPU at once.
    ///
    /// Three separate global serialization points meet on this path and none of them was measured
    /// under contention: the context-wide `regions` mutex (63% of `sys_object_map`), the global
    /// object id map, and the CSPRNG mutex behind the create nonce.
    #[bench]
    fn object_create_delete_contended(b: &mut Bencher) {
        if !bench_mode() {
            return;
        }
        let _mark = Mark::new("object_create_delete_contended");
        contended(
            b,
            || (),
            |_| {
                std::hint::black_box(BenchObj::new_volatile());
            },
        );
    }

    /// One `pread` of an already-open, already-faulted file: the fd data path with no open, no
    /// seek and no fault in it.
    ///
    /// `file_open` prices opening a file and nothing priced reading one.
    #[bench]
    fn file_read_cached(b: &mut Bencher) {
        if !bench_mode() {
            return;
        }
        let _mark = Mark::new("file_read_cached");
        let path = "sysbench-read.dat";
        std::fs::write(path, &[7u8; 64 * 1024]).unwrap();
        let file = std::fs::File::open(path).unwrap();
        let fd = std::os::fd::AsRawFd::as_raw_fd(&file);
        let mut buf = [0u8; 4096];
        // Read once outside the loop so the object's pages are resident: this is meant to price
        // the read path, not a page-in, which `page_fault_zero_fill` covers.
        let mut warm = IoCtx::new(Some(0), twizzler_rt_abi::io::IoFlags::empty(), None);
        let _ = twizzler_rt_abi::io::twz_rt_fd_pread(fd, &mut buf, &mut warm);
        // Reported as throughput as well as latency: at 4 KiB a read is dominated by the
        // per-call cost, so MB/s here is really "one small read", and the 64 KiB variant below is
        // what says anything about bandwidth. Both are wanted -- the gap between them is the
        // per-call overhead.
        b.bytes = buf.len() as u64;
        b.iter(|| {
            let mut ctx = IoCtx::new(Some(0), twizzler_rt_abi::io::IoFlags::empty(), None);
            std::hint::black_box(twizzler_rt_abi::io::twz_rt_fd_pread(fd, &mut buf, &mut ctx))
        });
        drop(file);
        let _ = std::fs::remove_file(path);
    }

    /// The same read at 64 KiB: large enough that the per-call cost is amortized, so the MB/s
    /// figure is bandwidth rather than call overhead.
    #[bench]
    fn file_read_cached_64k(b: &mut Bencher) {
        if !bench_mode() {
            return;
        }
        let _mark = Mark::new("file_read_cached_64k");
        let path = "sysbench-read64k.dat";
        std::fs::write(path, &[7u8; 64 * 1024]).unwrap();
        let file = std::fs::File::open(path).unwrap();
        let fd = std::os::fd::AsRawFd::as_raw_fd(&file);
        let mut buf = vec![0u8; 64 * 1024];
        let mut warm = IoCtx::new(Some(0), twizzler_rt_abi::io::IoFlags::empty(), None);
        let _ = twizzler_rt_abi::io::twz_rt_fd_pread(fd, &mut buf, &mut warm);
        b.bytes = buf.len() as u64;
        b.iter(|| {
            let mut ctx = IoCtx::new(Some(0), twizzler_rt_abi::io::IoFlags::empty(), None);
            std::hint::black_box(twizzler_rt_abi::io::twz_rt_fd_pread(fd, &mut buf, &mut ctx))
        });
        drop(file);
        let _ = std::fs::remove_file(path);
    }

    /// A name lookup on its own, without the map that `File::open` does afterwards.
    ///
    /// `file_open` is one gate call into naming-srv plus a map, and there was no way to say which
    /// half its cost sat in without subtracting one guess from another.
    #[bench]
    fn naming_get(b: &mut Bencher) {
        if !bench_mode() {
            return;
        }
        let _mark = Mark::new("naming_get");
        let path = "sysbench-name.dat";
        std::fs::write(path, b"sysbench").unwrap();
        let Some(mut namer) = naming::static_naming_factory() else {
            console("naming_get: no naming handle, skipping\n");
            return;
        };
        // Resolve first, and refuse to bench if nothing resolves: a lookup that fails takes the
        // error path, which is not the path this is meant to price and would not be visible as
        // anything but a suspiciously good number.
        let Some(name) = [format!("/initrd/{}", path), path.to_string()]
            .into_iter()
            .find(|n| namer.get(n, naming::GetFlags::empty()).is_ok())
        else {
            console("naming_get: name did not resolve, skipping\n");
            let _ = std::fs::remove_file(path);
            return;
        };
        b.iter(|| std::hint::black_box(namer.get(&name, naming::GetFlags::empty()).is_ok()));
        let _ = std::fs::remove_file(path);
    }

    /// Volatile object create + map (via the builder) followed by delete.
    #[bench]
    fn object_create_delete(b: &mut Bencher) {
        if !bench_mode() {
            return;
        }
        let _mark = Mark::new("object_create_delete");
        b.iter(|| {
            std::hint::black_box(BenchObj::new_volatile());
        });
    }

    /// Persistent object create + delete: both are round trips through the pager.
    #[bench]
    fn pager_create_delete_persistent(b: &mut Bencher) {
        if !bench_mode() {
            return;
        }
        let _mark = Mark::new("pager_create_delete_persistent");
        b.iter(|| {
            std::hint::black_box(BenchObj::new_persistent());
        });
    }

    /// Dirty one page of a persistent object and sync it: pager round trip plus disk write.
    ///
    /// This and its contended sibling drive the pager hard enough to exhaust its request slots
    /// ("out of pager request slots"), and have provoked "pager request slot 0 was recycled
    /// under a waiter (wanted PageData, found SyncRegion)" -- a lost-wakeup shape. That noise is
    /// the reason for the [`bench_mode`] guard; in bench mode it is the workload.
    #[bench]
    fn pager_sync_dirty_page(b: &mut Bencher) {
        if !bench_mode() {
            return;
        }
        let _mark = Mark::new("pager_sync_dirty_page");
        let mut state = DirtySync::new();
        b.iter(|| state.sync());
    }

    /// Concurrent syncs of distinct persistent objects: contention on the kernel-pager queues
    /// and the pager itself. See [`pager_sync_dirty_page`] on the WARNs this provokes.
    ///
    /// **Ignored on purpose, 2026-08-22.** This is where the pager `SyncRegion` wedge family lands:
    /// every boot that hangs, hangs entering this bench, and it costs a full 5m22s timeout and the
    /// whole round -- including every bench after it -- rather than just its own number. Measured
    /// over one afternoon of sweeps it took 5 rounds of ~40, and 2 of the 8 in `many-gf-on` alone,
    /// which is well above the "one per ~20 contended rounds" `sysbench.md` budgets for it.
    ///
    /// `#[ignore]` rather than deletion or a `return`, deliberately: the harness prints it as
    /// `ignored`, so a reader of the log sees a bench that was skipped instead of a bench that
    /// silently vanished -- which is indistinguishable from a scraper miss. Re-enable by deleting
    /// the attribute once the wedge has its own session; it is a real defect and not a bench bug,
    /// and nothing here should be read as having fixed it.
    #[bench]
    #[ignore]
    fn pager_sync_dirty_page_contended(b: &mut Bencher) {
        if !bench_mode() {
            return;
        }
        let _mark = Mark::new("pager_sync_dirty_page_contended");
        contended(b, DirtySync::new, DirtySync::sync);
    }

    /// Cross-compartment secgate call round trip into the pager compartment.
    #[bench]
    fn secgate_pager_disk_len(b: &mut Bencher) {
        if !bench_mode() {
            return;
        }
        let _mark = Mark::new("secgate_pager_disk_len");
        let obj = BenchObj::new_persistent();
        obj.sync();
        let id = obj.obj().id();
        b.iter(|| std::hint::black_box(pager::disk_len(id).unwrap()));
    }


    /// Time `n` spawns individually and print the distribution, not just a mean.
    ///
    /// `compartment_spawn_exit` first read 14.9 ms +/- 9.4 ms -- a +/-63% band, which no A/B and
    /// no cross-OS comparison can survive. libtest reports one number per bench and cannot say
    /// whether that band is symmetric noise, a bimodal split, or a monotonic drift as spawns
    /// accumulate state in `naming-srv` and the reaper. Those three want different responses, and
    /// a mean is identical for all of them. So: record every sample, and print the shape.
    ///
    /// `first10`/`last10` is the drift test specifically -- if spawn N is dearer than spawn 1, the
    /// bench is measuring accumulated state and its headline figure depends on iteration count.
    fn spawn_distribution(label: &str, n: usize, mut one: impl FnMut() -> (u64, u64)) {
        let mut total = Vec::with_capacity(n);
        let mut create = Vec::with_capacity(n);
        let mut reap = Vec::with_capacity(n);
        for _ in 0..n {
            let (c, r) = one();
            create.push(c);
            reap.push(r);
            total.push(c + r);
        }
        let mean = |v: &[u64]| if v.is_empty() { 0 } else { v.iter().sum::<u64>() / v.len() as u64 };
        let first10 = mean(&total[..10.min(total.len())]);
        let last10 = mean(&total[total.len().saturating_sub(10)..]);
        let tmean = mean(&total);
        for v in [&mut total, &mut create, &mut reap] {
            v.sort_unstable();
        }
        let q = |v: &Vec<u64>, f: f64| v[((v.len() - 1) as f64 * f) as usize];
        console(&format!(
            "SYSBENCH-SPAWN {} n={} min={} p50={} p90={} max={} mean={} first10={} last10={}\n",
            label, n, total[0], q(&total, 0.5), q(&total, 0.9),
            total[total.len() - 1], tmean, first10, last10,
        ));
        // The split that says which half to look at. `create` is everything up to `Command::spawn`
        // returning -- the naming lookup, the gate into the monitor, the whole `RunCompLoader`
        // path, and the child's main thread being started. `reap` is the child actually running
        // (runtime entry, ctors, `main`, teardown) plus the parent's wait waking up. They are
        // measured from the same clock in the same iteration, so they sum to the total exactly.
        console(&format!(
            "SYSBENCH-SPAWNSPLIT {} create_p50={} create_mean={} reap_p50={} reap_mean={} create_min={} reap_min={}\n",
            label, q(&create, 0.5), mean(&create), q(&reap, 0.5), mean(&reap),
            create[0], reap[0],
        ));
    }

    /// Path to the do-nothing compartment `compartment_spawn_exit` starts. `src/bin/nullexit` is
    /// `fn main() {}` with no dependencies past the default runtime, so the number is the spawn
    /// machinery rather than the program.
    const NULL_PROG: &str = "/pkg/twizzler/bin/nullexit";
    /// A large, dependency-heavy binary, run through the identical path. The pair is the
    /// measurement: the delta is what dynamic linking a real program adds to a spawn, which a
    /// single number cannot separate from compartment creation itself.
    const BIG_PROG: &str = "/pkg/twizzler/bin/leakcheck";


    /// One spawn and one wait, the unit every spawn bench times.
    fn spawn_once(prog: &str, args: &[&str]) -> (u64, u64) {
        let t0 = std::time::Instant::now();
        let mut child = std::process::Command::new(prog)
            .args(args)
            .spawn()
            .unwrap_or_else(|e| panic!("spawn {prog}: {e}"));
        let t1 = std::time::Instant::now();
        let status = child.wait().unwrap_or_else(|e| panic!("wait {prog}: {e}"));
        let t2 = std::time::Instant::now();
        // A child that failed to start would otherwise time as a *fast* spawn and read as a good
        // result, which is the failure mode `sysbench.md` keeps warning about.
        assert!(status.success(), "{prog} exited {status:?}");
        (
            (t1 - t0).as_nanos() as u64,
            (t2 - t1).as_nanos() as u64,
        )
    }

    /// Start a compartment that immediately exits, and wait for it -- Twizzler's analogue of
    /// `fork`+`exec`+`waitpid`.
    ///
    /// The whole path: `Command::spawn` -> `twz_rt_exec_spawn` -> a naming lookup for the program
    /// -> `monitor_api::CompartmentLoader` -> the monitor loading and relocating every DSO the
    /// program needs -> the child's runtime entry -> `main` returning -> teardown, plus the
    /// parent's wait. It is the most expensive single operation in this suite by two orders of
    /// magnitude, and until now the suite had no number for it at all.
    ///
    /// **Not a like-for-like with Linux even so, and the difference runs in Twizzler's favour on
    /// paper.** Linux's `fork`+`exec` of a *static* binary does no dynamic linking; every spawn
    /// here does, because there is no static-executable path through `Command`. Compare against
    /// the dynamic Linux arm, and read the static one as the floor process creation could reach.
    #[bench]
    fn compartment_spawn_exit(b: &mut Bencher) {
        if !bench_mode() {
            return;
        }
        let mark = Mark::new("compartment_spawn_exit");
        let _ = spawn_once(NULL_PROG, &[]);
        spawn_distribution("nullexit", 40, || spawn_once(NULL_PROG, &[]));
        b.iter(|| {
            mark.tick();
            spawn_once(NULL_PROG, &[])
        });
    }

    /// The same program, exiting via `process::exit` instead of returning from `main`.
    ///
    /// Isolates Rust's teardown from the spawn machinery, and makes the `_big` arm below
    /// interpretable: `leakcheck --child-exit` takes this route, so without this number the
    /// difference between the two would mix "bigger program" with "different exit path".
    #[bench]
    fn compartment_spawn_exit_noteardown(b: &mut Bencher) {
        if !bench_mode() {
            return;
        }
        let mark = Mark::new("compartment_spawn_exit_noteardown");
        let _ = spawn_once(NULL_PROG, &["--exit-now"]);
        spawn_distribution("nullexit_noteardown", 40, || {
            spawn_once(NULL_PROG, &["--exit-now"])
        });
        b.iter(|| {
            mark.tick();
            spawn_once(NULL_PROG, &["--exit-now"])
        });
    }

    /// The same spawn of a large, dependency-heavy program (`leakcheck`, which exits at the top
    /// of `main`).
    ///
    /// Against `compartment_spawn_exit_noteardown` -- same exit path -- the difference is what
    /// the program's own size and DSO set add to a spawn.
    #[bench]
    fn compartment_spawn_exit_big(b: &mut Bencher) {
        if !bench_mode() {
            return;
        }
        let mark = Mark::new("compartment_spawn_exit_big");
        let _ = spawn_once(BIG_PROG, &["--child-exit"]);
        spawn_distribution("leakcheck", 40, || spawn_once(BIG_PROG, &["--child-exit"]));
        b.iter(|| {
            mark.tick();
            spawn_once(BIG_PROG, &["--child-exit"])
        });
    }

}
