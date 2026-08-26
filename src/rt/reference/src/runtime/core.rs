//! Implements the core runtime functions.

use core::mem::MaybeUninit;
use std::{
    collections::BTreeMap,
    ffi::{c_char, c_void, CStr, CString},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::Instant,
};

use dynlink::context::runtime::RuntimeInitInfo;
use monitor_api::{RuntimeThreadControl, SharedCompConfig};
use tracing::Level;
use twizzler_abi::{
    syscall::{sys_get_random, GetRandomFlags},
    upcall::{UpcallFlags, UpcallInfo, UpcallMode, UpcallOptions, UpcallTarget},
};
use twizzler_rt_abi::{
    core::{
        auxv, BasicAux, BasicReturn, CompartmentInitInfo, CtorSet, ExitCode, RuntimeInfo,
        RUNTIME_INIT_COMP, RUNTIME_INIT_MONITOR,
    },
    info::SystemInfo,
    time::Monotonicity,
};

use super::{slot::mark_slot_reserved, thread::TLS_GEN_MGR, ReferenceRuntime};
use crate::{
    preinit::{preinit_abort, preinit_unwrap},
    preinit_println,
    runtime::{thread::libc_init_tcb, RuntimeState},
    OUR_RUNTIME,
};

#[derive(Copy, Clone)]
struct PtrToInfo(*mut c_void);
unsafe impl Send for PtrToInfo {}
unsafe impl Sync for PtrToInfo {}
static MON_RTINFO: OnceLock<Option<PtrToInfo>> = OnceLock::new();

fn search_envs_enabled(envp: *mut *mut c_char, name: &str) -> bool {
    unsafe {
        let mut env = envp;
        while !env.is_null() && !(*env).is_null() {
            let s = std::ffi::CStr::from_ptr(*env);
            if s.to_str().unwrap_or("").starts_with(name) {
                return true;
            }
            env = env.add(1);
        }
        false
    }
}

extern "C" {
    fn __mlibc_handle_thread_exit(pointer: *mut u8, ret_val: i32);
}

/// DIAG: which heap object backs `ptr`, if any.
///
/// The census names the object a leak lands in; this names it from the *allocation* side, so a
/// per-spawn cost can be matched to the thing that pays it (heap block, stack, TLS region) rather
/// than inferred from a page count. Writes the id as two halves because `u128` across an
/// `extern "C"` boundary is not worth the argument. Not part of the runtime ABI.
#[no_mangle]
pub extern "C-unwind" fn __twz_rt_diag_heap_id(ptr: *const u8, hi: *mut u64, lo: *mut u64) -> u32 {
    match OUR_RUNTIME.get_id_from_heap_ptr(ptr) {
        Some(id) => {
            let raw: u128 = id.raw();
            unsafe {
                *hi = (raw >> 64) as u64;
                *lo = raw as u64;
            }
            1
        }
        None => 0,
    }
}

/// Run mlibc's pthread-key destructors against `tp`'s TCB.
///
/// Nothing on the spawn path reaches mlibc's own `thread_exit`, so without this call a key's
/// destructor never runs for a thread this compartment spawned: `trampoline` -> std's
/// `thread_start` -> `twz_rt_exit` -> `sys_thread_exit`. std's Rust-side TLS destructors are
/// unaffected -- `thread_start` drains those itself, on the thread, which is where they belong.
///
/// The one that matters is ferroc's: it releases a thread's heap by registering
/// `ThreadLocal::put(id)` through `pthread_key_create`, and recycling that id is what lets the next
/// spawn reuse the dead thread's context and slabs. Without it every spawn took a fresh 4 MiB slab
/// and never gave it back -- measured at exactly `SLAB_SIZE` of fresh address space per spawn.
pub(crate) fn run_mlibc_thread_dtors(tp: *mut u8, code: i32) {
    if !tp.is_null() {
        unsafe { __mlibc_handle_thread_exit(tp, code) };
    }
}

/// Skip `set_naming_namespace` when the target is already the root namespace.
///
/// A compartment does not signal `COMP_READY` until `pre_main_hook` returns, so everything here is
/// inside `Command::spawn`. This one call measured **2,954 us of a 6,126 us spawn** (48%), of which
/// 2,896 us was acquiring a naming handle -- a compartment lookup plus a dynamic-gate resolution
/// plus a server-side buffer, paid by every compartment at startup whether or not it ever names
/// anything.
///
/// Setting the namespace to "/" changes nothing: `NameRoot::Current` is set to "/" just above,
/// and a freshly-created server handle already sits at the root -- with a single handle per
/// runtime there is no pool to re-sync, so skipping the call skips a whole gate round-trip.
/// `TWZ_RT_INITIAL_DIR` is the parent's cwd, so this fires whenever the parent is at the root --
/// the common case.
///
/// **This defers rather than deletes for a program that does use naming**: such a program acquires
/// the handle at its first lookup instead. A do-nothing child never pays it at all, so the spawn
/// benchmarks see the full saving and a real workload will see less.
///
/// On by default. Set it `false` to restore the unconditional call -- which is also the A/B, since
/// the two arms differ by exactly this line. Measured `-45.6%` on `compartment_spawn_exit`
/// (6,301,106 -> 3,427,608 ns, disjoint ranges, four alternating arms); see `spawnbench.md` §14.
const SKIP_ROOT_NAMESPACE_SET: bool = true;

/// Switch for the child-side startup phase counter (`PREMAIN`): subscriber / fds / naming.
///
/// A compartment does not signal `COMP_READY` until `pre_main_hook` returns, so everything it does
/// is inside the monitor's `start` phase and inside `Command::spawn`. Off by default.
pub const PRE_MAIN_PHASE_STATS: bool = false;

/// Switch for the spawn-latency join (`CHILDTOP` here, `SPAWNGO` in the monitor's
/// `start_main_thread`). **Flip both together** -- they are two crates and one measurement.
///
/// Prices the window the spawn breakdown could only reach by subtraction: from `sys_spawn`
/// returning in the parent to the child's first instruction in `init_for_compartment`. That
/// residual read ~190 us and was filed as "sched latency", but a residual inherits every other
/// phase's error and the kernel's own wake->run histogram says wakes cost tens of microseconds,
/// not hundreds -- so the label is a hypothesis, not a measurement.
///
/// Deliberately separate from [`PRE_MAIN_PHASE_STATS`]: that switch also arms `CTORONE`, ~15
/// records per spawn, and a ring drain landing inside the window would inflate the very number
/// this exists to read (see the `statlog` first-drain artifact that cost ~600 us of a spawn).
/// This is one record per side per spawn.
pub const SPAWN_LAT_STATS: bool = false;

impl ReferenceRuntime {
    #[track_caller]
    pub fn exit(&self, code: i32) -> ! {
        if self.state().contains(RuntimeState::READY) {
            let id = crate::runtime::thread::with_current_thread(|ct| ct.id());
            if id == 1 {
                OUR_RUNTIME.close_fds();
                // `process::exit` skips post_main_hook, and statlog no longer drains on a fresh
                // ring's first record -- so flush here or an exit-now program's records are lost.
                // Free when the ring is empty.
                secgate::statlog::drain();
            }
            twizzler_abi::syscall::sys_thread_exit(code as u64);
        } else {
            preinit_println!("runtime exit before runtime ready: {}", code);
            preinit_abort();
        }
    }

    pub fn gc(&self) {
        self.gc_threads();
        self.heap_gc();
        self.gc_object_cache();
    }

    pub fn abort(&self) -> ! {
        if self.state().contains(RuntimeState::READY) {
            preinit_abort();
        } else {
            preinit_println!("runtime abort before runtime ready");
            preinit_abort();
        }
    }

    pub fn is_monitor(&self) -> Option<*mut c_void> {
        MON_RTINFO
            .get()
            .as_ref()
            .unwrap()
            .map(|x| x.0 as *mut _ as *mut c_void)
    }

    pub fn cgetenv(&self, name: &CStr) -> *const c_char {
        // TODO: this approach is very simple, but it leaks if the environment changes a lot.
        static ENVMAP: Mutex<BTreeMap<String, CString>> = Mutex::new(BTreeMap::new());
        let Ok(name) = name.to_str() else {
            return core::ptr::null();
        };
        let Ok(val) = std::env::var(name) else {
            return core::ptr::null();
        };
        let mut envmap = ENVMAP.lock().unwrap();
        envmap
            .entry(val.to_string())
            .or_insert_with(|| CString::new(val.to_string()).unwrap())
            .as_ptr()
    }

    pub fn runtime_entry(
        &self,
        rtinfo: *const RuntimeInfo,
        std_entry: unsafe extern "C-unwind" fn(BasicAux) -> BasicReturn,
        main: usize,
    ) {
        if OUR_RUNTIME.state().contains(RuntimeState::READY) {
            return;
        }
        let rtinfo = unsafe { rtinfo.as_ref().unwrap() };
        match rtinfo.kind {
            RUNTIME_INIT_MONITOR => {
                let init_info = unsafe {
                    rtinfo
                        .init_info
                        .monitor
                        .cast::<RuntimeInitInfo>()
                        .as_ref()
                        .unwrap()
                };
                let _ = MON_RTINFO.set(Some(PtrToInfo(init_info as *const _ as *mut _)));
                self.init_for_monitor(init_info);
            }
            RUNTIME_INIT_COMP => {
                let init_info = unsafe {
                    rtinfo
                        .init_info
                        .comp
                        .cast::<CompartmentInitInfo>()
                        .as_ref()
                        .unwrap()
                };
                let _ = MON_RTINFO.set(None);
                let mut entry_stack = Vec::new();
                entry_stack.push(rtinfo.argc);
                if !rtinfo.args.is_null() {
                    for arg in unsafe { core::slice::from_raw_parts(rtinfo.args, rtinfo.argc) } {
                        entry_stack.push(*arg as usize);
                    }
                }
                entry_stack.push(0);
                if !rtinfo.envp.is_null() {
                    let mut envp = rtinfo.envp;
                    while !envp.is_null() && !unsafe { (*envp).is_null() } {
                        entry_stack.push(unsafe { *envp } as usize);
                        envp = unsafe { envp.add(1) };
                    }
                }
                entry_stack.push(0);
                // The aux vector is not optional: a libc finds it by walking past the envp
                // terminator and reading pairs until AT_NULL, so omitting it means getauxval()
                // walks off the end of this Vec. See twizzler_rt_abi::core::auxv.
                entry_stack.extend_from_slice(&auxv::entries(self.sysinfo().page_size));
                self.init_for_compartment(init_info, entry_stack.as_mut_ptr());
                std::mem::forget(entry_stack);
            }
            x => {
                preinit_println!("unsupported runtime kind: {}", x);
                preinit_abort();
            }
        }

        let mut null_env: [*mut c_char; 4] = [
            b"RUST_BACKTRACE=1\0".as_ptr() as *mut c_char,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        ];
        let env_ptr = if rtinfo.envp.is_null() {
            (&mut null_env).as_mut_ptr()
        } else {
            rtinfo.envp
        };

        if !unsafe { __twz_enable_libc_trace.is_null() } {
            let twz_enable_libc_trace =
                unsafe { std::mem::transmute::<_, extern "C" fn()>(__twz_enable_libc_trace) };

            if search_envs_enabled(env_ptr, "TWZ_LIBC_TRACE") {
                twz_enable_libc_trace();
            }
        }

        // Step 3: call into libstd to finish setting up the standard library and call main
        let ba = BasicAux {
            argc: rtinfo.argc,
            args: rtinfo.args,
            env: env_ptr,
            entry: main,
        };

        let ret = unsafe { std_entry(ba) };
        self.exit(ret.code);
    }

    pub fn pre_main_hook(&self) -> Option<ExitCode> {
        use twizzler_rt_abi::fd::NameRoot;
        let _t0 = std::time::Instant::now();
        // TODO: control this with env vars
        tracing::subscriber::set_global_default(
            tracing_subscriber::fmt()
                .with_max_level(Level::INFO)
                .finish(),
        )
        .unwrap();
        let _t_sub = _t0.elapsed();
        if self.state().contains(RuntimeState::IS_MONITOR) {
            self.init_slots();
            None
        } else {
            unsafe { self.set_runtime_ready() };
            OUR_RUNTIME.init_fds();
            let _t_fds = _t0.elapsed();

            let mut nr = self.nameroots.lock();
            //twizzler_abi::klog_println!("got: {:?}", std::env::var("TWZ_RT_INITIAL_DIR"));

            let current_dir = std::env::var("TWZ_RT_INITIAL_DIR").unwrap_or("/".to_string());

            // TODO
            nr.insert(NameRoot::Home, PathBuf::from("/"));
            nr.insert(NameRoot::Root, PathBuf::from("/"));
            nr.insert(NameRoot::Exe, PathBuf::from("/"));
            nr.insert(NameRoot::Temp, PathBuf::from("/tmp"));
            nr.insert(NameRoot::Current, PathBuf::from("/"));
            if !(SKIP_ROOT_NAMESPACE_SET && current_dir == "/")
                && crate::runtime::file::set_naming_namespace(Path::new(&current_dir)).is_ok()
            {
                nr.insert(NameRoot::Current, Path::new(&current_dir).to_path_buf());
            }
            let _t_naming = _t0.elapsed();
            // Everything here runs *before* the compartment signals READY, so it is inside the
            // monitor's `start` phase -- the largest remaining item in a spawn. Deferred through
            // statlog (drained at `post_main_hook`) so the measurement is not itself a console
            // write inside the thing being measured. Microseconds.
            secgate::statlog::record_on(
                PRE_MAIN_PHASE_STATS,
                "PREMAIN",
                _t_naming.as_micros() as u64,
                &[
                    _t_sub.as_micros() as u64,
                    (_t_fds - _t_sub).as_micros() as u64,
                    (_t_naming - _t_fds).as_micros() as u64,
                ],
            );

            let ret = match monitor_api::monitor_rt_comp_ctrl(
                monitor_api::MonitorCompControlCmd::RuntimeReady,
            ) {
                Ok(ret) => ret,
                _ => self.abort(),
            };
            ret
        }
    }

    pub fn post_main_hook(&self) {
        // Temporary (pagerperf.md): a program compartment's counters would otherwise sit in the
        // ring unprinted, since it exits long before the ring fills.
        secgate::statlog::drain();
        crate::runtime::object::mapstats::report();
        monitor_api::monitor_rt_comp_ctrl(monitor_api::MonitorCompControlCmd::RuntimePostMain)
            .unwrap();
    }

    pub fn sysinfo(&self) -> SystemInfo {
        let info = twizzler_abi::syscall::sys_info();
        SystemInfo {
            clock_monotonicity: Monotonicity::Weak.into(),
            available_parallelism: info.cpu_count().into(),
            page_size: info.page_size(),
        }
    }

    pub fn get_random(&self, buf: &mut [MaybeUninit<u8>], flags: GetRandomFlags) -> usize {
        // TODO: Once the Randomness PR is in, fix this.
        let out = sys_get_random(buf, flags).expect("failed to get randomness from kernel");
        out
    }
}

impl ReferenceRuntime {
    fn init_for_monitor(&self, init_info: &RuntimeInitInfo) {
        let upcall_target = UpcallTarget::new(
            Some(
                twizzler_rt_abi::arch::__twz_rt_upcall_entry
                    as unsafe extern "C-unwind" fn(_, _) -> !,
            ),
            Some(twizzler_rt_abi::arch::__twz_rt_upcall_entry),
            0,
            0,
            0,
            0.into(),
            0.into(),
            [UpcallOptions {
                flags: UpcallFlags::empty(),
                mode: UpcallMode::CallSelf,
            }; UpcallInfo::NR_UPCALLS],
        );
        twizzler_abi::syscall::sys_thread_set_upcall(upcall_target).unwrap();
        self.set_is_monitor();
        self.init_allocator(init_info);
        self.init_tls(init_info);
        self.init_ctors(&init_info.ctors);
    }

    fn init_for_compartment(&self, init_info: &CompartmentInitInfo, entry_stack: *mut usize) {
        let _start_1 = Instant::now();
        // Absolute, in the same monotonic domain the monitor's records are stamped in, so the two
        // sides join without assuming anything about `Instant`'s epoch. Taken here rather than
        // derived from the record's own timestamp minus its value: a syscall between the two
        // (`sys_thread_self_id`, below) would silently land in the gap. Pure clock read -- no gate
        // call -- so it is safe this early, before `set_comp_config`.
        let _t0_ns = if SPAWN_LAT_STATS {
            secgate::now_ns()
        } else {
            0
        };
        unsafe {
            preinit_unwrap(
                monitor_api::set_comp_config(
                    (init_info.comp_config_info as *const SharedCompConfig)
                        .as_ref()
                        .unwrap(),
                )
                .ok(),
            );
        }

        let mut tg = TLS_GEN_MGR.lock();
        let _start_2 = Instant::now();
        let tls = tg.get_next_tls_info(None, || RuntimeThreadControl::new(0));
        let (tls, tls_layout, tls_alloc_base) = preinit_unwrap(tls);
        // `init_core_thread` below maps this thread's repr through a monitor gate, and a
        // runtime-global lock held across a gate call is the shape that deadlocked `comp_lookup`
        // against `THREAD_MGR` (see `thread::mgr::impl_spawn`). Only `get_next_tls_info` needs the
        // manager, so it is released here rather than at the end of the function.
        drop(tg);
        let _start_3 = Instant::now();
        twizzler_abi::syscall::sys_thread_settls(tls as u64);
        let _start_4 = Instant::now();
        twizzler_abi::upcall::set_self_upcall_ptr(crate::arch::twz_rt_upcall_entry_c).unwrap();
        let _start_5 = Instant::now();
        libc_init_tcb(tls);
        self.init_core_thread(tls, tls_alloc_base, tls_layout);
        if !unsafe { __mlibc_entry_from_rust.is_null() } {
            let mlibc_entry_from_rust = unsafe {
                std::mem::transmute::<_, extern "C" fn(*mut usize, *mut u8)>(
                    __mlibc_entry_from_rust,
                )
            };
            mlibc_entry_from_rust(entry_stack, core::ptr::null_mut());
        }
        let _start_6 = Instant::now();

        if !init_info.ctor_set_array.is_null() && init_info.ctor_set_len != 0 {
            let ctor_slice = unsafe {
                core::slice::from_raw_parts(init_info.ctor_set_array, init_info.ctor_set_len)
            };
            self.init_ctors(ctor_slice);
        }

        // Child-side bring-up split (`CHILDINI`): comp-config+TLS-lock / get_tls / set_tls /
        // set_upcall / libc+core-thread+mlibc / ctors. The diag sweep put ~1.8ms of a ~3.5ms spawn
        // between thread start and `pre_main_hook`, and this is most of that window; the record's
        // own timestamp (vs `PREMAIN`'s) prices the libstd init that follows. Same switch as
        // `PREMAIN`, deferred through statlog.
        if SPAWN_LAT_STATS {
            // vals: [absolute start us, low 64 bits of this thread's id]. The id is the join key
            // to the monitor's `SPAWNGO`; pairing by "nearest preceding record" instead would be
            // an assumption that spawns never overlap, which is true for the nullexit bench and
            // false in general -- and it would fail silently rather than loudly.
            let me = twizzler_abi::syscall::sys_thread_self_id();
            secgate::statlog::record_on(
                SPAWN_LAT_STATS,
                "CHILDTOP",
                _start_1.elapsed().as_micros() as u64,
                &[_t0_ns / 1000, me.raw() as u64],
            );
        }
        secgate::statlog::record_on(
            PRE_MAIN_PHASE_STATS,
            "CHILDINI",
            _start_1.elapsed().as_micros() as u64,
            &[
                (_start_2 - _start_1).as_micros() as u64,
                (_start_3 - _start_2).as_micros() as u64,
                (_start_4 - _start_3).as_micros() as u64,
                (_start_5 - _start_4).as_micros() as u64,
                (_start_6 - _start_5).as_micros() as u64,
                _start_6.elapsed().as_micros() as u64,
            ],
        );
    }

    fn init_ctors(&self, ctor_array: &[CtorSet]) {
        for (seti, ctor) in ctor_array.iter().enumerate() {
            unsafe {
                if let Some(legacy_init) = ctor.legacy_init {
                    let _t = std::time::Instant::now();
                    (core::mem::transmute::<_, extern "C" fn()>(legacy_init))();
                    // Per-ctor timing (`CTORONE`): set index, entry index (MAX = legacy DT_INIT),
                    // entry address. CHILDINI put ~876us of a spawn in this loop; this names the
                    // ctor. Same switch as PREMAIN/CHILDINI.
                    secgate::statlog::record_on(
                        PRE_MAIN_PHASE_STATS,
                        "CTORONE",
                        _t.elapsed().as_micros() as u64,
                        &[seti as u64, u64::MAX, legacy_init as usize as u64],
                    );
                }
                if !ctor.init_array.is_null() && ctor.init_array_len > 0 {
                    let init_slice: &[usize] = core::slice::from_raw_parts(
                        ctor.init_array as *const usize,
                        ctor.init_array_len,
                    );
                    for (calli, call) in init_slice.iter().cloned().enumerate() {
                        let _t = std::time::Instant::now();
                        (core::mem::transmute::<_, extern "C" fn()>(call))();
                        secgate::statlog::record_on(
                            PRE_MAIN_PHASE_STATS,
                            "CTORONE",
                            _t.elapsed().as_micros() as u64,
                            &[seti as u64, calli as u64, call as u64],
                        );
                    }
                }
            }
        }
    }

    fn init_allocator(&self, info: &RuntimeInitInfo) {
        for slot in &info.used_slots {
            mark_slot_reserved(*slot);
        }
        self.register_bootstrap_alloc(info.bootstrap_alloc_slot);
    }

    fn init_tls(&self, info: &RuntimeInitInfo) {
        let tls = info.tls_region.get_thread_pointer_value();
        twizzler_abi::syscall::sys_thread_settls(tls as u64);
    }
}

extern "C" {
    #[linkage = "extern_weak"]
    static __mlibc_entry_from_rust: *mut u8;
    #[linkage = "extern_weak"]
    static __twz_enable_libc_trace: *mut u8;
}

use twizzler_rt_abi::core::rt0::__rust_entry_from_c;
#[cfg(target_arch = "aarch64")]
#[unsafe(no_mangle)]
#[unsafe(naked)]
pub unsafe extern "C" fn _start() {
    core::arch::naked_asm!(
        "b {entry}",
        entry = sym __rust_entry_from_c,
    );
}

#[cfg(target_arch = "x86_64")]
#[unsafe(no_mangle)]
#[unsafe(naked)]
pub unsafe extern "C" fn _start() {
    // Align the stack and jump to rust code. If we come back, trigger an exception.
    core::arch::naked_asm!(
        "and rsp, 0xfffffffffffffff0",
        "call {entry}",
        "ud2",
        entry = sym __rust_entry_from_c,
    );
}

#[used]
// Ensure the compiler doesn't optimize us away!
static ENTRY: unsafe extern "C" fn() = _start;
