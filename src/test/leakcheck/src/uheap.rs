//! This compartment's userspace heap, per size class, read from the reference runtime.
//!
//! The kernel side of this harness has `LEAKCHECK-KALLOC`; userspace had nothing equivalent, so
//! `l7-spawn-proc`'s growth could be located to a compartment (two `note=heap` objects in this
//! process's own slots) but not within it.
//!
//! Two questions this answers that a page count cannot. **Are the pages live blocks?** `net_count`
//! per class is allocations minus frees, so a class holding blocks is visible as a class rather
//! than as a page total. **Or are they discarded frees?** `alloc`/`dealloc` in the runtime have
//! four early returns that drop a free on the floor and one that routes an allocation to a bump
//! allocator whose frees are never honoured; those are counted separately, and by bytes, so
//! "the heap holds live blocks" and "the runtime threw the free away" cannot serialize to the same
//! number.

pub const NR_BRANCH: usize = 16;
pub const NR_CLASSES: usize = 32;
pub const NR_WORDS: usize = NR_BRANCH + NR_CLASSES * 4;

const BRANCH_NAMES: [&str; 8] = [
    "ferroc",
    "early_cold",
    "early_nots",
    "talc",
    "drop_notready",
    "drop_earlyptr",
    "drop_nulltls",
    "drop_nots",
];

unsafe extern "C" {
    /// Per-size-class heap census in the reference runtime (`runtime/alloc.rs::census`).
    fn __twz_rt_diag_heap_census(out: *mut u64, n: usize) -> usize;
    /// Arms it. Disarmed by default so that no boot but this harness's pays for it.
    fn __twz_rt_diag_heap_census_arm() -> u64;
}

/// Arm the runtime's census, once, before any op runs.
///
/// Separate from `take()` on purpose: arming inside the first snapshot would make op 1's window
/// start mid-arm and its `alloc`/`free` counts unbalanced by however many blocks were already
/// live. Reports whether it was already armed, so an instrument that switched itself on somewhere
/// else cannot hide behind this one.
pub fn arm() {
    let was = unsafe { __twz_rt_diag_heap_census_arm() };
    crate::console(&format!("LEAKCHECK-UHEAP-ARM was_armed={}\n", was));
}

#[derive(Clone)]
pub struct Snapshot {
    pub w: [u64; NR_WORDS],
    pub ok: bool,
}

pub fn take() -> Snapshot {
    let mut w = [0u64; NR_WORDS];
    let n = unsafe { __twz_rt_diag_heap_census(w.as_mut_ptr(), NR_WORDS) };
    Snapshot {
        w,
        ok: n == NR_WORDS,
    }
}

/// A signed delta, because a class can go negative (freeing blocks allocated before the window).
fn d(after: u64, before: u64) -> i64 {
    after as i64 - before as i64
}

pub fn report(op: &str, before: &Snapshot, after: &Snapshot, iters: usize) {
    // A zero census and a census that was never wired up serialize to the same all-zero table
    // unless the readout says which it was.
    if !before.ok || !after.ok {
        crate::console(&format!(
            "LEAKCHECK-UHEAP {} unavailable (not armed, or runtime lacks the census)\n",
            op
        ));
        return;
    }
    let iters = iters.max(1) as f64;

    let mut line = format!("LEAKCHECK-UHEAP {}", op);
    for (i, name) in BRANCH_NAMES.iter().enumerate() {
        line.push_str(&format!(
            " {}={}/{}",
            name,
            d(after.w[i], before.w[i]),
            d(after.w[i + 8], before.w[i + 8])
        ));
    }
    line.push('\n');
    crate::console(&line);

    let mut rows: Vec<(usize, i64, i64, i64, i64)> = Vec::new();
    let mut net_bytes_total: i64 = 0;
    for c in 0..NR_CLASSES {
        let b = NR_BRANCH + c * 4;
        let ac = d(after.w[b], before.w[b]);
        let ab = d(after.w[b + 1], before.w[b + 1]);
        let fc = d(after.w[b + 2], before.w[b + 2]);
        let fb = d(after.w[b + 3], before.w[b + 3]);
        if ac == 0 && fc == 0 {
            continue;
        }
        net_bytes_total += ab - fb;
        rows.push((c, ac, fc, ac - fc, ab - fb));
    }
    rows.sort_by_key(|r| -(r.4.abs()));

    crate::console(&format!(
        "LEAKCHECK-UHEAP-TOTAL {} net_bytes={} per_iter={:.1} classes={}\n",
        op,
        net_bytes_total,
        net_bytes_total as f64 / iters,
        rows.len()
    ));
    for (c, ac, fc, nc, nb) in rows.into_iter().take(20) {
        crate::console(&format!(
            "LEAKCHECK-UHEAP-CLASS {} le={} alloc={} free={} net_count={} net_bytes={} per_iter={:.2}\n",
            op,
            1u64 << c,
            ac,
            fc,
            nc,
            nb,
            nb as f64 / iters
        ));
    }
}

// ---- live-block tracking -----------------------------------------------------------------------

/// Entries the dump buffer can hold. Blocks beyond this are counted as overflow by the runtime,
/// never dropped silently.
pub const TRACK_MAX: usize = 1024;

unsafe extern "C" {
    fn __twz_rt_diag_heap_track_arm(lo: usize, hi: usize);
    fn __twz_rt_diag_heap_track_dump(out: *mut u64, n: usize) -> usize;
    /// Which heap object a pointer belongs to; `ops.rs` declares the same symbol.
    fn __twz_rt_diag_heap_id(ptr: *const u8, hi: *mut u64, lo: *mut u64) -> u32;
}

pub fn track_arm(lo: usize, hi: usize) {
    unsafe { __twz_rt_diag_heap_track_arm(lo, hi) };
}

pub fn track_off() {
    // lo > hi disarms.
    unsafe { __twz_rt_diag_heap_track_arm(1, 0) };
}

/// Dump the blocks allocated inside the window and not freed.
///
/// Prints the first 32 bytes of each. That is the identification: a retained `String` shows its
/// text, a `Vec` of ABI structs shows recognisable object ids, a boxed struct shows its first
/// field. Reading them is safe whether or not the block was freed -- heap objects stay mapped for
/// the life of the compartment, and a dropped free leaves the bytes exactly where they were.
pub fn track_report(op: &str, iters: usize) {
    let mut buf = vec![0u64; TRACK_MAX * 2 + 5];
    let n = unsafe { __twz_rt_diag_heap_track_dump(buf.as_mut_ptr(), buf.len()) };
    if n < 5 {
        crate::console(&format!("LEAKCHECK-UTRACK {} unavailable\n", op));
        return;
    }
    let live_words = n - 5;
    let (ins, rem, ovf, miss, trunc) = (buf[n - 5], buf[n - 4], buf[n - 3], buf[n - 2], buf[n - 1]);
    crate::console(&format!(
        "LEAKCHECK-UTRACK {} live={} inserted={} removed={} overflow={} free_miss={} truncated={} per_iter={:.2}\n",
        op,
        live_words / 2,
        ins,
        rem,
        ovf,
        miss,
        trunc,
        (live_words / 2) as f64 / iters.max(1) as f64
    ));

    // Group identical byte prefixes: 220 retentions of the same allocation site print as one row
    // with a count, which is what makes the dump readable at all.
    let mut groups: Vec<([u8; 32], usize, usize, u64, u64)> = Vec::new();
    for i in (0..live_words).step_by(2) {
        let ptr = buf[i] as *const u8;
        let size = buf[i + 1] as usize;
        let mut head = [0u8; 32];
        let take = size.min(32);
        unsafe { core::ptr::copy_nonoverlapping(ptr, head.as_mut_ptr(), take) };
        let (mut hi, mut lo) = (0u64, 0u64);
        let has_id = unsafe { __twz_rt_diag_heap_id(ptr, &mut hi, &mut lo) };
        let (hi, lo) = if has_id == 0 { (0, 0) } else { (hi, lo) };
        match groups.iter_mut().find(|g| g.0 == head && g.2 == size) {
            Some(g) => g.1 += 1,
            None => groups.push((head, 1, size, hi, lo)),
        }
    }
    groups.sort_by_key(|g| -(g.1 as i64));
    for (head, count, size, hi, lo) in groups.into_iter().take(24) {
        let hex: String = head.iter().map(|b| format!("{:02x}", b)).collect();
        let ascii: String = head
            .iter()
            .map(|&b| {
                if (0x20..0x7f).contains(&b) {
                    b as char
                } else {
                    '.'
                }
            })
            .collect();
        crate::console(&format!(
            "LEAKCHECK-UTRACK-BLK {} count={} size={} obj={:x}{:x} head={} |{}|\n",
            op, count, size, hi, lo, hex, ascii
        ));
    }
}

// ---- heap ownership ---------------------------------------------------------------------------

unsafe extern "C" {
    fn __twz_rt_diag_heap_objects(out: *mut u64, n: usize) -> usize;
}

/// Every heap object this compartment's allocator owns.
///
/// `note=heap` is written by every compartment's allocator, so a census grower reading `note=heap`
/// names a kind, not an owner. This names the owner exactly, and unlike a slot-map join it stays
/// correct for objects created *after* the map was dumped -- an object born mid-run can never match
/// a startup snapshot, which reads as "not ours" and is not.
pub fn report_own_heaps(tag: &str) {
    let mut buf = vec![0u64; 3 * 256 + 2];
    let n = unsafe { __twz_rt_diag_heap_objects(buf.as_mut_ptr(), buf.len()) };
    if n < 2 {
        crate::console(&format!("LEAKCHECK-OWNHEAP {} unavailable\n", tag));
        return;
    }
    let (main, early) = (buf[n - 2], buf[n - 1]);
    crate::console(&format!(
        "LEAKCHECK-OWNHEAP {} main={} early={}\n",
        tag, main, early
    ));
    for i in (0..n - 2).step_by(3) {
        crate::console(&format!(
            "LEAKCHECK-OWNHEAP-OBJ {} slot={} id={:x}{:016x} kind={}\n",
            tag,
            buf[i],
            buf[i + 1],
            buf[i + 2],
            if (i / 3) < main as usize {
                "main"
            } else {
                "early"
            }
        ));
    }
}

/// Map compartment names to their security-context ids, so a `heap:<sctx>` note names an owner.
///
/// The note carries an sctx because that is all the allocator can cheaply write from inside its own
/// OOM handler. Turning it into a name needs the monitor, which is a gate call and must not happen
/// there -- so it happens here, once, from a well-known list. A name that fails to resolve is
/// printed as such rather than skipped: "this service was not running" and "this service was not
/// asked about" must not look the same.
pub fn report_compartment_sctxs() {
    // Both spellings deliberately. Services are loaded by `init` under short names ("naming" is
    // what `get_naming_handle` looks up) but some resolve under the `-srv` library name too, and a
    // name that does not resolve is reported rather than skipped -- so a miss is visible as a miss.
    const KNOWN: &[&str] = &[
        "pager",
        "pager-srv",
        "naming",
        "naming-srv",
        "logboi",
        "logboi-srv",
        "devmgr",
        "devmgr-srv",
        "cache",
        "cache-srv",
        "net",
        "net-srv",
        "display",
        "display-srv",
        "monitor",
        "init",
        "leakcheck",
    ];
    for name in KNOWN {
        match monitor_api::CompartmentHandle::lookup(name) {
            Ok(h) => match h.info() {
                Ok(i) => crate::console(&format!(
                    "LEAKCHECK-COMP name={} id={:x} sctx={:x}\n",
                    name,
                    i.id.raw(),
                    i.sctx.raw()
                )),
                Err(e) => crate::console(&format!("LEAKCHECK-COMP name={} info_err={}\n", name, e)),
            },
            Err(e) => crate::console(&format!("LEAKCHECK-COMP name={} lookup_err={}\n", name, e)),
        }
    }
    // Ours, for the join: whatever compartment this harness is, by the same route.
    match monitor_api::CompartmentHandle::current().info() {
        Ok(i) => crate::console(&format!(
            "LEAKCHECK-COMP name=SELF id={:x} sctx={:x}\n",
            i.id.raw(),
            i.sctx.raw()
        )),
        Err(e) => crate::console(&format!("LEAKCHECK-COMP name=SELF info_err={}\n", e)),
    }
}
