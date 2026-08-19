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
    struct Mark(&'static str);

    impl Mark {
        fn new(name: &'static str) -> Self {
            console(&format!("SYSBENCH-MARK begin {}\n", name));
            twizzler_abi::syscall::sys_debug_perfmark(true);
            Self(name)
        }
    }

    impl Drop for Mark {
        fn drop(&mut self) {
            console(&format!("SYSBENCH-MARK end {}\n", self.0));
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

    /// State for soft-fault benches: invalidate the mapping, then take the single fault that
    /// re-attaches it. Region invalidation detaches the whole mapping and the object's page
    /// table keeps its leaf entries, so per-page soft faults can't be provoked individually;
    /// each op is one MapCtrl syscall plus one fault, with no frame allocation.
    struct SoftFault {
        obj: BenchObj,
    }

    impl SoftFault {
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
    fn page_fault_soft(b: &mut Bencher) {
        if !bench_mode() {
            return;
        }
        let _mark = Mark::new("page_fault_soft");
        let mut state = SoftFault::new();
        b.iter(|| state.fault());
    }

    /// Soft faults from every CPU at once, each on its own object: contends on the context's
    /// mapping structures and generates cross-CPU TLB shootdowns.
    #[bench]
    fn page_fault_soft_contended(b: &mut Bencher) {
        if !bench_mode() {
            return;
        }
        let _mark = Mark::new("page_fault_soft_contended");
        contended(b, SoftFault::new, SoftFault::fault);
    }

    /// Page-fault handling for never-touched pages: zero-fill frame allocation + mapping.
    #[bench]
    fn page_fault_zero_fill(b: &mut Bencher) {
        if !bench_mode() {
            return;
        }
        let _mark = Mark::new("page_fault_zero_fill");
        let mut state = ZeroFill::new();
        b.iter(|| state.touch());
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
        // the read path, not a page-in, which `page_fault_soft` already covers.
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
    #[bench]
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
}
