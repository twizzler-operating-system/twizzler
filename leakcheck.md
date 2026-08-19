# leakcheck: an empirical leak harness — build notes and first results (2026-08-18)

Companion to [leakplan.md](leakplan.md), which is the design. This file records what was built,
what it measured, and the one finding it has produced so far. The four static audits
([kleaks.md](kleaks.md), [mleaks.md](mleaks.md), [oleaks.md](oleaks.md), [pleaks.md](pleaks.md))
all open with "nothing here was run"; this is the instrument that runs things.

## What was built

**ABI**: `MemoryStats` gained a `tracker: TrackerStats` field
([info.rs](src/lib/twizzler-abi/src/syscall/info.rs)) publishing the frame tracker's
`idle`/`kernel_used`/`page_data`/`total`/`pager_outstanding`/`allocated`/`freed`/`reclaimed`/
`waiting`/`reclaiming`. These existed only as `PERFMARK-MEM` serial lines, which an in-guest
program cannot compute a verdict from. Filled by a new read-only
`tracker::fill_stats` ([tracker.rs](src/kernel/src/memory/tracker.rs)), called from
`get_memory_stats` alongside the existing frame/fault/pagetables/allocator fills.

**Harness**: `src/bin/leakcheck`, a plain binary run via `--autostart`. 30 counters per sample
across kernel memory/objects/threads/sctx, the monitor, and this compartment's slots. Output goes
through `sys_kernel_console_write` (not stdout) so a serial log always carries it.

**Analysis**: [tools/leakplot.py](tools/leakplot.py) parses a boot log, refits every series, ranks
findings, and writes one PNG grid per operation. The fit is duplicated rather than trusted from
the log, so guest and host disagreeing is visible.

## Running it

> **Numbers below predate `IS_ZEROED = true` (shipped 2026-08-18, later).** ferroc's `post_alloc`
> now takes the `free_is_zero` branch and skips its memset, so it touches fewer pages on the
> allocation path. Every slope in this file was measured with the memset in place. The *conclusions*
> do not rest on those slopes -- the thread finding is carried by a 4 MiB vs 16 byte address stride
> and by per-object census attribution, neither of which a memset can move -- but any slope quoted
> here is now a historical baseline, and a comparison against a fresh run is a comparison across
> that flip. Re-measure before treating a difference as a regression.
>
> A trap this creates for anyone extending the catalogue: `IS_ZEROED` only changes `post_alloc`
> when `zero == true`, i.e. on `alloc_zeroed` paths (`vec![0; n]`, `Box::new_zeroed`, calloc). Every
> l2 op here uses `Vec::with_capacity`, which routes to plain `alloc`, so the flip is invisible to
> them. But **add a zeroed-allocation op and the skipped memset stops touching that block's pages**,
> so resident pages drop for any zeroed block the caller does not fully write. In a page-counting
> harness that reads as a leak being fixed when it is only a memset being removed. Prediction
> credited to the session that shipped the flip, and stated before the run rather than after it.
>
> Second consequence, for debug arms only: with `IS_ZEROED = true`, ferroc's
> `debug_assert!(all bytes zero)` is load-bearing, so a debug leakcheck run now aborts on a kernel
> zeroing regression instead of quietly returning stale bytes. Worth having; also a new way for a
> debug run to die.


```
python3 many.py -r 1 --config release-kvm-smp4 --heartbeat-tries 70 \
    --autostart "leakcheck --ops all --census" --kernel-arg=--diag --tag leakN -j 1
python3 tools/leakplot.py target/results/many-leakN/round1-release-kvm-smp4.log -o out/
```

`--heartbeat-tries` is **required** for the full catalogue and is not optional tuning. The default
budget is ~21 heartbeats of 15s = 322s and the 22 ops need ~460s, so the run dies mid-catalogue
having completed exactly 15 ops. It presents as `no test report (timeout or early exit)`, which is
misleading twice over: it is not a timeout, and `--timeout-scale` does not affect it -- that scales
`TWZ_SILENCE_TIMEOUT`, and this guest is never silent. The real message is in qemu's stderr,
"never shut the guest down", and the real limit is `tries > run.heartbeat_tries` in `qemu.rs`. The
tell is determinism: two runs died at the identical elapsed time having completed the identical op.

smp4 is not optional: `scan_deleted` runs from the bsp idle loop every 1000 iterations and
`cleanup_exited` every 100, and an smp1 boot never idles while anything runs. `--diag` is required
because autostart boots do not set kernel TEST_MODE.

## The statistic

Slope over a tail, not a before/after delta — a delta cannot separate a one-time cache fill from a
per-iteration leak, because at N=2 they are identical. Default N=40 with the first 10 discarded.

Three gates, all required, and the third is the one that earned its place:

| gate | rejects |
|---|---|
| `r2 >= 0.9` | churn — a counter that moves without trending |
| `growth >= 1` and `net > 0` between quiesced states | anything reclaimed once deferred work ran |
| `max_step_frac <= 0.34` | **background work** — growth concentrated in one jump |

The third came out of the first run's plots and is worth stating because r2 does not catch it.
The null control's `trk.page_data` climbed 8 frames with r2 = 0.77 — but as *two jumps of four*,
something else in the system doing a piece of work. A real leak spreads its growth evenly: the
positive control's largest single step is 3% of its total, the null control's is 50%. Fitting a
line to a staircase yields a confident slope and a plausible r2, so on r2 alone the null control
would eventually have been reported as leaking.

## Controls

Both ship, and both must pass before any other row is worth reading.

- **l0-null** — `sys_thread_self_id()` in a loop. Must read zero. Its residual is the floor.
- **p1-leak-object** — leaks one object per iteration on purpose. Must be detected, at its known
  size. A harness that has never demonstrated it can see a leak is an instrument that answers the
  same way regardless of the truth.

Measured (release/KVM, smp4, N=40):

```
null control l0-null: clean
positive control p1-leak-object: detected, obj.objects 1.000/iter (expected ~1.0)
```

The positive control also **calibrates the counters**: one leaked object costs
`obj.objects` +1.000, `trk.kernel_used` +3.000, `trk.page_data` +1.111, `mem.kalloc_bytes` +5083
per iteration, all at duty 1.00. That is the unit a future finding is read against.

## Results

| op | verdict |
|---|---|
| l0-null | clean (floor: page_data moves ~8-19 frames per run, stepped) |
| l1a-obj-create-delete | clean |
| l1b-map-unmap | clean |
| l2a-handle-map-drop | clean |
| l2b-heap | clean |
| l3-thread | **fixed 2026-08-19** — was ~80 KB/thread; root cause ferroc's per-thread heap |
| l7-spawn-proc | skipped, 40/40 iterations failed to spawn |
| p1-leak-object | detected (control) |

*(Superseded by [the full 22-op catalogue](#full-catalogue-against-the-shipped-tree-leak14-full-2026-08-19),
which is the current reading. This table is the phase-2 snapshot and is kept for the history.)*

Quiesce converged everywhere, in ~1.3s — except `l3-thread`, which took **3.3s**, the longest in
the suite and consistent with `cleanup_exited` popping a single thread per call.

`l7` reporting a skip rather than a clean zero is the honest-failure path working: `Command::new
("ls")` did not resolve. `ls` is a symlink init installs at `/initrd/ls` pointing at the on-disk
`uuhelper`, so the fix is a path, not an initrd entry. Unfixed only because another session holds
the tree.

### L1: thread spawn + join leaks ~80 KB into the caller's own compartment heap

```
                        N=40            N=120
trk.page_data      20.94/iter      19.69/iter    r2 0.953 -> 0.994
trk.kernel_used     1.18/iter       1.09/iter    r2 0.966 -> 0.996
net page_data        +764             +2400      pages, after a converged quiesce
```

~19.7 page-data frames per `std::thread::spawn` + `join`, about 80 KB a thread.

**It is a leak, not a high-water mark.** talc never returns pages, so heap-object growth could in
principle be fragmentation that plateaus. Tripling the iteration count settles it: the slope is
flat (20.9 -> 19.7), the fit *tightens* (r2 0.953 -> 0.994), the largest single step falls to 7% of
total growth, and the net scales 3.1x with 3x the iterations. Fragmentation decelerates. This does
not, across 120 iterations.

**The census names the holder**, which is what it is for:

```
LEAKCHECK-GROWER l3-thread 13227a4a... pages    0->1280 (+1280, 10.67/iter) new      note=heap
LEAKCHECK-GROWER l3-thread ef3988de... pages 1012->2078 (+1066,  8.88/iter) existing note=heap
LEAKCHECK-GROWER l3-thread 1d049780... pages 1597->1645 (  +48,  0.40/iter) existing note=monitor-heap
```

10.67 + 8.88 = 19.55 pages/iter in objects tagged `heap`, against a measured `page_data` slope of
19.69 — essentially all of it. The `heap` note is written at
[talc.rs:143](src/rt/reference/src/runtime/alloc/talc.rs#L143), so these are the **reference
runtime's per-compartment talc heap objects for the calling compartment**. The workload also
forced a brand-new 1280-page heap object partway through, which a bounded high-water mark would
not do. Against a null-control floor of 0.15 heap pages/iter (leakcheck's own console output),
this is ~130x.

**This refutes the hypothesis the counters suggested.** mleaks.md M1 (`TlsRegion` has no `Drop`,
one leaked per monitor-spawned thread) predicted the growth would be in the *monitor's* heap. The
monitor heap moves 0.40 pages/iter — 2% of the total, and the null control moves it 0.10/iter
anyway. The leak is in the caller's own compartment heap. M1 may still be real, but it is not
what this measures, and the counters alone would have sent the fix to the wrong process. This is
the case for the census existing.

**Also worth noting**: leakplan.md §2.2 assumed `MonAlloc`/`Track` in `monitor/src/main.rs`
already tracked allocated bytes and only needed exporting. It does not — it records call-site IPs,
and it is not even installed as the `#[global_allocator]` (the attribute is commented out). A
monitor heap-bytes counter would have to be written, not exported. The census made it unnecessary.

**Where to look next**: what the reference runtime allocates per spawned thread and does not free
on join — `InternalThread` (thread/internal.rs), the per-thread TLS region handed out by
`TlsGenMgr` (thread/tcb.rs), the thread name, and the join packet. `twz_rt_gc()` calls
`gc_threads()` and the quiesce calls it every round, so whatever this is either is not on the gc
path or is not reachable from it. ~80 KB is far larger than the bookkeeping structs, which points
at the TLS region as the biggest single candidate.

### Two discriminators: it is live growth, and it is rate-invariant

talc never returns pages to its heap object, and a slope alone cannot separate a leak from
deferred reclamation. Two tests, one boot each.

**Repetition** — the same op three times under different names, so three series land in one boot.
A high-water mark grows once and then reuses; a leak grows again every time.

| op | page_data slope | net pages |
|---|---|---|
| l3-thread | 20.95 | 774 |
| l3-thread-b | 23.48 | 870 |
| l3-thread-c | 16.78 | 779 |
| l3-thread-slow | 18.49 | 824 |

Each run adds ~800 pages again. Across the four, one heap object goes 931 -> 2402 pages and a
second 0 -> 1817, both monotone, neither ever shrinking: ~13 MB over 160 spawns. **Live growth,
not a high-water mark.**

**Rate** — `l3-thread-slow` is the identical body with a 10 ms sleep per iteration. Deferred
reclamation is a race against allocation rate, so slowing the workload should give the reaper time
and cut the slope. It does not: 18.49 sits inside the three fast runs' spread of 16.78-23.48.
**Rate-invariant, so a leak rather than reclamation lag.**

Two honest limits on that second claim. The run-to-run spread is +-20%, so this rules out a large
rate effect and not a small one. And the harness does not record the iteration loop's wall time —
only quiesce durations — so the *achieved* rate ratio is unknown rather than the 10x intended.
Recording per-op run time is a one-line fix and should land before this test is repeated.

### TLS is excluded, arithmetically

The audits pointed at TLS and so did I. The measurement closes it:

```
LEAKCHECK-TLS layout_bytes=1944 layout_pages=0.47 align=32 gen=4
```

**1944 bytes** — under half a page, against a measured ~80 KB per spawn. The leak is 42x the
entire TLS template, so no accounting of TLS regions reaches it, however many are leaked and
wherever they live. That excludes all three candidates at once:

- **The compartment's region** (`impl_spawn` -> `TLS_GEN_MGR::get_next_tls_info`). Freed by
  `InternalThread::Drop` on join anyway, and 1944 bytes if it were not.
- **A second region from `cross_compartment_entry`.** Does not arise: `impl_spawn` installs the
  region it allocated as the new thread's thread pointer, so the entry path takes its `tp != 0`
  warm return. That path is only for a thread crossing into a compartment that did not start it.
- **The monitor's supervisor region.** Real and separate — `build_super_tls` at spawn, installed
  as `super_thread_pointer` ([thread.rs:519](src/rt/monitor/src/mon/thread.rs#L519)) — so there
  genuinely are two TLS regions per thread by design. But it is in the monitor's heap, which the
  census clocks at 0.40 pages/iter, and it is pooled via `pool::put_tls`.

Worth stating as a method point: three rounds of reading the code produced three plausible TLS
stories, and one number killed all of them in a line. The size check should have come first --
`80 KB / 1944 B = 42` needs no understanding of the spawn path at all. Reach for the arithmetic
that bounds a hypothesis before the code that explains it.

**What is left**: the 2 MiB stack. `stackpool`'s own doc says a 2 MiB request "clears ferroc's
`LARGE_MAX` and takes a fresh span from the base allocator, whose pages nothing has touched yet" --
the pool exists precisely because that path is expensive. 80 KB is ~4% of a 2 MiB stack, about what
a trivial thread touches (the `STACK_TOP_ZERO` page plus libstd's setup frames). If the span is
reused the pages stay mapped and nothing grows; if it is not, every spawn touches fresh pages.
`l2c-heap-2mb` / `l2d-heap-2mb-touched` test that with no threads involved, against `l2b-heap`'s
clean 64 KiB.

### L2: touched pages in a long-lived object are never returned

> **Stale in one direction (2026-08-18, later).** These numbers were taken before a kernel fix to
> `Table::setup_zero_range`, which walked a shared `&mut MappingCursor` and advanced it twice per
> level-1 region: any `zero_range` crossing a 2 MiB boundary zeroed only the first region and
> returned `Ok(())`. `decommit_range` is the *only* path that ever returns a heap object's frames
> and it goes through that syscall, so pre-fix every decommit spanning 2 MiB freed one region and
> reported success. The 512 pages/iter below is therefore measured against a broken decommit and
> overstates retention by an unknown amount.
>
> Re-running it is not simply a rerun. Both `trk.freed` and `mem.tlb_shootdowns` read flat zero
> across `l2d` while `trk.allocated` runs at 512.8/iter, and both are nonzero for `l3-thread` in
> the same boot -- so decommit is never *reached* on this workload at all, and a post-fix `l2d`
> still reading 512 would say nothing about the fix. Gate the l2 rows on `trk.freed` going nonzero
> before reading their r2; retention is above the base allocator, in shard retirement.


> **Read this with [the narrowing](#reconciling-that-with-l2-the-mechanism-is-real-but-scoped-to-surviving-objects)
> and [the refutation](#the-floor-creep-prediction-is-refuted-the-mechanism-is-not) below.** As
> first written this section claimed the effect held generally; it does not. It holds for a
> long-lived object with no memory pressure — which is exactly the regime measured here — and does
> *not* explain whole-system resident page data, which was what it was reached for. The numbers
> below are sound; the scope sentence at the end of the section was too broad and is corrected
> later.

Isolating the 2 MiB allocation from threads entirely turned the thread finding into a special case
of something general.

| op | page_data slope | net pages | verdict |
|---|---|---|---|
| l2b-heap (64 KiB, untouched) | 0.0000 | 14 | clean |
| l2c-heap-2mb (2 MiB, untouched) | 0.0000 | 10 | clean |
| **l2d-heap-2mb-touched** (2 MiB, one write per page) | **512.0000** | **20493** | **leak** |
| l3-thread | 20.9257 | 774 | leak |

**512.0000 pages per iteration, r2 = 1.000, maxstep 0.03.** That is exactly 2 MiB / 4096 — every
page touched, retained, with no rounding. 40 iterations of a workload whose live set never exceeds
2 MiB retained **80 MB**, and the compartment's heap object went 1041 -> 21530 pages in one op.

The pair is what makes it a mechanism rather than a number: allocating and freeing 2 MiB without
touching it is *clean* (l2c), and the identical allocation with one byte written per page leaks all
512. So the allocator is not failing to free the block -- it is that a page, once faulted into the
heap object, is never given back. Freed memory is reusable as *address space* but its physical
frames stay charged to the object forever. `stackpool`'s doc names the upstream half: a 2 MiB
request "clears ferroc's `LARGE_MAX` and takes a fresh span from the base allocator, whose pages
nothing has touched yet" -- fresh span each time, so each iteration's writes fault fresh pages.

This reframes L1. `l3-thread`'s 20.9 pages/iter is not a fact about threads; it is this, at the
scale of what a trivial thread actually touches. The thread pools (stack and TLS, cap 8 each) are
what keep it at 21 pages instead of 512.

**Whether to call it a leak is a judgement, and the magnitude is the argument.** Allocators that
never decommit are ordinary. But here the heap is an *object* and its pages are real frames, so a
compartment's floor rises monotonically with the high-water mark of everything it has ever touched,
never with what it currently holds. A 2 MiB live set costing 80 MB resident is a 40x overhead, and
nothing in the system brings it back.

**Next**: whether ferroc's large path can reuse a freed span (which would fix it without any
decommit), and whether the heap object can be trimmed. Both are upstream of the thread result --
fixing this fixes L1 as a side effect, and the reverse is not true.

### Shipped (2026-08-19): the fix is on, unconditionally

`InternalThread::drop` now calls `__mlibc_handle_thread_exit` for every thread it reaps, and the
knob, the alternate `exit` placement and all six A/B ops are deleted. It depends on a three-line
guard in the `src/ports/ferroc` submodule -- `fini` clears its `HEAP` thread-local only when the
finalized id belongs to the running thread -- and **the two must land together**: the guard alone is
inert, and the call alone would have each reaping thread abandon its own heap, which is worse than
the leak it fixes.

Confirmed on the shipped tree (`leak16-shipped`, release/KVM/smp4, fingerprint `5e42002e5fec`,
ferroc submodule diff `6ea216ceb5c8`). PASS, 1m50s, zero panics, zero supervisor exceptions:

| check | before | after |
|---|---|---|
| destructor probe | `ran=0/10` | **`ran=10/10`** |
| `l3-thread` `page_data` | 20.58/iter, LEAK | **9.95/iter, clean** (r2 0.842, maxstep 0.500) |
| spawn stride | `0x400000` x 38/39 | **`0x10` x 39/39** |
| span over 40 spawns | 160.000 MiB | **0.0006 MiB** |

The census agrees independently: the ferroc thread-heap object no longer appears as a grower at all,
leaving the compartment's general heap (~0.3/iter, present under `l0-null` too) and monitor-heap.
`p1-leak-object` still detected at 1.09 pages/iter in the same boot, so the harness was shown
capable of reporting a leak in the run where it reports none for threads.

The probe stays in the catalogue as a regression guard rather than as a result: `ran` dropping below
`set` means the call has been lost again, which is precisely how it was lost in the first place --
nobody removed it, it was never there.

**Coverage limit, stated rather than implied.** One boot, release/KVM/smp4. The teardown path had
never executed in this configuration before tonight; ~130 spawned threads exercised it without a
fault, but TCG and smp1 are covered by the validation sweep rather than by anything here.

### L1 root cause: ferroc's per-thread heap is never released

`l3-thread`'s per-spawn growth is ferroc's thread-local heap. Every thread gets its own context,
heap and 4 MiB slabs; ferroc releases them by registering `fini` -> `ThreadLocal::put(id)` through
`pthread_key_create` ([global/thread.rs:56](src/ports/ferroc/src/global/thread.rs#L56)). Recycling
the id is what lets the *next* spawn reuse a dead thread's entry -- `assign()` explicitly reuses an
already-initialized entry for a recycled id, and only calls `insert` for a fresh one.

**That destructor never runs.** mlibc calls key destructors only from `thread_exit` /
`run_dtors_for_tcb`, and a Twizzler thread reaches neither: `trampoline` -> std's `thread_start` ->
`twz_rt_exit` -> `sys_thread_exit`. `CrossThread::drop` calls `__mlibc_handle_thread_exit` for a
thread entering from another compartment; `InternalThread::drop`, the path a `std::thread::spawn`
takes, has no equivalent. So every spawn takes a fresh id, a fresh entry and fresh slabs, and
nothing is ever reused.

Measured three ways in one boot (`many-leak10-dtor`, release/KVM/smp4, N=40), with a diagnostic
knob selecting where the destructors run: `0` off, `1` on the exiting thread, `2` at teardown from
whoever reaps the `InternalThread`.

**1. The destructors do not run.** A key registered by leakcheck itself, set on 10 spawned threads:

```
LEAKCHECK-PTHREAD-DTOR arm=off       set=10/10 ran=0
LEAKCHECK-PTHREAD-DTOR arm=exit      set=10/10 ran=10
LEAKCHECK-PTHREAD-DTOR arm=teardown  set=10/10 ran=10
```

The probe runs in every arm on purpose. A probe that has only ever reported zero has not been shown
capable of reporting anything else; `ran=10` twice is what makes `ran=0` an absence rather than a
broken probe.

**2. Where each spawned thread's first allocation lands.** This is the mechanism itself, in
addresses rather than in frames, and it needs no slope at all:

| arm | most common delta | over 40 spawns |
|---|---|---|
| off | **0x400000** (38/39) | 160.00 MiB |
| exit | **0x10** (39/39) | 624 bytes |
| teardown | **0x10** (39/39) | 624 bytes |

0x400000 is exactly `SLAB_SIZE`: with the destructors off, every spawn's first allocation lands in a
brand-new 4 MiB slab. 0x10 is exactly `GRANULARITY`: with them on, consecutive spawns get
consecutive blocks of the *same shard* of the *same heap*. The teardown arm's first address
(`0x4957420280`) continues from the exit arm's last (`0x4957420270`) -- the same heap carried across
both. A factor of 262,144 in address-space footprint per spawn.

**3. Slope and census.**

| arm | `page_data`/iter | r2 | maxstep | duty | `kernel_used`/iter |
|---|---|---|---|---|---|
| l3-thread (off) | 20.62 | 0.963 | 0.240 | 1.000 | 1.039 |
| l3-thread-dtor-exit | 10.19 | 0.848 | 0.474 | 0.172 | 0.072 |
| l3-thread-dtor-teardown | 7.65 | 0.776 | 0.483 | 0.207 | 0.218 |

Both fixed arms fail the r2 and `max_step_frac` gates, so by this harness's own criteria neither is
classified as leaking any more -- the growth that remains is stepped, not per-iteration. The census
names what stopped:

```
l3-thread                 2b3e02ca... pages  134->554  (+420, 10.50/iter) note=heap
l3-thread-dtor-exit       2b3e02ca... pages  554->563  (   +9,  0.23/iter) note=heap
l3-thread-dtor-teardown   2b3e02ca... (absent)
```

`kernel_used` is the corroborating counter and it collapses 14x. Page-table frames track *address
span*, not page count -- which is why `l2d` costs 0.80 of them for 512 touched pages while
`l3-thread` costs 1.04 for 21. That ratio was the tell, sitting unread in the leak8 log: 21 pages
that cost more page-table memory than a contiguous 2 MiB touch cannot have been appended to the
previous iteration's pages.

**Both placements work, identically.** Which is a correction: I first wrote that the destructors
"have to run where their thread-locals live". They do not. mlibc's side is foreign-safe by
construction -- `run_dtors_for_tcb` reads each key's value out of the *dead* thread's TCB, which is
why `CrossThread::drop` can already call it. What is not foreign-safe is the destructor ferroc
registers: two of `fini`'s three statements are pure in the argument, and the third,
`HEAP.set(empty_heap)`, writes a `#[thread_local]` of whoever *executes* it. Run from a reaper it
would abandon the reaper's own heap without recycling that id, and -- its slot now empty -- the
reaper would take the id it had just freed on its next allocation, leaving the next real spawn to
assign a fresh entry anyway. So the teardown arm carries a three-line ferroc guard: clear `HEAP`
only when the id being finalized is the running thread's. Under `exit` the guard is a no-op.

That makes teardown the better placement rather than merely an equal one: it is the only one that
covers a thread which never reaches `twz_rt_exit` at all -- force-exit, compartment teardown -- which
exit-placement structurally misses. Both together is safe, because mlibc nulls `localKeys[i].value`
after calling a destructor from either path, so a second pass finds nothing.

**The parts probe closes three candidates and moves the rest to a different object.** Reporting,
per spawn, the address *and owning heap object* of each thing a spawn allocates (`many-leak11-parts`,
80 spawns across both arms):

```
stack = 0x4840797aff   identical on all 80 spawns, both arms
tls   = 0x4940200b00   identical on all 80 spawns, both arms
heap  = 4 MiB stride (off arm)  ->  16 byte stride (fixed arm)
```

The stack and the TLS region are *byte-for-byte the same address every time*, in both arms. Both
pools work perfectly, and neither contributes a page. That replaces the earlier deduction-plus-A/B
("a bounded pool cannot produce unbounded linear growth") with a direct observation, and it is the
stronger form of the same claim.

The object ids then split the baseline cleanly. The thread's own heap block and its TLS both live in
`11ae0060...`; the residual lives in `e0a0494a...`, a *different* heap object that no per-spawn
allocation the probe can see ever addresses:

| object | l3-thread | l3-thread-dtor-exit |
|---|---|---|
| `11ae0060` (thread heap, note=heap) | 10.50/iter | **0.23/iter** |
| `e0a0494a` (note=heap) | 6.80/iter | 9.97/iter |
| `1f96ea9b` (note=monitor-heap) | 0.30/iter | 0.30/iter |

Across all four thread arms in that boot the second object reads 6.80, 10.05 (destructors off) and
9.97, 6.88 (on) -- **completely overlapping, so it is knob-independent**, and the apparent rise in
the table is spread rather than an effect. That also excludes a whole class of causes at once:
anything whose lifetime is per-thread-entry, ferroc's per-thread `Heap`/`Context` and the bucket
chunks holding them included, must get *cheaper* in the arm where ids are recycled and
`ThreadLocal::insert` stops being called. This does not move.

So `l3-thread`'s 20.6 was never one leak. It was ~10.5 of ferroc thread-heap churn, which is fixed
and confirmed fixed by object id rather than by total; ~7-10 in a second heap object, untouched by
the fix; and 0.3 of monitor heap, flat in every arm including the null control.

> **Resolved (2026-08-19, later):** the "other half" below is attributed — freed small blocks are
> never reused at all, threads incidental. See
> [the leak18-leak21 arc](#the-residual-attributed-small-block-churn-never-reuses-leak18-leak21-2026-08-19)
> at the end of this file.

**Still open: the other half.** The second heap object above (`e0a0494a`, `b83c02ca` in the
earlier boot -- ids are per-boot) grows ~10 pages/iter in *both* fixed arms and grew 6.80/iter in the
baseline, where it was never separated from the ferroc term. It also grows 0.28/iter under
`l0-null`, so it is the compartment's general heap rather than anything thread-specific, and the
spawn multiplies it by 35x. It is stepped rather than linear -- duty 0.17, maxstep 0.48,
about seven jumps of ~57 pages -- which is the shape of an allocator extending its arena in chunks
rather than of a per-spawn retention, and it is consistent with L2 (a touched page is never returned
by the object holding it) rather than with a new leak. It is now the dominant remaining term and the
census has already named its holder, which is where the next measurement starts. Not attributed
here, and not claimed as fixed.

Two method notes. The size check that would have found this in one line is `4 MiB / 16 bytes`, not
any amount of reading -- the same lesson the TLS exclusion taught, unlearned and relearned. And the
diagnostic knob is a runtime `AtomicU32` rather than a `const`, so all three arms ran in one boot
against one tree state; the stack-recycle A/B lost three attempts to the tree moving between builds,
and this cost nothing to avoid.

### Full catalogue against the shipped tree (leak14-full, 2026-08-19)

All 22 ops, release/KVM/smp4, `--census`, N=40. PASS, 7m38s, zero panics and zero supervisor
exceptions. Verdicts below apply the three gates to **level counters only** -- a cumulative counter
(`trk.allocated`, `trk.freed`, `mem.page_faults`, `mem.tlb_shootdowns`) rises at a constant rate for
any op that does work, so it has a near-perfect r2 and a tiny `max_step_frac` *by construction*, and
gating it flags every working op as a leak. My first pass at this table did exactly that and called
`l1a` a leak. The `Kind::Level`/`Kind::Cumulative` tag exists to prevent it; use it.

| op | `page_data`/iter | r2 | maxstep | verdict |
|---|---|---|---|---|
| l0-null | 0.3204 | 0.858 | 0.500 | clean (floor) |
| p1-leak-object | 1.1860 | 0.989 | 0.152 | **LEAK** (control, `obj.objects` exactly 1.0000/iter) |
| l1a-obj-create-delete | 0.1566 | 0.587 | 1.000 | clean |
| l1b-map-unmap | 0.1112 | 0.417 | 1.000 | clean |
| l2a-handle-map-drop | 0.0721 | 0.270 | 1.000 | clean |
| l2b-heap | 0.0000 | 1.000 | 0.000 | clean |
| l2c-heap-2mb | 0.0000 | 1.000 | 0.000 | clean |
| l2d-heap-2mb-touched | 512.0392 | 1.000 | 0.035 | **LEAK** |
| l2e-heap-small | 0.0000 | 1.000 | 0.000 | clean |
| l2f-heap-2mb-touched-gc | 512.0000 | 1.000 | 0.034 | **LEAK** |
| l3-thread | 20.5771 | 0.962 | 0.246 | **LEAK** |
| l3-thread-dtor-exit | 9.9023 | 0.848 | 0.487 | clean |
| l3-thread-dtor-teardown | 7.6327 | 0.781 | 0.487 | clean |
| l3-thread-addr | 24.7809 | 0.938 | 0.314 | **LEAK** |
| l3-thread-addr-exit | 9.8532 | 0.847 | 0.485 | clean |
| l3-thread-addr-teardown | 7.6409 | 0.777 | 0.487 | clean |
| l3-thread-parts | 24.7055 | 0.950 | 0.230 | **LEAK** |
| l3-thread-parts-exit | 10.0323 | 0.845 | 0.485 | clean |
| l3-thread-b | 16.8523 | 0.945 | 0.261 | **LEAK** |
| l3-thread-c | 18.6189 | 0.952 | 0.257 | **LEAK** |
| l3-thread-slow | 19.2336 | 0.953 | 0.264 | **LEAK** |
| l7-spawn-proc | -- | -- | -- | SKIP, 40/40 spawns failed |

**Six untreated thread arms leak and six treated ones do not.** Every arm with the destructors off
trips `trk.page_data` and `trk.kernel_used`; not one arm with them on trips a single level counter,
under either placement. The two placements are also indistinguishable from each other -- exit reads
9.90/9.85/10.03 across its three pairs, teardown 7.63/7.64, and the address stride is 0x10 in both.
That is a stronger claim than the one this file made after the first boot, which had one pair and
described the treated arms as merely "stepped".

**The repetition and rate arms confirm the earlier conclusions on the shipped tree.** `l3-thread`,
`-b` and `-c` read 20.58 / 16.85 / 18.62 -- the same workload three times, each growing again, so
live growth rather than a high-water mark. `-slow`, the identical body at a tenth the rate, reads
19.23, inside that spread: rate-invariant, so a leak rather than reclamation lag.

**`l2f` settles the L2 design question.** `l2d` with an explicit `twz_rt_gc()` every iteration reads
**512.0000 against 512.0392** -- indistinguishable. The most generous possible collection schedule
recovers nothing, so no schedule can, and the fix has to move inside ferroc rather than into a
collection cadence. `sys_object_copy` also reads **0 calls for the entire boot**, so decommit was
never reached even once across 22 ops including two that retain 80 MB each.

**Two harness notes, both cost a boot.**

- **`--timeout-scale` is the wrong knob for a long autostart run.** Two runs died at exactly 5m22s
  having completed exactly 15 of 22 ops, with the log cut mid-token. That determinism is the tell:
  it is not a timeout but `qemu.rs`'s heartbeat budget (`tries > run.heartbeat_tries`, 15s per try,
  ~21 tries). `--timeout-scale` only scales `TWZ_SILENCE_TIMEOUT`, and this guest was never silent.
  Use `--heartbeat-tries`. many.py reports it as "no test report (timeout or early exit)", which is
  misleading; the real message is in qemu's stderr: "never shut the guest down".
- **`get_object_stats()` calls `print_all_objects()`** ([obj/mod.rs:1080](src/kernel/src/obj/mod.rs#L1080),
  committed at HEAD). Every `sys_info(ObjectStats)` dumps every object and its notes to the serial
  console, and leakcheck samples that once per iteration: 242,237 lines and 42 MB per run. It is
  common-mode across arms so no comparison here is affected, but it dominates wall-clock and it is
  why the catalogue does not fit a default budget.

### The decommit path is never invoked at all (leak15-decommit)

I suspected our side was silently declining: `decommit_range` does nothing when
`LOCAL_ALLOCATOR.get_id_from_ptr` cannot name the range's object, which is a deliberate
best-effort early return, and my parts probe had already shown that lookup returning `None` for a
2 MiB thread stack. Counters at three levels, per op, settle it:

```
LEAKCHECK-DECOMMIT l0-null                hook_decommit=0 hook_dealloc=0 ranges=0 no_id=0
LEAKCHECK-DECOMMIT l2b-heap               hook_decommit=0 hook_dealloc=0 ranges=0 no_id=0
LEAKCHECK-DECOMMIT l2c-heap-2mb           hook_decommit=0 hook_dealloc=0 ranges=0 no_id=0
LEAKCHECK-DECOMMIT l2d-heap-2mb-touched   hook_decommit=0 hook_dealloc=0 ranges=0 no_id=0
LEAKCHECK-DECOMMIT l2f-heap-2mb-touched-gc hook_decommit=0 hook_dealloc=0 ranges=0 no_id=0
LEAKCHECK-DECOMMIT l2e-heap-small         hook_decommit=0 hook_dealloc=0 ranges=0 no_id=0
LEAKCHECK-DECOMMIT l3-thread              hook_decommit=0 hook_dealloc=0 ranges=0 no_id=0
```

**Both base hooks read zero for every op.** `TwzFerrocBase::decommit` and
`TwzFerrocBase::deallocate` are never called -- not once across 40 iterations of allocating,
touching and freeing 2 MiB (`l2d`), nor across 40 more that force `twz_rt_gc()` every iteration
(`l2f`), nor across 40 thread spawns. So `decommit_range` is never entered, `ranges` and `no_id`
are zero for the trivial reason, and **the hypothesis is refuted: we decline nothing, because we
are never asked.** The `get_id_from_ptr` failure I found on the stack pointer is real and
irrelevant -- that code never runs on this workload.

The counters are placed to make that distinguishable rather than to confirm it: `[0]`/`[1]` sit at
the ferroc-to-us boundary, `[2]` inside `decommit_range` *after* the `len == 0` guard, `[3]` on the
`None` arm. A decline and a never-asked would otherwise produce the same zero -- the same shape as
the stage-table-versus-shutdown control two sections up, and the third time tonight a zero needed a
positive control to be readable.

**Read against the standing prior that ferroc is correct, this says retention is by design.** A
freed huge shard goes back through `finalize_shard`, and only its `Err(slab)` arm reaches
`Arenas::deallocate` and thence the base; the `Ok` arms keep the slab in `free_shards` for reuse.
Caching a hot slab for the next allocation is what an allocator is supposed to do. What makes it
visible here is Twizzler-specific: a retained slab's pages are real frames charged to a long-lived
object, and nothing gives them back. That is `l2d`'s 512 pages/iter, and it is not a bug in ferroc.

**Still open, and now sharper.** The same hooks fire ~265 times per boot at ~4 MiB each under the
test suite (measured by the session that shipped `IS_ZEROED`), so slabs demonstrably *do* retire in
other workloads. The question is no longer "why does nothing come back" but "what does the suite do
that a tight alloc/free loop does not". That is a workload question with a known-positive
comparison already in hand, which is a much better place to start than `finalize_shard` cold.

**It also refutes link one of a chain worth recording.** The session profiling the fault path
proposed: we decline decommits -> frames never returned -> `page_data` stays high -> `page_cond()`
permanently true -> background zeroer permanently disabled -> every allocation memsets inline. They
measured the last three directly and those stand. The first link was mine and it is wrong -- we
decline nothing. Whatever keeps `page_data` high, it is not our early return.

## Open

- **`l7-spawn-proc` is blocked by a runtime bug, not by anything in the harness.** Every spawn
  from inside a compartment fails, for paths that demonstrably exist and that init itself spawns.

  **Facts, all measured:**
  - `/pkg/twizzler/bin/uuhelper` and `/initrd/ls` both `stat` at **126,262,720 bytes** and both
    fail `exec` with `NamingError::NotFound`. Same path, same process, opposite answers.
  - `/initrd/leakcheck` — **the exact path init used to launch this process** — fails `exec` from
    inside that process.
  - `PATH` is set and inherited (`Some("/initrd")`), so the bare form resolves to the same path and
    fails identically.

  **Three causes eliminated, each by evidence rather than argument:**
  1. *Wrong invocation form* (`uuhelper` needs `ls` as argv[1]) — no; the correct form fails too.
  2. *Symlink traversal* — no; `resolve_name` already passes `GetFlags::FOLLOW_SYMLINK`
     ([file.rs:702](src/rt/reference/src/runtime/file.rs#L702)), and I nearly filed this as the
     cause before reading the line.
  3. *No naming handle, falling back to `find_init_name`'s bare-name table* — no; the bare name
     fails as well, with `PATH=Some("/initrd")` set and inherited.
  4. *Wrong namespace — use the on-disk copy the namer can see* — no; `/pkg/twizzler/bin/leakcheck`
     (11 MB, the same directory whose `uuhelper` stats fine) fails identically.

  Every path tried, from inside a compartment: `ls`, `/initrd/ls`, `/pkg/twizzler/bin/ls`,
  `/pkg/twizzler/bin/uuhelper`, `uuhelper ls`, `leakcheck`, `/initrd/leakcheck`,
  `/pkg/twizzler/bin/leakcheck`. **Eight paths, one error**: `NamingError::NotFound`.

  What remains is the symptom, which is reportable on its own: **the fd layer and the spawn
  resolver disagree about what an absolute path means.** A program can be `open`ed that cannot be
  `exec`ed. `unittest` spawns children successfully from its own compartment, so it is not
  spawning-in-general that is broken — the difference between those two contexts is the next thing
  to look at, and it needs someone who owns that code rather than another guess from me.

  **Cost of not stopping sooner: five boots across four hypotheses.** The `stat`/`exec`
  contradiction was sufficient to report from the first measurement, and every subsequent boot
  refined the failure without changing what should be filed. Generating mechanisms was cheap enough
  to feel like progress; it was not progress. The rule that worked elsewhere tonight — report the
  certain symptom, mark the cause suspected, hand it to whoever owns the code — applies here and I
  reached for it four hypotheses late.

  The op stays in the catalogue and self-reports as `SKIP` with the reason, so it costs a row and
  never a wrong answer. **Not a leak result, and not counted as one.**
- **The TLS region is not the leak.** `InternalThread::Drop`
  ([internal.rs](src/rt/reference/src/runtime/thread/internal.rs)) frees it explicitly —
  `tlspool::put` or `LOCAL_ALLOCATOR.dealloc`, both paths covered — and `impl_join` runs
  `prep_cleanup` + `do_thread_gc` synchronously, so the drop happens on join and not on a later
  sweep. Both the TLS and stack pools are capped at 8. mleaks.md M1 is right that
  `dynlink::tls::TlsRegion` has no `Drop`, but this path does not rely on one: it tracks `layout`
  and `alloc_base` in `InternalThread` and frees them itself. Still open: `TlsGenMgr.thread_count`
  increments per thread and is never decremented (its own `TODO` says so), far too small to be
  80 KB; and the monitor's `PerThread`/`SimpleBuffer`, allocated into the *compartment's* memory,
  whose `clean_per_thread_data` callers are unchecked.
- **Per-op wall time is not recorded**, which is what limits the rate test above.

### The stack is not the source — deduced, then measured

The A/B has failed three times to environmental causes, but the pools' own structure settles most
of it without a boot:

- Both pools cap at `MAX = 8` and both `put`s are reached unconditionally from
  `InternalThread::drop`.
- `stackpool::take(size)` matches on size alone, and every spawn uses the same clamped
  `MIN_STACK_SIZE`, so a returned stack always matches the next request.
- `l3-thread` spawns and *joins* each iteration, so at most one stack is outstanding at a time.

**A bounded pool cannot produce unbounded linear growth.** From iteration 2 onward the pool always
holds a matching stack, so the stack is reused, its pages are already resident, and it contributes
nothing new to touch. Growth was linear over 120 iterations with no plateau — so whatever accrues
~21 pages per spawn, it is not the stack. The same argument covers the TLS region, which is pooled
identically and is 1944 bytes regardless.

That leaves the small per-spawn allocations — `InternalThread`, `Box<ThreadSpawnArgs>`, the thread
name, the `all_threads` map node, std's own `Thread`/join packet — landing in heap spans nothing
has touched before. Which is just the L2 finding again: it is not that these are leaked, it is that
touching a fresh page anywhere in the heap object costs a frame permanently.

**Measured, on the fourth attempt** (both arms one tree state apart, `reclaiming = 0` throughout
both — the clean regime, now recorded rather than reconstructed):

| arm | page_data slope | r2 | net |
|---|---|---|---|
| `RECYCLE = true` | 20.8641 | 0.952 | 773 |
| `RECYCLE = false` | **29.1177** | 0.975 | 1102 |

**Direction confirmed, magnitude wrong.** Turning recycling off *adds* cost, so the pool is
genuinely being hit and stacks genuinely are reused — had they not been, the arms would have
matched. The boundedness argument holds and the conclusion stands: **the ~21 pages/iter measured
with recycling on are not the stack.**

But the prediction of "+21, for ~42 total" was wrong: the delta is **+8.25 pages/iter**. That is
the true fresh-touch footprint of a 2 MiB stack for a trivial thread — the `STACK_TOP_ZERO` page
plus about seven pages of libstd setup frames — not the ~21 I had assumed by matching it to the
observed leak. The census agrees: with recycling off, the surviving heap object grows 15.28/iter
against 7.03/iter with it on, and the extra lands in the same object.

**Caveat the tooling raised on its own**: neither A/B arm included the positive control — only
`l0-null` and `l3-thread` ran — so those two boots do not self-validate. `leakplot.py` printed
"no positive control ran. A clean report has not been shown to be capable of being dirty" and
marked the whole report unvalidated, which is correct. Detection capability was established across
leak1-leak8 on the same binary lineage, and the null control was clean in both arms, so the
comparison stands; but the arms should have carried `p1-leak-object` and cost nothing to do so.

The error is worth naming because it is the same one twice: I reasoned "the leak is 21 pages, a
fresh stack would be leaked pages, therefore a fresh stack is ~21 pages". That assumed the quantity
I was trying to explain in order to size its candidate cause. The A/B measured the candidate
independently and it is 8.25 — so stacks account for none of the 20.86 baseline, and would account
for only 28% of the total even when recycling is off.

## Blocked, and on what (resolved -- kept for the record)

*(Resolved: the A/B landed on the fourth attempt -- see "The stack is not the source" above. This
section is kept because the failure modes are the durable part.)*

The stack-recycle A/B (`stackpool::RECYCLE` on vs off) was **unanswered for four attempts**. It is the test of
whether `l3-thread`'s 21 pages/iter are the stack at all: with the pool working, turning it off
should *add* ~21 pages/iter for ~42 total; unchanged at ~21 means the pool is not being hit and the
stack was already the leak. Two attempts died to tree collisions rather than to anything about the
measurement, and the `--bench-iters` ratchet falsifier is queued behind it.

Both are blocked on a diagnostic const another session has in the tree
(`IDLE_TTL_AMPLIFIED = Duration::ZERO` in `handlecache.rs`), which kills net-srv inside
`start_network` — and since init's order is logboi -> devmgr -> pager -> namer -> cache -> **net**
-> display -> sshd -> unittest/autostart, nothing after net starts. leakcheck never runs, and
neither would `--bench-iters`, which goes through unittest.

Worth recording as a design error of mine rather than bad luck: I had planned to run *both* A/B arms
against that const as a constant background, on the reasoning that a constant cancels in a
difference. It does — but **a background has to be survivable to be a constant**, and I never
checked that the tree state I was treating as neutral could reach the measurement at all.

## A bug fell out of it

The boot that stalled produced a real defect, and the way I first described it was wrong in a way
worth keeping:

```
WARN supervisor exception in net: MemoryContextViolation(Read: 2ac0004006)
WARN   fault ip 0x2680036039
```

A read of an unmapped slot -- the compartment's live mappings are `0x2980/0x29c0/0x2a00/0x2a40/
0x2b00/0x2b80...`, and `0x2ac0000000` is not among them. Symbolized against `libnet_srv.so`:
`core::ptr::read::<Unalign<Status>>` under `VirtIONetRaw` under `start_network_direct` -- virtio-net
holds a raw pointer to a device Status field in an object whose `ObjectHandle` it has already
released. The other session's per-slot history gives the order: mapped -> comp-unmap -> count 0 ->
unmapped -> *then* the read. No race needed.

Attribution is clean: `supervisor exception in net,` appears in **0 of 27,967** historical logs and
**2 of 2** runs whose image carried the amplifier. It is invisible without it because the handle
cache's 2s TTL papers over the window.

**My error, and its shape:** I reported this as "the amplifier caught the race it was built to
find". It is not that race -- wrong compartment, wrong slot offset, and the opposite event order.
Establishing that the amplifier *caused* the fault is one claim; concluding it is therefore the
*specific* bug the amplifier targets is another, and I ran them together. Correct description:
**a latent use-after-release in virtio-net, exposed by a diagnostic that removes the handle
cache's grace period.**

### The ratchet falsifier came back against the mechanism

`--bench-iters 3` on `page_fault_zero_fill`, resident `page=` at each mark:

```
47537  651615   47964  1002967   48281  1153405
(begin  end)     (begin  end)     (begin  end)
   pass 1          pass 2           pass 3
```

**The floor does not ratchet** — 47537 -> 47964 -> 48281 is flat. Pages *are* returned between
passes, which is the sawtooth, not the ratchet the mechanism predicts. The peaks climb
(651k -> 1003k -> 1153k), so something accumulates, but not the thing claimed. The run also did not
wedge, so it does not corroborate sysbench.md's `--bench-iters` entry either.

Six points is thin for estimating a *slope*. It is not thin for refuting a *directional*
prediction: flat to within 1.6% across a workload that peaked at 1.15M frames in between is a large
margin against "the floor ratchets", and the observation goes the wrong way, not merely nowhere.

### Reconciling that with L2: the mechanism is real but scoped to surviving objects

The two results looked contradictory and are not. `l2d` measures a **compartment heap object**,
which is never deleted, so its pages are never freed and its high-water mark is permanent. The
sysbench floor measures **global `page_data`**, which includes objects the workload creates and
deletes — and a deleted object's pages come back when it is reaped. Both are true at once:

> **Touched pages are never returned by the object that holds them. They come back only when the
> object itself is deleted and reaped.** A long-lived object — a compartment heap, above all —
> therefore ratchets permanently to the high-water mark of everything it has ever touched, while a
> workload churning short-lived objects reclaims normally.

That is a narrower claim than the one this file made an hour ago, and it survives both
observations instead of one.

**It also makes a sharper prediction, which my own thin data already supports.** If the floor is
the sum of a sawtoothing part (churned objects) and a monotone part (surviving heap objects), the
floor should not be *flat* — it should creep, by a small amount, monotonically. Observed:
47537 -> 47964 -> 48281, **+744 frames across two pass boundaries, monotone, on a floor of 47.5k.**
Flat to 1.6%, but not flat: rising in one direction only, at roughly the scale a compartment heap
would contribute.

### The floor-creep prediction is refuted; the mechanism is not

A 26-bench remeasurement plus the suite twice in one boot (another session's run, 29 boots, one
source hash) tested it. **The floor falls**: ~1,115,000 frames through pass 1, ~1,088,000 through
pass 2 — 27,000 frames *lower* after an additional pass of work. Not a ratchet, not flat, and the
opposite of the creep predicted above. The peaks do ratchet within a pass
(`zero_fill_contended` 1.83M then 1.94M; `file_open_external` 1.10M then 2.14M) and are reclaimed
by the next boundary.

**So the floor prediction is wrong and is withdrawn.** For a mixed workload that creates and
deletes objects, the surviving-heap contribution is smaller than what reclamation returns, and it
does not show up as a rising resident total.

**What survives, and why the obvious objection does not apply.** The natural reading is that the
suite ran under memory pressure — `reclaiming=true` from its second bench onward — which is exactly
the regime where accumulation would be masked, so the floor test cannot discriminate there. The
clean test is a workload that stays *below* the `page_cond` threshold. **That test has already been
run: it is `l2d`.** From its own samples:

```
first: idle=2,899,803  page_data=39,857  reclaimed=0
last:  idle=2,879,800  page_data=59,825  reclaimed=0
       page_cond threshold = idle/2 = 1,439,900   ->  24x below it, never triggered
```

`reclaimed` is **0 across the entire op** — the reclaim thread recovered not one frame — and
`page_data` peaked 24x under the pressure threshold. So the 512.0000 pages/iter at r2 = 1.000 was
measured in precisely the uncontaminated regime, with reclamation provably idle. It was the control
neither run was designed to be.

**Final position, both halves true:**

- **In a long-lived object with no memory pressure, every touched page is retained.** Exact,
  reproducible, reclaim-free: `l2d`, 512 pages/iter, r2 = 1.000.
- **It does not dominate a mixed workload's resident total.** Object churn plus active reclamation
  return far more than it accumulates, so the global floor does not rise — it falls.

The honest summary is that this is a real property of long-lived object heaps and a poor
explanation of whole-system `page_data` behaviour, which is what it was reached for. Caveat on the
refuting run, from the session that produced it: n=1, and the 27k drop is 2.4% of the floor, shown
to be not-a-rise rather than shown to exceed boot-to-boot spread.

## Gotchas found while building this

- **Quiesce must compare level counters only.** Comparing whole samples never converges:
  `trk.allocated`, `mem.page_faults` and the rest of the cumulative set rise forever from
  background activity, so no two consecutive samples are ever equal and every quiesce would
  report failure. Caught before the first boot, but only just.
- **Poking beats sleeping.** The reference runtime's handle cache expires entries at
  `IDLE_TTL = 2s`, but only when someone touches the cache — a sleeping process never triggers
  it, and until the entry goes, `scan_deleted` will not reap the object behind it. `quiesce()`
  calls `twz_rt_gc()` every round, not once.
- **A failed operation must not be graded.** `l7`'s first version fell back to a 1 ms sleep when
  the spawn failed, which would have reported the whole op as clean. It now counts failures and
  emits `LEAKCHECK-SKIP`.
- **A gate call that fails is absent, not zero.** `monitor_api::stats()` returning `None` writes
  `u64::MAX`, and the fit refuses those series. Substituting zero would manufacture a step change
  in seven counters at once.
- **The plots earned their cost immediately.** `max_step_frac` exists because a staircase and a
  gradient look the same in a table of slopes and completely different on a chart. Run
  `leakplot.py -o` and look, at least once per new operation.

## The residual, attributed: small-block churn never reuses (leak18-leak21, 2026-08-19)

### The residual is linear at long horizon (leak18-long, 2026-08-19)

The drift-vs-leak question the treated `l3-thread` left open had a clean discriminator: fragmentation
drift is self-limiting (free space accumulates and gets reused, so growth must decelerate), a leak is
not. One arm, N=220 with a 200-sample tail, 5.5x every previous horizon:

```
l3-thread @ N=220:  trk.page_data 9.02/iter  r2 0.995  maxstep 0.08  net 1,759 pages
slope by quarter of the tail:  9.03   9.22   8.80   8.94
```

**Dead flat. Zero deceleration.** The staircase that failed the gates at N=40 (r2 0.85, maxstep
0.49) resolves into a straight line at scale — the same behaviour that convicted the pre-fix leak
when N went 40->120 — and at this horizon the residual **passes all three gates**. Not self-limiting
drift. The step structure sharpened too: growth is dominated by steps of ~130 pages (520 KiB) every
~18 spawns, plus 4-page steps between; census puts all of it in the compartment's general heap
object (1068 -> 3037 pages) with monitor-heap at 0.33/iter.

Two harness facts from the same boot, both of which matter later:

- **`l0-null` also trips at long horizon**: 0.141/iter, r2 0.963, so leakplot marks the whole report
  unvalidated ("the instrument is measuring itself"). `p1-leak-object` was detected at exactly
  1.000/iter in the same boot and the floor is 64x below the l3 signal, so the verdict survives —
  but the floor's *cause* was misattributed at first, and the correction is in the falsifier section
  below.
- The repetition arms in leak17 (l3-thread/-b/-c each growing ~270 pages again) had already hinted
  the residual was not decelerating; leak18 is that observation with enough horizon to be a result.

### It is not threads: the xfree triptych (leak19-xfree, leak20-local, 2026-08-19)

The suspect was cross-thread free: spawn/join splits alloc and free across threads, and in a
sharded per-thread-heap allocator a foreign free goes to the owning heap's deferred list. Two ops
isolated it with no spawning — one long-lived worker, identical 32 KiB batches (128 x 256 B,
touched) over the same channels, differing only in who drops the boxes — plus, after leak19, a
third with no worker and no channel at all:

| op | threads | channel | who frees | `page_data`/iter | r2 | duty |
|---|---|---|---|---|---|---|
| l3x-xfree-cross | 2 | yes | other thread | 8.898 | 1.000 | 1.00 |
| l3x-xfree-same | 2 | yes | allocating thread | 8.894 | 1.000 | 1.00 |
| l3x-xfree-local | 1 | no | allocating thread | 8.892 | 1.000 | 1.00 |

**Identical to three decimal places.** Cross-thread free is refuted by symmetry, and `local`
removes threads entirely: one thread allocating and freeing 32 KiB of touched 256 B blocks per
iteration strands at churn rate, smoothly (maxstep 0.01 — not even shard-boundary steps).

The refutation also exposed the control this hunt had been leaning on. `l2e-heap-small` churns
**untouched** capacity (`Vec::with_capacity(64)`), and an allocation nothing writes to cannot fault
a page however badly reuse fails — so "single-thread churn reads clean" had never been shown for
touched churn. The same arithmetic un-controls `l0-null`: at ~570 B/iter its floor is consistent
with l0's own transient churn marching at churn rate, too small to distinguish reuse-works from
reuse-never-happens. Both controls were real measurements; neither measured what I had been citing
it for.

### The harvest falsifier: freed small blocks are never reused (leak21-seq3, 2026-08-19)

Last alternative standing: something about max-live > 1 defeats harvest (the batch holds 128 blocks
at once). `l3x-xfree-seq` kills it — identical churn volume, 128 *sequential* alloc -> touch -> free
per iteration, max-live exactly one block:

```
l3x-xfree-seq    8.639/iter   r2 1.000   duty 1.00   maxstep 0.01
l3x-xfree-local  8.889/iter   (same boot)
difference       0.250 pages/iter = 1.02 KiB/iter = the Vec the seq arm does not allocate
```

That difference is the internal consistency check: the instrument resolves a single omitted
allocation site at the right size.

**Verdict: alloc -> touch -> free -> alloc of the same 256 B class never reuses the freed block.**
Not under thread churn, not under batches, not with max-live 1 and nothing else in flight.
Allocation is bump-only for this class; freed blocks are never harvested; resident growth equals
cumulative touched allocation volume. Under L2 (a touched page is never returned by the object
holding it) every compartment heap therefore ratchets at the rate the program allocates-and-touches,
forever.

This closes the "other half" left open after the destructor fix, and reframes the whole L3 story:

- `l3-thread`'s residual ~9 pages/iter is spawn churn (~36 KB of small allocations per spawn+join)
  marching through fresh address space. Nothing thread-specific remains.
- `l0-null`'s 0.139/iter floor is its own sampling transients marching. It is **not** harness
  sample retention: `Sample` is an inline `[u64; 31]` pushed into a `Vec` preallocated with
  `with_capacity(iters)` — nothing is retained per iteration. leakplot's "instrument measuring
  itself" banner is right that the floor is real and wrong about the mechanism; with no-reuse
  established, any process's transient churn shows a floor at its churn rate.
- The 0x10 spawn stride from the destructor fix was read as "reuse of the same shard". It is
  bump-pointer *continuation* through the same shard — consistent with the fix's actual claim (the
  thread heap is recycled, no fresh 4 MiB slab per spawn) but never evidence of freed-block reuse.

**Scope, stated.** Measured for the 256 B class, 220 iterations, release/KVM/smp4, one boot per
arm. Untouched allocations are invisible to a page-counting harness by construction (`l2b`/`l2c`/
`l2e` clean says nothing about their reuse). The huge-allocation path has its own version of this
finding (`l2d`, retention by design in `finalize_shard`); whether the small-class no-harvest shares
a mechanism with it is exactly the open question.

**Where the dive starts.** The free path's routing, in the ferroc port: if frees are misclassified
as foreign (a broken locality/thread-id check), every free lands on the deferred/delayed list, and
the l2f note already records that `collect_inner` walks the sized bins while `reclaim_all` pops
only the *abandoned* list — a deferred list nothing drains reproduces every number above. The
known-positive comparison stands ready: the test suite retires ~265 slabs per boot through the base
hooks, so the full-slab path demonstrably runs in other workloads; block-level harvest has now been
looked for directly and never observed.

**Method notes, three, all cheap to keep.**

- The falsifier cost three launches to land one boot: the first died to a session restart mid-round
  (background sweeps do not survive their session's teardown — check for orphaned xtask/qemu
  children when relaunching, and kill by lane tag); the second built from a tree that had picked up
  another session's temporary `CREATE_PROFILE=true` kernel edit in a 2-minute window and was killed
  as contaminated (timestamps settled it: edit 08:33:34, build close 08:35:25). Box handoffs on a
  shared tree need explicit build-snapshot boundaries in both directions.
- The strongest evidence in this arc was arithmetic on numbers already in hand: quarters of a tail,
  the 0.25-page Vec delta, 4 MiB / 16 bytes. Reach for the division before the next boot.
- When a discriminator refutes your hypothesis symmetrically (cross == same), the first suspect is
  the control both arms were judged against, not a subtler mechanism. The l2e-untouched flaw was
  visible in its own doc comment the whole time.

### The no-reuse finding, root-caused and fixed: THREAD_STARTED never set on main threads (leak22-leak23, 2026-08-19)

> This section supersedes the mechanism left open above: "freed small blocks are never reused" was
> true only for the **main thread**, and ferroc was never the holder of the bug. The prior
> ("ferroc is battle tested; assume correct unless really sure") pointed at the routing, and the
> routing was it.

`alloc.rs` routes an allocation to ferroc only when the calling thread's control block carries
`THREAD_STARTED`; otherwise it serves from the early allocator (bump-only `early_talc`) and
**silently drops the thread's frees** (the `!ts` branch, plus `is_ptr_early_alloc` eating frees of
early pointers by design). The flag has exactly two setters — the spawn `trampoline`
([tcb.rs:53](src/rt/reference/src/runtime/thread/tcb.rs#L53)) and `cross_compartment_entry`
([mgr.rs](src/rt/reference/src/runtime/thread/mgr.rs)) — and `init_core_thread`, the **main
thread's** init path, was not one of them. So every compartment main thread ran its whole life on
the no-free bump path: everything it allocated was leaked at churn rate, and ferroc never saw a
single one of its allocations.

Confirmed three ways in one boot (`leak22-worker`) before fixing:

| probe | result |
|---|---|
| `l3x-xfree-seq` (identical churn, main thread) | 8.64/iter, r2 1.000 — strands |
| `l3x-xfree-worker` (identical churn, trampoline-started thread) | **0.13/iter = the null floor** — clean |
| `LEAKCHECK-MAINHEAP` heap-id fingerprint | `main=0` (early_talc, invisible to the heap list), `worker=<real object>` |

One churn, 65x apart, decided purely by which thread allocates. This also dissolves two standing
mysteries at once: leak15's "ferroc base hooks read 0 across l2d" (the 2 MiB allocations never
went through ferroc at all), and "what does the test suite do that a tight loop does not" (libtest
runs tests on spawned threads; leakcheck ops ran on main).

**Fix** (shipped, permanent): one `fetch_or(THREAD_STARTED)` in `init_core_thread`
([mgr.rs](src/rt/reference/src/runtime/thread/mgr.rs)), mirroring the trampoline — TLS and the
libc TCB are already installed by its caller at that point (`runtime_entry` does `settls` ->
`libc_init_tcb` -> `init_core_thread`). Validated (`leak23-fix`, build `cb86b456be54abe7`):

| series | before | after |
|---|---|---|
| `l3x-xfree-seq` (main churn) | 8.64/iter LEAK | **0.139/iter, floor** |
| `MAINHEAP` | main=0 | **main == worker == same ferroc heap object** |
| `l3-thread` | 9.02/iter | **4.93/iter** (halved, still linear r2 0.995) |
| `l0-null` floor | 0.138 | 0.141 — **unmoved** |
| `p1-leak-object` | 1.000 | 1.000 (control intact) |

**System-wide meaning of the bug**: every single-threaded program leaked 100% of its heap churn;
`init`, `shell`, and every compartment's main-thread work were on this path. Gate-entered server
threads were unaffected (`cross_compartment_entry` sets the flag). All bench numbers cross this fix
boundary as a different allocator regime for main-thread work — pre-fix baselines are pre-fix.

**What survives the fix, stated as the new open items:**

- **`l3-thread` at 4.93/iter, linear at N=220.** The main-thread half of spawn churn is gone; a
  per-spawn ~20 KB accrual remains and does not respond to any allocator-side change measured so
  far. Top candidate from the standing open list: the monitor's `PerThread`/`SimpleBuffer` —
  object-backed, allocated per spawned thread into compartment memory, `clean_per_thread_data`
  callers unchecked. Unattributed until measured.
- **The null floor (0.141/iter) is not churn.** It survived the rerouting unchanged, so ~580 B per
  sample is genuinely retained somewhere in the sampling path (monitor stats gate is the first
  suspect). It is what keeps leakplot's "controls failed" banner on at long horizon; small, but it
  bounds every clean verdict this harness issues.

Two predictions registered before the validation boot were wrong and are kept: "l3-thread drops to
the floor" (it halved) and "the null floor shrinks" (it did not move). Both wrong predictions point
at the same fact — there is a second, smaller retention family that is not allocator routing.
