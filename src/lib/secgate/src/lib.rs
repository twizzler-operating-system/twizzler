#![feature(fn_traits)]
#![feature(unboxed_closures)]
#![feature(tuple_trait)]
#![feature(auto_traits)]
#![feature(negative_impls)]
#![feature(linkage)]
#![feature(maybe_uninit_as_bytes)]
#![feature(thread_local)]

use core::ffi::{c_char, CStr};
use std::{
    cell::{Cell, UnsafeCell},
    fmt::Debug,
    marker::{PhantomData, Tuple},
    mem::MaybeUninit,
    sync::OnceLock,
};

pub use secgate_macros::*;
use twizzler_abi::object::ObjID;
pub use twizzler_rt_abi::error::{ResourceError, TwzError};

pub mod util;

/// Shared reporting cadence for the temporary counters in the open path (pagerperf.md).
///
/// Power-of-two was the cadence everywhere, and it hides any phase that starts past the last
/// report: `pagepar`'s 16 opens land after naming-srv's 64th `get` and the monitor's 256th map,
/// and the next report would be at 128/512, which the run never reaches. Differencing consecutive
/// reports is how these counters are read, so a phase with no report inside it is unmeasurable.
///
/// This reports eight times per doubling instead, which bounds the output the same way
/// power-of-two does -- a counter at 16k emits every 2048, not every call -- while guaranteeing a
/// report within 1/8 of the current count, so any phase worth a name gets one inside it.
pub mod statcadence {
    /// Master switch for every counter built on [`report_now`], [`crate::statline`], and
    /// [`crate::statlog`].
    ///
    /// Off. These are the temporary counters from the `pagerperf.md`/`sysperf.md` rounds, and left
    /// on they are not a passive observer: the last measured boot spent ~1,500 of its ~2,500
    /// console lines on them, at roughly a millisecond of emulated-16550 time per line, plus a
    /// clock read per record. Anything measuring a boot is measuring those too. Flip to `true` for
    /// a run that needs them; the call sites all stay in place.
    pub const STATS_ON: bool = false;

    /// Reports per doubling. 1 reproduces the original power-of-two cadence.
    ///
    /// Raising this used to be first-order expensive, because a report *was* a console write --
    /// milliseconds-class on an emulated 16550, under a kernel-wide serial lock. At 8, `pagepar`'s
    /// read phase went from 8 lines to ~40 and its summed open time from 75 ms to ~118 ms: the
    /// counters were charging ~40% of the number they reported.
    ///
    /// [crate::statlog] removes that coupling -- a record is now an atomic increment and a few
    /// stores, and the console traffic happens in one batch when the ring fills -- so this can be
    /// raised for a run that needs a short phase bracketed. Counters still on [statline] are
    /// unchanged and still pay per report.
    pub const REPORTS_PER_OCTAVE: u64 = 32;

    pub fn report_now(n: u64) -> bool {
        if !STATS_ON || n == 0 {
            return false;
        }
        let prev_pow2 = 1u64 << (u64::BITS - 1 - n.leading_zeros());
        n % (prev_pow2 / REPORTS_PER_OCTAVE).max(1) == 0
    }

    /// A console line under construction, flushed in whole writes.
    ///
    /// Sized well past one record so a drain batches several lines per syscall: the kernel takes
    /// the serial lock and polls the UART per byte, so the write count matters almost as much as
    /// the byte count.
    pub(crate) struct Line {
        buf: [u8; 1024],
        len: usize,
    }

    impl Line {
        pub(crate) fn new() -> Self {
            Self {
                buf: [0; 1024],
                len: 0,
            }
        }

        /// End the current line, flushing if there is no room for another.
        pub(crate) fn newline(&mut self) {
            if self.len < self.buf.len() {
                self.buf[self.len] = b'\n';
                self.len += 1;
            }
            if self.len > self.buf.len() - 256 {
                self.emit();
            }
        }

        /// Write out whatever has accumulated.
        pub(crate) fn emit(&mut self) {
            if self.len == 0 {
                return;
            }
            twizzler_abi::syscall::sys_kernel_console_write(
                twizzler_abi::syscall::KernelConsoleSource::Console,
                &self.buf[..self.len],
                twizzler_abi::syscall::KernelConsoleWriteFlags::empty(),
            );
            self.len = 0;
        }
    }

    impl core::fmt::Write for Line {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            let n = s.len().min(self.buf.len() - self.len);
            self.buf[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
            self.len += n;
            Ok(())
        }
    }

    /// Emit one counter line in a single console write.
    ///
    /// `klog_println!` writes each formatted fragment with its own syscall, and the kernel takes
    /// the serial lock per write, so two threads reporting at once produce one garbled line and
    /// both counters are lost. Buffering the whole line and issuing one write makes a report atomic
    /// against other reporters.
    pub fn report(args: core::fmt::Arguments) {
        if !STATS_ON {
            return;
        }
        report_forced(args);
    }

    /// [report], for a counter carrying its own switch rather than [STATS_ON].
    pub fn report_forced(args: core::fmt::Arguments) {
        use core::fmt::Write;
        let mut line = Line::new();
        let _ = line.write_fmt(args);
        if line.len < line.buf.len() {
            line.buf[line.len] = b'\n';
            line.len += 1;
        }
        line.emit();
    }
}

/// `klog_println!` for a counter line, in one console write. See [statcadence::report].
#[macro_export]
macro_rules! statline {
    ($($arg:tt)*) => {
        $crate::statcadence::report(format_args!($($arg)*))
    };
}

/// In-memory counter log: record now, print later.
///
/// The problem this solves is not the total cost of the output, it is *when* the output happens.
/// A counter that formats and writes to the console on every report charges milliseconds of serial
/// I/O to whatever phase it is reporting from, which is exactly the phase being measured -- see
/// [statcadence::REPORTS_PER_OCTAVE] for the measurement of that.
///
/// So recording is decoupled from printing. A record is an atomic index bump and a few relaxed
/// stores into a static ring, with **no formatting and no syscall**; the ring is drained to the
/// console in one batch when it fills. Each record carries its own timestamp, so a drain that
/// happens long after the fact still attributes correctly -- which is what makes the deferral safe
/// rather than merely cheaper.
///
/// Records are `STATLOG <tag> <sctx> <n> <ts_us> <v0..>`, deliberately unformatted; post-process
/// them on the host.
pub mod statlog {
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

    /// Records held before a drain. Sized so a whole run fits without a forced drain: a drain
    /// inside a measured phase perturbs it, which is what the marker is there to catch.
    pub const CAP: usize = 16384;
    /// Values per record, past the tag/sctx/n/timestamp header.
    pub const VALS: usize = 6;
    const WORDS: usize = 4 + VALS;

    static RING: [AtomicU64; CAP * WORDS] = [const { AtomicU64::new(0) }; CAP * WORDS];
    static IDX: AtomicUsize = AtomicUsize::new(0);
    static DRAINING: AtomicBool = AtomicBool::new(false);
    static DROPPED: AtomicU64 = AtomicU64::new(0);
    static LAST_DRAIN: AtomicU64 = AtomicU64::new(0);
    /// How often a compartment that never fills the ring flushes it anyway.
    const DRAIN_INTERVAL_NS: u64 = 2_000_000_000;

    /// First 8 bytes of the counter name, as a word, so a record needs no pointer to outlive it.
    const fn tag8(name: &str) -> u64 {
        let b = name.as_bytes();
        let mut w = 0u64;
        let mut i = 0;
        while i < 8 && i < b.len() {
            w |= (b[i] as u64) << (i * 8);
            i += 1;
        }
        w
    }

    /// Append one record. Cheap enough to call from anywhere the counter itself is cheap.
    pub fn record(tag: &str, n: u64, vals: &[u64]) {
        record_on(false, tag, n, vals)
    }

    /// [record_on], for a caller that must not reference the monitor.
    ///
    /// `record_on` stamps each record with [`super::get_sctx_id`], which calls
    /// `__is_monitor_ready` -- a symbol only the monitor defines. A counter in a crate that is also
    /// linked into `bootstrap` (dynlink is) therefore fails to *link* even when the counter is
    /// switched off, because the call site still exists. `cargo check` does not link, so this shows
    /// up only in a real build. Records land with sctx 0.
    pub fn record_on_anon(on: bool, tag: &str, n: u64, vals: &[u64]) {
        record_inner(on, tag, n, vals, 0)
    }

    /// [record], for a counter with its own switch.
    ///
    /// [super::statcadence::STATS_ON] is global: flipping it to measure one path turns on every
    /// counter in the tree, which is both a lot of console traffic and a change to what everyone
    /// else's runs are measuring. A counter investigating one path takes its switch as an argument
    /// instead, so a run measuring the spawn path perturbs only the spawn path.
    pub fn record_on(on: bool, tag: &str, n: u64, vals: &[u64]) {
        if !on && !super::statcadence::STATS_ON {
            return;
        }
        record_inner(on, tag, n, vals, super::get_sctx_id().raw() as u64 & 0xffff)
    }

    fn record_inner(on: bool, tag: &str, n: u64, vals: &[u64], sctx: u64) {
        if !on && !super::statcadence::STATS_ON {
            return;
        }
        let i = IDX.fetch_add(1, Ordering::Relaxed);
        if i >= CAP {
            // The ring is full and someone is (or should be) draining it.
            if !DRAINING.load(Ordering::Relaxed) {
                drain();
            } else {
                DROPPED.fetch_add(1, Ordering::Relaxed);
            }
            return;
        }
        let base = i * WORDS;
        RING[base].store(tag8(tag), Ordering::Relaxed);
        RING[base + 1].store(sctx, Ordering::Relaxed);
        RING[base + 2].store(n, Ordering::Relaxed);
        let now = super::now_ns();
        RING[base + 3].store(now / 1000, Ordering::Relaxed);
        for k in 0..VALS {
            RING[base + 4 + k].store(vals.get(k).copied().unwrap_or(u64::MAX), Ordering::Relaxed);
        }
        if i + 1 == CAP {
            drain();
            return;
        }
        // A long-lived server never exits and never fills the ring, so without this its records
        // for any phase of interest sit here unprinted forever -- which is exactly what happened
        // to naming-srv's and the monitor's on the first run of this. Draining on an interval
        // instead means the console traffic is periodic and rare rather than tied to the work, and
        // the drain marker says whether one landed inside a measured phase.
        let last = LAST_DRAIN.load(Ordering::Relaxed);
        if last == 0 {
            // Fresh ring (a just-started compartment): start the interval now rather than
            // draining. The old behavior paid a console write to print exactly one record,
            // and it landed inside whatever spawn phase took the first record -- measured at
            // ~600us, mis-attributed to the child's post-ctor startup in `spawndiag2` until
            // `spawndiag3` moved it into the ctor window and exposed it.
            let _ = LAST_DRAIN.compare_exchange(0, now, Ordering::Relaxed, Ordering::Relaxed);
            return;
        }
        if now.saturating_sub(last) > DRAIN_INTERVAL_NS
            && LAST_DRAIN
                .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            drain();
        }
    }

    /// Drain if the interval has elapsed. Cheap when it has not: one clock read and a compare.
    pub fn drain_if_due() {
        if IDX.load(Ordering::Relaxed) == 0 {
            return;
        }
        let now = super::now_ns();
        let last = LAST_DRAIN.load(Ordering::Relaxed);
        if now.saturating_sub(last) > DRAIN_INTERVAL_NS
            && LAST_DRAIN
                .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            drain();
        }
    }

    /// Format and emit everything recorded so far, then reset the ring.
    ///
    /// Safe to call at any time; only one caller drains and the rest drop records while it runs,
    /// which is counted and reported rather than silent.
    pub fn drain() {
        // Deliberately not gated on `STATS_ON`: with `record_on`, records can be present when it
        // is off, and a record that is never drained is a record that was never taken.
        if DRAINING.swap(true, Ordering::Acquire) {
            return;
        }
        let count = IDX.load(Ordering::Relaxed).min(CAP);
        if count == 0 {
            DRAINING.store(false, Ordering::Release);
            return;
        }
        // Marks when the console traffic happened, as opposed to when the records were taken. A
        // drain that lands inside a measured phase perturbs it; this is how you find out that it
        // did, instead of quietly folding it into the result.
        super::statcadence::report_forced(format_args!(
            "STATLOG drain {} records at {} us",
            count,
            super::now_ns() / 1000
        ));
        let mut line = super::statcadence::Line::new();
        for i in 0..count {
            let base = i * WORDS;
            let tag = RING[base].load(Ordering::Relaxed).to_le_bytes();
            let tag = core::str::from_utf8(&tag)
                .unwrap_or("?")
                .trim_end_matches('\0');
            use core::fmt::Write;
            let _ = write!(
                line,
                "STATLOG {} {:x} {} {}",
                tag,
                RING[base + 1].load(Ordering::Relaxed),
                RING[base + 2].load(Ordering::Relaxed),
                RING[base + 3].load(Ordering::Relaxed),
            );
            for k in 0..VALS {
                let v = RING[base + 4 + k].load(Ordering::Relaxed);
                if v == u64::MAX {
                    break;
                }
                let _ = write!(line, " {}", v);
            }
            line.newline();
        }
        line.emit();
        let dropped = DROPPED.swap(0, Ordering::Relaxed);
        if dropped > 0 {
            super::statcadence::report_forced(format_args!(
                "STATLOG dropped {} records while draining",
                dropped
            ));
        }
        IDX.store(0, Ordering::Relaxed);
        DRAINING.store(false, Ordering::Release);
    }
}

/// A struct of information about a secure gate. These are auto-generated by the
/// [crate::entry] macro, and stored in a special ELF section (.twz_secgate_info) as an array.
/// The dynamic linker and monitor can then use this to easily enumerate gates.
#[repr(C)]
pub struct SecGateInfo<F> {
    /// A pointer to the implementation entry function. This must be a pointer, and we statically
    /// check that is has the same size as usize (sorry cheri, we'll fix this another time)
    pub imp: F,
    /// The name of this secure gate. This must be a pointer to a null-terminated C string.
    name: *const c_char,
}

impl<F> core::fmt::Debug for SecGateInfo<F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SecGateInfo({:p})", self.name)
    }
}

impl<F> SecGateInfo<F> {
    pub const fn new(imp: F, name: &'static CStr) -> Self {
        Self {
            imp,
            name: name.as_ptr(),
        }
    }

    pub fn name(&self) -> &CStr {
        // Safety: we only ever construct self from a static CStr.
        unsafe { CStr::from_ptr(self.name) }
    }
}

// Safety: If F is Send, we are too because the name field points to a static C string that cannot
// be written to.
unsafe impl<F: Send> Send for SecGateInfo<F> {}
// Safety: If F is Sync, we are too because the name field points to a static C string that cannot
// be written to.
unsafe impl<F: Sync> Sync for SecGateInfo<F> {}

/// Minimum alignment of secure trampolines.
pub const SECGATE_TRAMPOLINE_ALIGN: usize = 0x10;

/// Non-generic and non-pointer-based SecGateInfo, for use during dynamic linking.
pub type RawSecGateInfo = SecGateInfo<usize>;
// Ensure that these are the same size because the dynamic linker uses the raw variant.
static_assertions::assert_eq_size!(RawSecGateInfo, SecGateInfo<&fn()>);

/// Arguments that will be passed to the secure call. Concrete versions of this are generated by the
/// macro.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct Arguments<Args: Tuple + Crossing + Copy> {
    args: Args,
}

impl<Args: Tuple + Crossing + Copy> Arguments<Args> {
    pub fn with_alloca<F, R>(args: Args, f: F) -> R
    where
        F: FnOnce(&mut Self) -> R,
    {
        alloca::alloca(|stack_space| {
            stack_space.write(Self { args });
            // Safety: we init the MaybeUninit just above.
            f(unsafe { stack_space.assume_init_mut() })
        })
    }

    pub fn into_inner(self) -> Args {
        self.args
    }
}

/// Return value to be filled by the secure call. Concrete versions of this are generated by the
/// macro.
#[derive(Copy)]
#[repr(C)]
pub struct Return<T: Crossing + Copy> {
    isset: bool,
    ret: MaybeUninit<T>,
}

impl<T: Copy + Crossing> Clone for Return<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: Crossing + Copy> Return<T> {
    pub fn with_alloca<F, R>(f: F) -> R
    where
        F: FnOnce(&mut Self) -> R,
    {
        alloca::alloca(|stack_space| {
            stack_space.write(Self {
                isset: false,
                ret: MaybeUninit::uninit(),
            });
            // Safety: we init the MaybeUninit just above.
            f(unsafe { stack_space.assume_init_mut() })
        })
    }

    /// If a previous call to set is made, or this was constructed by new(), then into_inner
    /// returns the inner value. Otherwise, returns None.
    pub fn into_inner(self) -> Option<T> {
        if self.isset {
            Some(unsafe { self.ret.assume_init() })
        } else {
            None
        }
    }

    /// Construct a new, uninitialized Self.
    pub fn new_uninit() -> Self {
        Self {
            isset: false,
            ret: MaybeUninit::uninit(),
        }
    }

    /// Set the inner value. Future call to into_inner will return Some(val).
    pub fn set(&mut self, val: T) {
        self.ret.write(val);
        self.isset = true;
    }
}

/// An auto trait that limits the types that can be send across to another compartment. These are:
/// 1. Types other than references, UnsafeCell, raw pointers, slices.
/// 2. #[repr(C)] structs and enums made from Crossing types.
///
/// # Safety
/// The type must meet the above requirements.
pub unsafe auto trait Crossing {}

impl<T> !Crossing for &T {}
impl<T> !Crossing for &mut T {}
impl<T: ?Sized> !Crossing for UnsafeCell<T> {}
impl<T> !Crossing for *const T {}
impl<T> !Crossing for *mut T {}
impl<T> !Crossing for &[T] {}
impl<T> !Crossing for &mut [T] {}

unsafe impl<T: Crossing + Copy> Crossing for Result<T, TwzError> {}

/// Required to put in your source if you call any secure gates.
// TODO: this isn't ideal, but it's the only solution I have at the moment. For some reason,
// the linker doesn't even bother linking the libcalloca.a library that alloca creates. This forces
// that to happen.
#[macro_export]
macro_rules! secgate_prelude {
    () => {
        #[link(name = "calloca", kind = "static")]
        extern "C" {
            pub fn c_with_alloca();
        }
    };
}

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Ord, Eq, Hash)]
#[repr(C)]
pub struct GateCallInfo {
    thread_id: ObjID,
    src_ctx: ObjID,
    /// Monotonic nanoseconds at which the caller handed off, for the callee to subtract (see
    /// [GateCallInfo::inbound_transit_ns]). Temporary, pagerperf.md.
    call_start: u64,
}

/// The monotonic clock, in nanoseconds. One tick-counter read and a multiply once the runtime has
/// cached the tickrate; the base is global, so two reads from different compartments are directly
/// comparable.
pub fn now_ns() -> u64 {
    twizzler_rt_abi::time::twz_rt_get_monotonic_time().as_nanos() as u64
}

impl GateCallInfo {
    /// Allocate a new GateCallInfo on the stack for the closure.
    pub fn with_alloca<F, R>(thread_id: ObjID, src_ctx: ObjID, f: F) -> R
    where
        F: FnOnce(&mut Self) -> R,
    {
        // Stamped here rather than by the caller stubs: every gate call, static and dynamic, goes
        // through this function, and this is the last point before the trampoline runs. Zero when
        // the stats are off, which `inbound_transit_ns` already reads as "no stamp".
        let call_start = if crate::statcadence::STATS_ON {
            twizzler_rt_abi::time::twz_rt_get_monotonic_time().as_nanos() as u64
        } else {
            0
        };
        alloca::alloca(|stack_space| {
            stack_space.write(Self {
                thread_id,
                src_ctx,
                call_start,
            });
            // Safety: we init the MaybeUninit just above.
            f(unsafe { stack_space.assume_init_mut() })
        })
    }

    /// Nanoseconds between the caller stamping this and the callee reading it -- the inbound half
    /// of the compartment transition, which no timer on either side covers.
    ///
    /// The monotonic clock is the raw tick counter scaled by a tickrate the kernel reports, with no
    /// per-compartment base, so the two reads are directly comparable. Returns `None` for a call
    /// with no stamp (a zeroed or pre-existing `GateCallInfo`) rather than a bogus interval.
    pub fn inbound_transit_ns(&self) -> Option<u64> {
        if self.call_start == 0 {
            return None;
        }
        Some(now_ns().saturating_sub(self.call_start))
    }

    /// Get the ID of the source context, or None if the call was not cross-context.
    pub fn source_context(&self) -> Option<ObjID> {
        if self.src_ctx.raw() == 0 {
            None
        } else {
            Some(self.src_ctx)
        }
    }

    /// Get the ID of the calling thread.
    pub fn thread_id(&self) -> ObjID {
        if self.thread_id.raw() == 0 {
            twizzler_abi::syscall::sys_thread_self_id()
        } else {
            self.thread_id
        }
    }

    /// Ensures that the data is filled out (may read thread ID from kernel if necessary).
    pub fn canonicalize(self) -> Self {
        Self {
            thread_id: self.thread_id(),
            src_ctx: self.src_ctx,
            call_start: self.call_start,
        }
    }
}

fn get_tp() -> usize {
    let mut val: usize;
    unsafe {
        #[cfg(target_arch = "x86_64")]
        core::arch::asm!("rdfsbase {}", out(reg) val);
        #[cfg(not(target_arch = "x86_64"))]
        core::arch::asm!("mrs {}, tpidr_el0", out(reg) val);
    }
    val
}

/// Get the thread ID of the caller.
pub fn get_thread_id() -> ObjID {
    if !unsafe { __is_monitor_ready() } {
        return twizzler_abi::syscall::sys_thread_self_id();
    }
    #[thread_local]
    static ONCE_ID: OnceLock<ObjID> = OnceLock::new();
    if get_tp() != 0 {
        *ONCE_ID.get_or_init(|| twizzler_abi::syscall::sys_thread_self_id())
    } else {
        twizzler_abi::syscall::sys_thread_self_id()
    }
}

/// Get the thread ID of the caller.
pub fn get_sctx_id() -> ObjID {
    if !unsafe { __is_monitor_ready() } {
        return twizzler_abi::syscall::sys_thread_active_sctx_id();
    }
    #[thread_local]
    static ONCE_ID: OnceLock<ObjID> = OnceLock::new();
    if get_tp() != 0 {
        *ONCE_ID.get_or_init(|| twizzler_abi::syscall::sys_thread_active_sctx_id())
    } else {
        twizzler_abi::syscall::sys_thread_active_sctx_id()
    }
}

pub fn runtime_preentry(info: &GateCallInfo) -> Result<(), TwzError> {
    // Before the entry work, so `transit` is the transition alone and `entry` is what
    // `cross_compartment_entry` adds on top of it.
    // Three clock reads on the entry side of every gate call, static and dynamic, when
    // unconditional -- `transitstats` is the only consumer and it is off.
    let transit_ns = statcadence::STATS_ON.then(|| info.inbound_transit_ns()).flatten();
    let t_entry = statcadence::STATS_ON.then(twizzler_rt_abi::time::twz_rt_get_monotonic_time);
    let res = twizzler_rt_abi::core::twz_rt_cross_compartment_entry();
    let t_done = statcadence::STATS_ON.then(twizzler_rt_abi::time::twz_rt_get_monotonic_time);
    res?;
    // Reported only after the entry call has returned: a cold entry runs with no usable thread
    // pointer until then, and the report path is not worth auditing for that. The transit value is
    // still the one taken before it.
    if let (Some(transit), Some(t_entry), Some(t_done)) = (transit_ns, t_entry, t_done) {
        transitstats::record(transit, t_done.saturating_sub(t_entry).as_nanos() as u64);
    }
    // Servers are busiest exactly when there is something to measure, so the interval check rides
    // gate entry as well as `record`: a counter that reports rarely still gets its ring flushed.
    statlog::drain_if_due();
    set_caller(info.clone());
    Ok(())
}

/// Temporary instrumentation for the File::open latency hunt (pagerperf.md): the inbound half of a
/// compartment transition, which neither the caller's nor the callee's timers cover.
pub mod transitstats {
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNT: AtomicU64 = AtomicU64::new(0);
    static TRANSIT: AtomicU64 = AtomicU64::new(0);
    static ENTRY: AtomicU64 = AtomicU64::new(0);

    pub fn record(transit: u64, entry: u64) {
        // The reporting below is compiled out when stats are off, but these accumulators are not:
        // three lock-prefixed RMWs on adjacent statics, on every gate entry from every thread, for
        // a total nothing reads. Gate them on the same switch so measurement and reporting go
        // together.
        if !super::statcadence::STATS_ON {
            return;
        }
        let n = COUNT.fetch_add(1, Ordering::Relaxed) + 1;
        let t = TRANSIT.fetch_add(transit, Ordering::Relaxed) + transit;
        let e = ENTRY.fetch_add(entry, Ordering::Relaxed) + entry;
        if super::statcadence::report_now(n) {
            crate::statlog::record("TRANSITS", n, &[t / 1000, e / 1000]);
        }
    }
}

pub struct SecFrame {
    tp: usize,
    sctx: ObjID,
}

/// Snapshot the caller's compartment context, to be reinstalled by [restore_frame] once the gate
/// returns.
///
/// The sctx read used to be a syscall on every gate call, for a value the caller stub then went on
/// to ask [get_sctx_id] for anyway. A thread running a compartment's own code is always active in
/// that compartment's context -- entry switches to the callee's and this restores the caller's --
/// so the answer is a per-(thread, compartment) constant, which is exactly what `get_sctx_id`
/// memoizes in this compartment's TLS. It still falls back to the kernel when there is no usable
/// thread pointer to memoize into.
pub fn frame() -> SecFrame {
    let tp = get_tp();
    let sctx = get_sctx_id();
    SecFrame { tp, sctx }
}

pub fn restore_frame(frame: SecFrame) {
    // The kernel tracks a thread pointer per (thread, security context), so switching back also
    // restores this compartment's TLS -- one syscall for both halves of the frame. The check is an
    // `rdfsbase` and covers the paths where no switch happened on the way in (a callee entered
    // from the monitor never runs `cross_compartment_entry`), where the kernel has no swap to
    // undo. Restoring a pointer we did not leave with would be a compartment running on another
    // compartment's TLS, so it is worth the branch.
    twizzler_abi::syscall::sys_thread_set_active_sctx_id(frame.sctx).unwrap();
    if frame.tp != 0 && get_tp() != frame.tp {
        twizzler_abi::syscall::sys_thread_settls(frame.tp as u64);
    }
}

#[derive(Clone, Copy)]
pub struct DynamicSecGate<'comp, A, R> {
    address: usize,
    _pd: PhantomData<&'comp (A, R)>,
}

impl<'a, A: Tuple + Crossing + Copy, R: Crossing + Copy> Fn<A> for DynamicSecGate<'a, A, R> {
    extern "rust-call" fn call(&self, args: A) -> Self::Output {
        unsafe { dynamic_gate_call(*self, args) }
    }
}

impl<'a, A: Tuple + Crossing + Copy, R: Crossing + Copy> FnMut<A> for DynamicSecGate<'a, A, R> {
    extern "rust-call" fn call_mut(&mut self, args: A) -> Self::Output {
        unsafe { dynamic_gate_call(*self, args) }
    }
}

impl<'a, A: Tuple + Crossing + Copy, R: Crossing + Copy> FnOnce<A> for DynamicSecGate<'a, A, R> {
    type Output = Result<R, TwzError>;

    extern "rust-call" fn call_once(self, args: A) -> Self::Output {
        unsafe { dynamic_gate_call(self, args) }
    }
}

impl<'a, A, R> Debug for DynamicSecGate<'a, A, R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "DynamicSecGate [{} -> {}] {{ address: {:x} }}",
            std::any::type_name::<A>(),
            std::any::type_name::<R>(),
            self.address
        )
    }
}

impl<'comp, A, R> DynamicSecGate<'comp, A, R> {
    pub unsafe fn new(address: usize) -> Self {
        Self {
            address,
            _pd: PhantomData,
        }
    }
}

// Temporary instrumentation for the File::open latency hunt (pagerperf.md).
pub mod gatestats {
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNT: AtomicU64 = AtomicU64::new(0);
    static FRAME: AtomicU64 = AtomicU64::new(0);
    static CALL: AtomicU64 = AtomicU64::new(0);
    static RESTORE: AtomicU64 = AtomicU64::new(0);
    pub fn record(frame: u64, call: u64, restore: u64) {
        // See the note in `transitstats::record`: four RMWs on adjacent statics per dynamic gate
        // call, whose sums this function then discards outright.
        if !super::statcadence::STATS_ON {
            return;
        }
        let n = COUNT.fetch_add(1, Ordering::Relaxed) + 1;
        let f = FRAME.fetch_add(frame, Ordering::Relaxed) + frame;
        let c = CALL.fetch_add(call, Ordering::Relaxed) + call;
        let r = RESTORE.fetch_add(restore, Ordering::Relaxed) + restore;
        // Every call; see the note in naming-core's NAMEGET. Values are ns here, not us: the
        // spans being compared are tens of microseconds and rounding to us loses the answer.
        crate::statlog::record("GATESTAT", n, &[frame, call, restore]);
        let _ = (f, c, r);
    }
}

pub unsafe fn dynamic_gate_call<A: Tuple + Crossing + Copy, R: Crossing + Copy>(
    target: DynamicSecGate<A, R>,
    args: A,
) -> Result<R, TwzError> {
    // Six clock reads per dynamic gate call when this is unconditional, none of them inlined.
    // `gatestats::record` is the only consumer and it is off, so take them only when it is on.
    let t_frame = statcadence::STATS_ON.then(std::time::Instant::now);
    let frame = frame();
    let frame_ns = t_frame.map_or(0, |t| t.elapsed().as_nanos() as u64);
    let t_call = statcadence::STATS_ON.then(std::time::Instant::now);
    // Allocate stack space for args + ret. Args::with_alloca also inits the memory.
    let ret = GateCallInfo::with_alloca(get_thread_id(), get_sctx_id(), |info| {
        Arguments::<A>::with_alloca(args, |args| {
            Return::<Result<R, TwzError>>::with_alloca(|ret| {
                // Call the trampoline in the mod.
                unsafe {
                        //#mod_name::#trampoline_name_without_prefix(info as *const _, args as *const _, ret as *mut _);
                        #[cfg(target_arch = "x86_64")]
                        core::arch::asm!("call {target}", target = in(reg) target.address, in("rdi") info as *const _, in("rsi") args as *const _, in("rdx") ret as *mut _, clobber_abi("C"));
                        #[cfg(not(target_arch = "x86_64"))]
                        todo!()
                    }
                ret.into_inner()
            })
        })
    });
    let call_ns = t_call.map_or(0, |t| t.elapsed().as_nanos() as u64);
    let t_restore = statcadence::STATS_ON.then(std::time::Instant::now);
    restore_frame(frame);
    gatestats::record(
        frame_ns,
        call_ns,
        t_restore.map_or(0, |t| t.elapsed().as_nanos() as u64),
    );
    ret.ok_or(ResourceError::Unavailable)?
}

/// `Cell`, not `RefCell`: [`GateCallInfo`] is `Copy`, so the borrow flag bought nothing and cost a
/// read-modify-write plus a `panic_already_borrowed` edge on every gate entry -- on a value only
/// the owning thread can reach.
#[thread_local]
static CALLER_INFO: Cell<Option<GateCallInfo>> = Cell::new(None);

unsafe extern "C" {
    fn __is_monitor_ready() -> bool;
}

pub fn set_caller(info: GateCallInfo) {
    if unsafe { __is_monitor_ready() } {
        CALLER_INFO.set(Some(info));
    }
}

fn _reset_caller() {
    if unsafe { __is_monitor_ready() } {
        CALLER_INFO.set(None);
    }
}

pub fn get_caller() -> Option<GateCallInfo> {
    if !unsafe { __is_monitor_ready() } {
        return None;
    }
    let info = CALLER_INFO.get();
    if info.is_none() {
        panic!("..")
    }
    info
}
