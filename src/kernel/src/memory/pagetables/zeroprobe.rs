//! Does a zero-filled anonymous page stay zero until it is unmapped?
//!
//! The question this answers is the one at `frame.rs`'s `try to use the MMU to detect if a page is
//! actually ever written to or not`, and it is also the sizing question for a Linux-style shared
//! read-only zero page: both are worth exactly the number of faulted-in pages that are never
//! written.
//!
//! # Why the dirty bit cannot be read directly
//!
//! [`Table::map`](super::table) ORs `EntryFlags::DIRTY` into *every* new leaf. So `DIRTY` in an
//! object page table does not mean "someone wrote this", it means "may need writeback" -- a
//! deliberate over-approximation, because the kernel writes object data through the physical
//! direct map (`Object::write_bytes`/`set_bytes` go via `frame.virtaddr()`) where the object's own
//! entry never sees the store. Reading the bit as-is answers `true` for everything.
//!
//! So the probe suppresses that bit for the anonymous fill path only, via
//! [`MappingFlags::PROBE`](super::MappingFlags), and marks those entries with a spare software bit
//! so unmap can tell a probed entry from an ordinary one. Anonymous objects are safe to do this
//! to: their dirty list is collected by `MapControlCmd::Sync` and then discarded, because both use
//! sites in `region.rs` gate on `use_pager()`.
//!
//! # Why the content scan is the point
//!
//! With `DIRTY` suppressed, a clean entry still is not proof the page was never written -- every
//! direct-map writer named above is invisible to it. Rather than argue about how big that hole is,
//! [`record`] reads the frame and asks whether it is *actually* still zero. That turns the
//! experiment into a 2x2:
//!
//! - `clean & zero`   -- the population both optimizations would serve.
//! - `clean & nonzero` -- **the falsifier.** The MMU said untouched and the bytes disagree. Any
//!   non-zero count here means the dirty bit is not a sound basis for skipping a re-zero, and
//!   names how much the direct-map writers cost.
//! - `dirty & zero`   -- written per the MMU but zero anyway (a write of zeroes, or a COW break).
//!   Conservative, a missed opportunity, harmless.
//! - `dirty & nonzero` -- ordinary written pages.
//!
//! **`clean & zero` is an upper bound on "never written", not a lower one.** A frame arrives here
//! zeroed, so a page that was never written is necessarily both clean and zero -- but so is a page
//! written only through the direct map with a value of zero, and `Object::set_bytes(.., 0)` does
//! exactly that. The cell therefore contains the never-written pages plus an unknown number of
//! invisibly-zero-written ones. Read it as a ceiling on what a shared zero page could serve.
//!
//! The two questions this file gets asked have different answers, and the matrix separates them:
//! a shared zero page serves `clean & zero` only (any write breaks it), whereas skipping a re-zero
//! at free serves everything in `zero` regardless of the bit -- so the dirty bit's *recall* against
//! the `zero` column, not its precision, is what decides the second one.
//!
//! # Bounds on what is counted
//!
//! Only frames whose last reference is going away (`refcount == 1` at unmap). A frame shared by
//! two object page tables after `setup_cow_range` has one dirty bit per entry and no reverse map
//! to find the others with, so a per-entry answer would be meaningless; those are counted in
//! [`SHARED`] instead of guessed at.

use core::sync::atomic::{AtomicUsize, Ordering};

use crate::memory::frame::FrameRef;

/// Master gate. Off leaves `Table::map` setting `DIRTY` unconditionally, exactly as before, and
/// every hook here inert -- with this false the installed entry bits are identical to a tree
/// without this module, since `map_page_probed` never puts `PROBE` into the settings.
///
/// Off: the experiment has run. Three boots of `--tests` at release-kvm-smp4 (tag `zeroprobe-1`,
/// build a925e260d7ec245d), ~286k probed anonymous pages reaching unmap each:
///
/// | | count | share |
/// |---|---|---|
/// | clean + zero | 10,811 / 10,962 / 11,007 | 3.8% |
/// | clean + NONZERO | 1 / 1 / 1 | -- |
/// | dirty + zero | 30,794 / 30,805 / 30,814 | 10.8% |
/// | dirty + nonzero | 244,012 / 244,085 / 244,135 | 85.4% |
///
/// So: 14.6% of anonymous pages are still zero when they are unmapped, but the dirty bit finds
/// only 3.8% of them -- about a quarter recall -- and gets one wrong per boot, reproducibly the
/// same one. Neither number justifies the machinery. Left in place, gated, because the matrix is
/// cheap to re-run against a different workload and the interesting cell (`dirty + zero`, the 10.8%
/// the bit cannot see) is the one a content-based scheme would have to go after instead.
pub const ENABLED: bool = false;

/// Read the frame's bytes at unmap to get ground truth, rather than trusting the bit.
///
/// This is a page-sized read per probed unmap, on object teardown. It is the whole reason the
/// experiment can distinguish "the bit is sound" from "the bit is convenient", so it defaults on;
/// turn it off to measure the probe's own cost.
pub const SCAN: bool = true;

/// Entries installed with the probe, i.e. the denominator.
static INSTALLED: AtomicUsize = AtomicUsize::new(0);
/// Probed entries reaching unmap. Less than [`INSTALLED`] by whatever is still mapped at shutdown.
static SEEN: AtomicUsize = AtomicUsize::new(0);
/// Probed unmaps skipped because the frame still has other references.
static SHARED: AtomicUsize = AtomicUsize::new(0);
/// `[dirty][nonzero]`.
static MATRIX: [[AtomicUsize; 2]; 2] =
    [const { [const { AtomicUsize::new(0) }; 2] }; 2];
/// Bytes covered by the entries in [`MATRIX`], so a large page is not counted as one 4 KiB page.
static BYTES: [[AtomicUsize; 2]; 2] = [const { [const { AtomicUsize::new(0) }; 2] }; 2];

pub fn record_install() {
    if !ENABLED {
        return;
    }
    INSTALLED.fetch_add(1, Ordering::Relaxed);
}

/// Tally one probed entry being torn down. `dirty` is the entry's hardware dirty bit.
pub fn record(dirty: bool, frame: FrameRef) {
    if !ENABLED {
        return;
    }
    SEEN.fetch_add(1, Ordering::Relaxed);
    // The caller is about to drop this reference; anything above one means another entry also
    // points here and this entry's bit does not describe the frame.
    if frame.refcount() > 1 {
        SHARED.fetch_add(1, Ordering::Relaxed);
        return;
    }
    let nonzero = if SCAN {
        // u64 rather than u8: same answer, an eighth of the loads, and the frame is aligned.
        frame.as_slice::<u64>().iter().any(|w| *w != 0)
    } else {
        // Without the scan there is no ground truth, so report the bit's own claim. The two
        // "disagree" cells are then empty by construction -- which is why SCAN defaults on.
        dirty
    };
    let d = dirty as usize;
    let z = nonzero as usize;
    MATRIX[d][z].fetch_add(1, Ordering::Relaxed);
    BYTES[d][z].fetch_add(frame.size(), Ordering::Relaxed);
}

pub fn print() {
    if !ENABLED {
        emerglogln!("== zeroprobe: disabled");
        return;
    }
    let installed = INSTALLED.load(Ordering::Relaxed);
    let seen = SEEN.load(Ordering::Relaxed);
    let shared = SHARED.load(Ordering::Relaxed);
    if installed == 0 {
        emerglogln!("== zeroprobe: no probed mappings installed");
        return;
    }
    let cell = |d: usize, z: usize| {
        (
            MATRIX[d][z].load(Ordering::Relaxed),
            BYTES[d][z].load(Ordering::Relaxed),
        )
    };
    let (cz, cz_b) = cell(0, 0);
    let (cn, cn_b) = cell(0, 1);
    let (dz, dz_b) = cell(1, 0);
    let (dn, dn_b) = cell(1, 1);
    let counted = cz + cn + dz + dn;
    emerglogln!(
        "== zeroprobe (scan={}): installed {} probed anon mappings, {} reached unmap, {} shared (skipped), {} counted",
        SCAN,
        installed,
        seen,
        shared,
        counted
    );
    let pct = |n: usize| {
        if counted == 0 {
            0
        } else {
            n * 100 / counted
        }
    };
    emerglogln!(
        "   clean+zero    {:>8} ({:>2}%) {:>9} KB   <- never written, still zero",
        cz,
        pct(cz),
        cz_b / 1024
    );
    emerglogln!(
        "   clean+NONZERO {:>8} ({:>2}%) {:>9} KB   <- FALSIFIER: dirty bit missed a write",
        cn,
        pct(cn),
        cn_b / 1024
    );
    emerglogln!(
        "   dirty+zero    {:>8} ({:>2}%) {:>9} KB   <- conservative, zero anyway",
        dz,
        pct(dz),
        dz_b / 1024
    );
    emerglogln!(
        "   dirty+nonzero {:>8} ({:>2}%) {:>9} KB",
        dn,
        pct(dn),
        dn_b / 1024
    );
    emerglogln!(
        "   still-zero at unmap: {} of {} counted ({}%); dirty bit sound: {}",
        cz + dz,
        counted,
        pct(cz + dz),
        cn == 0
    );
}
