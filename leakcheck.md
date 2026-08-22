# leakcheck: an empirical leak harness — build notes and first results (2026-08-18)

Companion to [leakplan.md](leakplan.md), which is the design. This file records what was built,
what it measured, and the one finding it has produced so far. The four static audits
([kleaks.md](kleaks.md), [mleaks.md](mleaks.md), [oleaks.md](oleaks.md), [pleaks.md](pleaks.md))
all open with "nothing here was run"; this is the instrument that runs things.

> **Continued in [leakcheck2.md](leakcheck2.md) -> [leakcheck3.md](leakcheck3.md) ->
> [leakcheck4.md](leakcheck4.md).** `leakcheck4.md` is the current state as of 2026-08-22: 42 ops,
> 21 retaining nothing on any readout, objects and threads clean under a repeat-pass test, and the
> remaining items small and named. Read it before acting on a verdict in this file.
>
> **Two things about this instrument that affect how the tables below should be read**, both found
> 2026-08-22 and both since documented:
>
> - **`LEAKCHECK-CLEAN` covers only the fitted counters.** It is printed for ops holding thousands
>   of bytes per iteration in `kalloc` or the userspace heap — all three deliberate-leak controls
>   are marked clean while leaking. A "clean" verdict in this file means "the fits found nothing",
>   not "nothing was retained".
> - **`pages_gained` is one-sided.** `census::diff` keeps only `growth() > 0` over `after.pages`, so
>   objects that shrank are filtered out and objects that disappeared are absent. It ranks
>   candidates; it does not measure retention. The net columns are `objects_before`/`objects_after`
>   and `net=` on the `LEAKCHECK-FIT` line ("measured between two quiesced states").
>
> Also: a `slope=` fitted *during* an op counts anything with asynchronous teardown — thread
> reaping, deferred frame frees, the per-cpu frame pool — as growth. Several verdicts here were
> reached from slopes; `net=` is what says whether anything stayed.

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
| l3-thread | **fixed 2026-08-19/20** — was ~80 KB/thread (ferroc per-thread heap, mlibc localKeys, allocator routing), then a further ~230 KB/spawn of *kernel* memory from unreaped threads, fixed by the reaper thread and on by default since 2026-08-20 01:10 UTC |
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

- ~~**The 16-byte-class residual under thread churn.**~~ **Closed 2026-08-20** -- it is not a leak.
  It is `SlotMgr`'s lazily-allocated per-slot `Box<SlotState>`, one per newly-touched slot, freed
  in `SlotMgr::drop`. See [The 16-byte residual, attributed](#the-16-byte-residual-attributed-2026-08-20).
- ~~**`--kalloc-trap` is unsafe as written**~~ -- still true, and now superseded: use `--track`
  ([kalloc_track.rs](src/kernel/src/memory/kalloc_track.rs)), which records raw return addresses
  and never symbolizes, prints, or allocates inside the allocation path. The trap remains in
  [kalloc_census.rs](src/kernel/src/memory/kalloc_census.rs) and should be deleted rather than
  fixed; nothing needs it now.
- ~~**`l7-spawn-proc` is blocked by a runtime bug**~~ **UNBLOCKED 2026-08-20.** It was `std`'s
  `Stdio::null()` opening `/dev/null`, which Twizzler does not have — in the *parent*, before spawn.
  The op now runs 220/220 and **reports a leak of ~146 KB per process spawn**, attributed to the
  spawning process's own heap. See
  [l7-spawn-proc: it was the call form](#l7-spawn-proc-unblocked-it-was-the-call-form-not-the-path-2026-08-20)
  and [~146 KB per process spawn](#l7-spawn-proc-leaks-146-kb-per-process-spawn-l7leak-2026-08-20).
  **Two new open items fall out of it**, both real and neither investigated:
  1. *Where in the parent's heap.* 137 KB/spawn retained in leakcheck's own local heap;
     `trace_runtime_alloc` is the instrument, and a live-block attributor is wanted for the same
     reason `kalloc_track` was — a class total names a class, not a call site.
  2. *The persistent object.* A new `Persistent`-lifetime object gains 5.36 pages/iter (~4.8 MB
     across the op), i.e. something writes to backing store per process spawn. Untouched.
  3. *Give Twizzler a `/dev/null`, or a `cfg_select!` arm that skips the open like Fuchsia's.*
     Until then `Command::output()`, `Stdio::null()` and anything built on them fail for every
     program, with an error naming neither the device nor the reason.

  ~~Every spawn from inside a compartment fails, for paths that demonstrably exist and that init
  itself spawns.~~ (Original investigation preserved below.)

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

  **RESOLVED 2026-08-20, and the conclusion below was wrong. See
  [l7-spawn-proc: it was the call form](#l7-spawn-proc-unblocked-it-was-the-call-form-not-the-path-2026-08-20).**
  It is not a naming problem at all: every one of these paths spawns fine via `Command::spawn()` or
  `Command::status()`, and only `Command::output()` fails. The eight paths returned one error
  because **the path was never the variable** — all eight attempts held the call form fixed at
  `output()`.

  ~~What remains is the symptom, which is reportable on its own: **the fd layer and the spawn
  resolver disagree about what an absolute path means.** A program can be `open`ed that cannot be
  `exec`ed.~~ `unittest` spawns children successfully from its own compartment, so it is not
  spawning-in-general that is broken — the difference between those two contexts is the next thing
  to look at, and it needs someone who owns that code rather than another guess from me.

  **That last sentence named the right next step and it was not taken.** `unittest` uses
  `Command::spawn()`; this op used `Command::output()`. The comparison that resolved it was already
  written down here, one line above five more boots spent varying the path.

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

### The last per-spawn term: mlibc localKeys, 16 KiB per TCB init, never freed (leak24b, 2026-08-19)

The post-fix `l3-thread` residual (4.93/iter, linear at N=220) is mlibc's pthread-key table.
`initBasicTcb` in the **twizzler sysdep**
([thread.cpp:33](toolchain/src/mlibc/sysdeps/twizzler/thread.cpp#L33)) heap-allocates
`frg::array<LocalKey, PTHREAD_KEYS_MAX>` = 1024 x 16 B = **16,384 bytes = exactly 4 pages**, fully
touched by value-initialization, and no `frg::destruct` of it exists anywhere in mlibc. The
runtime's TCB recycling turns that into a per-spawn leak: `init_new_tls_region` copies the whole
TLS template over a recycled region (fresh and pool paths alike), so the previous table's only
pointer is wiped before mlibc ever runs. 4.00 of the measured 4.78 pages/iter, named by one line.

**Reuse-at-init was considered and is unimplementable**: the template wipe is fundamental to TLS
re-init and destroys the pointer before `initBasicTcb` could test it. The shipped form is the
**exit-path free**, in the sysdep's `__mlibc_handle_thread_exit`: destruct after `run_dtors_for_tcb`
(which reads the table), then null. Sound because `InternalThread::drop` orders the mlibc exit call
before `tlspool::put` (an existing, commented contract), so every region reaches the pool with
localKeys already freed; the caller is the reaper side, so no concurrent access; TCBs are
per-compartment, so no double-free pairing.

Shipped via `cargo toolchain bootstrap --step libc` (mlibc is sysroot, not workspace) + `build-all`.
Validated (`leak24b`, build `1c2bb59feedbf644`, first boot also carrying OMAP_SHARDED):

| series | leak23 (pre) | leak24b (post) |
|---|---|---|
| `l3-thread` `page_data` | 4.93/iter | **0.548/iter** |
| compartment-heap grower | +1051 pages | **+12 pages** (0.05/iter) |
| residual composition | — | monitor-heap 0.35 + floor 0.14 |
| seq / worker / l0-null | 0.139 / 0.137 / 0.141 | 0.141 / 0.138 / 0.140 (unchanged) |

Side effect recorded by another session's bench arms: the THREAD_STARTED fix alone moved the whole
baseline (file_open 7.6us -> 4.0us, create 30 -> 23.9us) — main-thread allocation had been taking a
globally-locked bump allocator for every program's whole life.

**Found in the same boot, owned by the omap change — reported, fixed same day:**
`sys_object_stats` returned `nr_objects = nr_mapped = 0` constantly (286/252 pre-omap) because
`get_object_stats` was the one call site the omap remodel missed (an audit truncated by `| head`);
handles/ties and `sys_enumerate(Objects)` were never affected. p1 stayed "detected" only through
counter redundancy — its calibrated `obj.objects 1.000/iter` line was dead until the fix, and its
restoration (`omap-statsfix`: 1.0000/iter, r2 1.000, net 220) is the closure evidence. That boot
also independently corroborated the localKeys fix (`l3-thread` page_data 0.53/iter vs 0.55 here).

**Open intermittent — `l3-thread` kernel-side kalloc, two contradictory data points:**
`leak24b` read `mem.kalloc_bytes` at 1,980 B/spawn (r2 0.973, duty 0.32, net 370 KB/220 spawns);
`omap-statsfix`, same tree one boot later, read 222 B/spawn at r2 0.06, net 34 KB — no
reproduction. So it is bursty rather than steady retention, an off/on A/B at one boot per arm
cannot discriminate, and it needs repetition arms in one boot (the l3-thread-b/-c pattern) before
any attribution. Logged with both numbers; not counted as a leak.

**Remaining open, post-everything:** the 0.14/iter background floor (uniform 4-page steps at
irregular intervals into compartment + monitor heaps — same quantum as one localKeys table, and
a background thread spawn/recycle would burn exactly that; unverified) and the monitor-heap
0.35/iter under spawn churn. Spawn+join is now ~2 KB/spawn of kernel-side kalloc and ~1.4 KB of
monitor-heap, from 80 KB when the campaign started.

### The null floor, attributed and fixed: the monitor never leaves its early allocator (leak25-leak26, 2026-08-19)

The 0.14/iter floor was mostly a third instance of the same bug family as the main-thread fix.
Amplifier arms attributed it (`leak25-floor`): 11x monitor stats gate calls scaled the floor to
0.959/iter r2 0.999 (0.082 pages ~ 335 B retained per call), 11x kernel stat syscalls moved
nothing, and the census put the growth entirely in the monitor's heap. Reading the routing:
`alloc.rs` sends every IS_MONITOR allocation to `alloc_early` (bump-only early_talc) and the
monitor never reaches the allocator switch, so `early_allocs_frozen` stays false in its runtime —
which makes `do_dealloc` drop **every monitor free, forever**. The `IS_MONITOR ->
LOCAL_ALLOCATOR.dealloc` branch was dead code. Every transient the monitor allocated per gate
call, per spawn, per background scan was retained.

**Fix**: `dealloc_early` on `LocalAllocator` (free into early_talc — symmetric, since every
monitor pointer comes from it) and the IS_MONITOR dealloc branch routed there. Compartment
early-free semantics unchanged (their drops happen earlier, at `is_ptr_early_alloc`, by design).

Validated (`leak26-monfree`): **the null control reads clean for the first time in this
harness's existence** — l0-null 0.140 -> 0.060 sub-gate residual, l0-stats10 0.959 -> no-slope,
the monitor-heap grower absent from the census, l3-thread's monitor term 0.35 -> 0.08. p1 intact
at 1.0000. The l0-stats10 arm doubles as the fix's own regression guard from here on.

**Risk, stated before the wide coverage exists**: the monitor had never run with a working free
path, so any latent monitor double-free/UAF was masked by frees being no-ops and is live now.
leak26's ~1,300 gate calls + 220 spawns of real frees ran clean; the full test matrix (riding
another session's frame-cache ladder, my fix constant in all arms) is the sufficiency test, and
the fix gets reverted on any monitor-side crash signature there.

**Remaining, toward slope-zero everywhere:**
- l0-null residual 0.060/iter (r2 0.90, sub-gate): leakcheck's own compartment heap creeps ~51
  pages/op while global page_data moves less — concurrent shrinkage elsewhere muddies per-object
  vs global accounting. Unattributed.
- l3-thread page_data 0.318/iter (r2 0.976): still gate-flagged; census rows now sum to well
  under the global slope, same accounting muddle.
- **The kernel-side spawn signal is now the dominant open item**: trk.kernel_used 79.7/iter
  r2 0.921 maxstep 0.33 (at the gate edge) + kalloc 2,730/iter r2 0.99 in leak26, against 27/0.66
  and 10/0.24 in earlier boots — bursty across boots, trending within this one. Kernel domain:
  thread repr lifecycle, cleanup_exited pacing, page-table frame churn are the starting suspects.

### The monitor-free fix wedged debug boots; returned guarded (monfree-bisect/-guarded, 2026-08-19)

The pre-flagged risk fired within hours, and the pre-agreed protocol caught it cheaply. A 10/10
debug-kvm wedge (log stops at 'bootstrapping runtime monitor', kernel idle) appeared in another
session's validation matrix; their cache-off reproduction excluded their own feature, the delta
window pointed here, and a one-hunk revert boot confirmed it: PASS 51s 55/55 against 10/10 wedges
with the fix in.

**Mechanism** — the fix's stated assumption ("every monitor pointer comes from early_talc") has a
real exception: the monitor frees bootstrap-allocator pointers before `bootstrap_alloc_slot` is
registered. The old dead branch dropped those harmlessly; the unguarded fix fed them to
`early_talc.free`, corrupting a talc that never allocated them, and debug-boot timing hit it in
monitor bootstrap every time.

**Shipped form** (validated on the wedge config, PASS 55/55): the free is gated on
`is_ptr_early_alloc(ptr)` — sound in both directions, since every legitimate monitor allocation's
slot is in early_talc's object list and no bootstrap pointer's is. Foreign pointers drop exactly as
they always did. The leak numbers (null control clean, stats amplifier flat) are re-proven by the
leak27 run whose l0-stats10 arm is the standing regression guard.

Method note: this cost two boots total because the failure protocol was negotiated before the
failure existed — signature split, cheapest-bisect-first, single-variable boots. The same fix
without the pre-agreement is a day of cross-session finger-pointing.

### Slope zero (leak27-zero, 2026-08-19)

**The null control reads statistical zero**: `trk.page_data` 0.002/iter at r2 0.04 — noise — with
both controls valid and p1 at exactly 1.0000. The floor's full decomposition, each term measured
and fixed independently: 0.060 the harness touching its own sample buffer (pre-touched run-lifetime
buffer), 0.082 the monitor retaining per gate call (guarded early-free fix), and the remainder
vanished with the monitor fix (l0-slow500 now shows nothing at all, so no time-driven background
survives on a healthy tree).

Other verdicts from the same boot: l1a/l1b clean; **l1c names SlotMgr retention at exactly
16 B per touched slot** (kalloc 16.0000/iter r2 1.000 — the regionremodel.md limitation, real but
bounded by slot-space span; ~4 table frames per 220 fresh slots); l3-thread's kernel_used signal
(73/iter, sub-gate noisy) tracks FA_PERCPU_CACHE=off, not retention — with the cache on it reads
clean, so it is the address-span artifact `kernel_used` is documented to measure, and leak26's
trending version was the corrupted-allocator boot.

**Spawn-join stands at 0.220/iter (~0.9 KB/spawn), 94x down from the campaign's start (20.6),
not yet zero**: 0.07 in monitor-heap (per-spawn monitor bookkeeping, sub-KB), 0.15 diffuse below
the census grower threshold. Also open: mem.kalloc_bytes trends at ~2.7 KB/spawn (r2 0.99, duty
0.39) — kernel-heap growth under churn, possibly the kernel-ferroc sibling of the userspace
no-reuse story, unconfirmed and unowned.

### The kernel half, named: ~11% of spawned threads are never reaped (leak29-kalloc, 2026-08-19)

The `mem.kalloc_bytes` signal left open by leak27 ("~2.7 KB/spawn, bursty across boots, unowned")
is not a per-spawn leak at all. It is **whole threads whose teardown never runs**, and the reason
it looked bursty across boots is that the quantity varies because it is a *count of stuck threads*,
not a per-spawn cost.

**The missing instrument.** `mem.kalloc_bytes` is net-live -- `alloc` does `fetch_add(layout.size())`
and `dealloc` does `fetch_sub` ([allocator.rs](src/kernel/src/memory/allocator.rs)) -- so a slope on
it is bytes the kernel allocated and never freed. It says how many; it never said which. A kernel
allocation census by size class ([kalloc_census.rs](src/kernel/src/memory/kalloc_census.rs),
`InfoKind::KallocCensus`, sampled by leakcheck at op boundaries) says which. Gross alloc/free counts
per class, not only a net: a class with 8,800 allocations and 8,580 frees and a class allocated 24
times and never freed have the same net and want different investigations.

**What it read for `l3-thread`, N=220** (release/KVM/smp4, build `d9b3be84e1b14fb6`):

```
size=8192 alloc=220 free=196 net_count=24 net_bytes=211968   (8832 B each)
size=4096 alloc=1066 free=1042 net_count=24 net_bytes=113664 (4736 B each)
size=1024 alloc=1375 free=1351 net_count=24 net_bytes=46272  (1928 B each)
size=288  alloc=1066 free=1018 net_count=48 net_bytes=13824  (two per thread)
size=512  alloc=733  free=709  net_count=24 net_bytes=12288
size=192  alloc=220  free=196  net_count=24 net_bytes=4608
size=48   alloc=220  free=196  net_count=24 net_bytes=1152
```

**`net_count = 24` in seven different size classes at once**, with `alloc = 220` -- exactly one per
spawn -- in four of them. That is not a leak of one structure. It is 24 complete `Thread`
allocation sets out of 220 spawns, 16,840 bytes each.

**`trk.kernel_used` corroborates independently, and it is the expensive half.**
`KERNEL_STACK_SIZE` is 2 MiB, and a `KernelStack` returns to the free list only when its `Thread`
drops. 24 retained threads predicts 24 x 512 = 12,288 frames; measured net is **12,451**. Two
counters, different subsystems, same 24 threads.

**It accumulates across the boot and is never returned.** Absolute `trk.kernel_used` at each arm's
first and last sample, three identical arms in one boot:

| arm | kernel_used first -> last | kalloc net |
|---|---|---|
| l3-thread | 95,049 -> 107,528 | +404,336 B (24 threads) |
| l3-thread-b | 107,480 -> 119,929 | +404,224 B (24 threads) |
| l3-thread-c | 119,874 -> 128,245 | +421,344 B (25 threads) |

**+33,196 frames = ~130 MiB of kernel memory pinned by 660 spawn+joins**, each arm starting where
the last ended. Amortized that is ~200 KB of kernel memory per spawn+join -- two orders of magnitude
above every userspace term this campaign has chased, and it was sitting under a counter that had
been written off as an address-span artifact.

**Both controls held in the same boot**, which is what makes the number readable: `l0-stats10` reads
`net_bytes=0` **exactly** (the monitor gate path retains no kernel heap), and `p1-leak-object` reads
+201 B of kernel heap per leaked object -- a known leak, at a plausible size, in the run that
reports 24 threads. `l0-null` reads *negative* (-616 B/iter): the boot's own backlog draining, ~7
threads over the op, which is the first direct evidence that reaping works at all and is merely
slow.

**Why no existing counter saw it.** `thr.threads` and `thr.pending_exit` are both flat at zero
slope, because `exit` removes the thread from `ALL_THREADS` *before* `do_schedule` pushes it to the
per-cpu `exited` list. A thread waiting to be reaped is in neither population. sample.rs's own note
-- "nr_pending_exit is the one to watch: cleanup_exited pops a single thread per call" -- named the
right mechanism against a counter structurally unable to show it.

**Suspected mechanism, not yet confirmed.** `Processor::cleanup_exited`
([processor.rs](src/kernel/src/processor.rs)) pops **one** thread per call, and its only routine
caller is the idle loop at `iter % 100 == 0` ([main.rs](src/kernel/src/main.rs)) -- a loop that ends
each pass in `halt_and_wait()`. So the reap rate is roughly (idle wakeups)/100 per cpu, independent
of how fast threads are exiting. The l0-null drain rate (~7 threads over one op, order 2-3 per
minute) is consistent with that arithmetic. Confirmation is a backlog counter, not more reading:
`nr_exited_backlog` should track the retained-thread count, and `nr_reaped` flat while the backlog
is non-zero distinguishes "reaping stopped" from "reaping is behind". Registered before the run.

**Constraint any fix has to respect**: `Thread::drop` -> `IdCounter::release` takes a *sleeping*
`Mutex` ([idcounter.rs](src/kernel/src/idcounter.rs#L83)), so reaping can block -- which is
presumably why it is deferred and throttled in the first place, and why "drain harder from the idle
loop" is not obviously safe. A dedicated reaper kernel thread, woken on push, is the placement that
can block legitimately.

### What the counter set cannot see: an audit of deferred consumers

The reap bug is not interesting because a queue backed up. It is interesting because **no counter
in the harness could have shown it**, so the instrument reported a healthy system through 130 MiB
of accumulation. That is a property of the counter set, not of the bug, and it is auditable without
finding a bug in each structure first: enumerate the places work is handed to a deferred consumer,
and ask which counter would move if that consumer stopped.

| structure | consumer, and its cadence | counter that would move |
|---|---|---|
| `Processor::exited` (per-cpu) | idle loop, **every 100th pass, one item per call**, then halts | **none** — `exit` unregisters from `ALL_THREADS` before pushing here, so `thr.threads` and `thr.pending_exit` are both blind |
| `REQUEUE` (global) | `requeue_all`, every idle pass + `finish_blocking`, batches of 8 | `thr.threads` — the thread has not exited and is still registered |
| `Processor::ipi_tasks` (per-cpu) | `run_ipi_tasks` | none — but bounded, since the issuer waits on `outstanding` |
| object pending-delete | `scan_deleted`, idle loop, **every 1000th pass, bsp only** | `obj.pending_delete` |
| monitor deferred unmap | `Unmapper` thread | `mon.space_mapped` - `mon.space_active` |
| pager inflight requests | pager completion path | `trk.pager_outstanding` |
| per-cpu frame cache (`PERCPU_FRAME_CACHE`) | frame allocator fast path | **unaudited** — whether cached frames count as idle or used is not established here |

**The durable form of this is a review question, not a table.** "If this queue stopped draining,
which counter moves?" is answerable by whoever writes the queue, at the moment they write it, and
costs a minute; the table below is the retroactive version and will be stale within a month. Ask it
of every new deferred consumer.

The two rows with a slow cadence are `Processor::exited` and object pending-delete, and only one of
them is observable. That is the whole difference between a bug that took a purpose-built census to
find and one that any boot's counters would have shown. Note also that the covered rows are covered
by *population* counters (a thing is still in a set someone counts) rather than by anyone having
thought about the queue -- so coverage here is incidental, and the next per-cpu structure added
will be invisible by default.

Credit: the audit framing is llama-twz-65's, in response to the finding above; the observation that
a per-cpu structure with a deferred consumer is invisible to global accounting *by construction* is
theirs as well.

### Pre-registered: the reap gate, and a cleanup thread (leak30/leak31, queued 2026-08-19)

Written before the boots, so the predictions cost something if they are wrong.

**The mechanism, restated after two corrections.** The reap is not merely throttled, it is gated on
a *safe point*, and for a real reason: `Thread::drop` -> `IdCounter::release` takes a **sleeping**
mutex ([idcounter.rs:83](src/kernel/src/idcounter.rs#L83)). So both existing reap sites are
conservative --

- `schedule_stattick` reaps one thread per tick, only when the interrupted thread `is_in_user()`,
  is not critical and holds no mutex ([sched.rs:1243](src/kernel/src/processor/sched.rs#L1243));
- the idle loop reaps one per hundred passes, where blocking would deschedule an idle thread --
  the wedge `schedule` documents at [sched.rs:915](src/kernel/src/processor/sched.rs#L915).

-- and both conditions are **anti-correlated with thread churn**: a spawn/join loop is in the kernel
or blocked for nearly all of its time, and an idle cpu never satisfies `is_in_user()` at all.

**A hypothesis I held and dropped, with the evidence that killed it.** After ea's finding that a
hoisted `#[thread_local]` address makes `with_disabled` protect the window but not the address
computation, I proposed that exiting threads are pushed onto a cpu that never drains them. The
disassembly refutes it for this build: `schedule()` disables interrupts before `do_schedule`,
`current_processor()` is a genuine out-of-line call after the `cli`, and its body re-reads
`%fs:0x0` at execution time -- there is no precomputed address to go stale. ea checked the pop side
independently, same answer. **Source-level reasoning about that hazard is not weak evidence, it is
no evidence: whether an address is hoisted is a codegen fact, and `objdump` settles in two minutes
what careful reading gets confidently wrong.**

**Predictions.**

1. `l3-thread-userspin` -- spawn+join, then burn 2 ms in *user* mode -- retains far less per spawn
   than `l3-thread`, despite spawning identically. This is the safe-point account's falsifier: it
   manufactures exactly the user-mode ticks the gate waits for. Unchanged retention means the gate
   is not what limits the reap and the account above is wrong.
2. The per-cpu watermark reads **similar depths across cpus**, not one cpu pinned high. Pinned
   instead would put ea's mechanism back on the table.
3. With `--reap=thread`, `thr.exited_backlog` returns to ~0 between operations and both
   `mem.kalloc_bytes` and `trk.kernel_used` lose their slopes on every `l3-*` arm.
4. `l3-thread-x10` retains ~10x `l3-thread` in the legacy arm (retention is per spawn, not per
   iteration) and ~0 in the treated arm.

**The change under test.** A reaper kernel thread ([thread/reaper.rs](src/kernel/src/thread/reaper.rs)):
drains every cpu's cleanup list, drops outside all locks, runs at BACKGROUND and donates REALTIME to
itself while the backlog exceeds 8 threads (16 MiB of pinned stacks) or memory is low. It has
neither existing constraint -- an ordinary kernel thread may block, and it drains without a per-pass
bound. It takes only entries whose `is_active_running()` is false, because a thread is pushed to the
list *before* it switches away and is briefly listed while still running on the stack its drop frees;
that check is what makes one shared reaper sound instead of one pinned per cpu.

**Default off** (`--reap=thread` opts in), so both arms come from one build and one tree state, and
so an unvalidated behaviour change does not silently move four other sessions' baselines. Flipping
the default later is its own announced boundary.

### Confirmed and fixed: the reap is gated on user-mode ticks (leak30/leak31, 2026-08-20)

Two arms, one build each from one tree state, differing only by `--reap=thread`. Both PASS, both
controls valid in both arms (`p1-leak-object` at exactly 1.0000 obj/iter; `l0-null` at +32 B of
kernel heap in the legacy arm).

**The falsifier answered for the mechanism.** `l3-thread-userspin` spawns and joins identically to
`l3-thread` and then burns 2 ms in *user mode*, manufacturing exactly the ticks the stattick reap
gate waits for:

| legacy arm | l3-thread | l3-thread-userspin |
|---|---|---|
| kalloc net | **+539,104 B** (2,450/iter) | **-33,376 B** (-152/iter) |
| `trk.kernel_used` | slope 80.9, r2 0.926, net **+16,578** frames | slope 0.089, r2 0.18, net **-11** |
| `thr.exited_backlog` | slope 0.160, r2 0.987, net **+32** | slope 0.017, r2 0.15, net **-2** |

16,578 frames is 32.4 x 512 -- exactly 32 threads x 2 MiB, agreeing with the kalloc count of
retained `Thread` allocation sets. **User-mode burn does not slow the leak, it reverses it**: the arm
that spawns just as hard drains the backlog it inherited. A pacing story does not have to produce
that; this one did.

**The severity is a function of spawn rate, not a constant.** At ten spawn+joins per iteration the
legacy arm reads `thr.exited_backlog` slope **5.96/iter at r2 0.9998**, net **+1,258 threads**, and
`trk.kernel_used` net **+667,042 frames = 2.54 GiB** -- a fifth of a 12 GB guest, from a workload
whose live set is one thread at a time -- with `trk.reclaiming` at **0 for the entire op**, because
this is legitimately-allocated kernel memory that no pressure calculation can see. The retained
*fraction* rises with the rate: 16% at one spawn per iteration, 57% at ten. A fixed leak rate cannot
do that; a fixed drain rate against scaling production is the only shape that does. So this is a
scaling limit on thread churn rather than a constant overhead. (Framing: twizzler-ea.)

**The fix, measured.** A reaper kernel thread ([thread/reaper.rs](src/kernel/src/thread/reaper.rs))
that drains every cpu's cleanup list, drops outside all locks, runs at BACKGROUND and donates
REALTIME to itself while the backlog exceeds 8 threads or memory is low:

| | l3-thread legacy -> reaper | l3-thread-x10 legacy -> reaper |
|---|---|---|
| kalloc net | +539,104 B -> **+32 B** | +21,208,560 B -> **+1,056 B** |
| `trk.kernel_used` net | +16,578 -> **+7** frames | +667,042 -> **-9** frames |
| `thr.exited_backlog` | 0.160/iter -> **0.0000, r2 1.0000** | 5.96/iter -> **0.0000, r2 1.0000** |
| `thr.reaped` | -- | -- -> **10.0000/iter, r2 1.0000, net 2,200** |

`+32 B` over 220 spawns is *identical to `l0-null`'s floor in the same boot*, so spawn+join now
retains nothing measurable in the kernel heap. `thr.reaped` at exactly 10.0000 per iteration over
2,200 spawns is the cleanest statement of it: production 10/iter, reaping 10.0000/iter, backlog
never leaving zero. The priority boost is load-bearing rather than a refinement -- against a
workload generating 5.96 threads/iteration of backlog, a plain BACKGROUND thread would rarely be
scheduled at all.

**The struct sizes name the census classes exactly**, printed at boot under `--diag`:
`sizeof Thread`, `LockTrackerInner = 1928`, `ArchThread = 6528`. The census's `Thread` class is
`sizeof Thread` **plus the 64-byte `Arc` header**; its 1928-byte class is `LockTrackerInner`.

> **The `Thread` class number is derived, not a label**, and nothing in `thread.rs` points here. Any
> change to `Thread`'s size silently renames this class, so a reader grepping the old number finds
> nothing and may conclude that threads stopped being allocated. Current values:
>
> | | `sizeof Thread` | census class (`+ 64` for the `Arc` header) |
> |---|---|---|
> | before the intrusive registries | 8768 | 8832 |
> | after (two `RBTreeAtomicLink`s on `Thread`) | **8832** | **8896** |
>
> ⚠️ **8832 means two different things either side of that row.** The *new* struct size is
> numerically equal to the *old* census class. A search for 8832 in an old boot log, an old revision
> of this document, or a pre-conversion measurement **does not fail -- it succeeds, with the wrong
> thing.** A silent zero is something we have all learned to distrust; a silent *hit* reads as
> confirmation. Date any 8832 you find before you believe it.
>
> **Measure it; do not compute it.** The links are 24 bytes each, but the struct grew by 64, not 48 --
> padding. `8768 + 2*24` predicts 8816 and is simply wrong. Two ways to get the truth:
>
> - from a boot: the `[thread] sizeof Thread=` line, printed by `log_thread_sizes()` under `--diag`;
> - without booting, in about fifteen seconds -- add a deliberately-wrong const probe, and the
>   compiler prints the real size in the mismatch:
>
>   ```rust
>   const _: [(); 0] = [(); core::mem::size_of::<Thread>()];
>   // error: expected an array with a size of 0, found one with a size of 8832
>   ```
>
>   Remove it and re-run `cargo check-all --kernel` to confirm the tree is clean again.
>
> Update this table when you change the struct.

`ArchThread` is 6,528 of the 8,832 -- two 3,072-byte xsave regions -- so a stranded thread costs
8.8 KB of heap before its 2 MiB stack. The stack is the headline; the heap is the detector.

**Prediction scorecard**, written before the boots:

1. userspin retains far less -- **confirmed**, and more strongly than stated (sign reversal).
2. per-cpu depths similar across cpus -- **wrong**. Observed `cpu0=137/140 cpu1=157/158 cpu2=0/1
   cpu3=0/1`: concentrated on the two cpus running the workload, zero on the idle two. It still
   discriminates in the right direction -- stranding predicts depth on a cpu the workload is *not*
   on -- but I predicted the wrong shape for my own mechanism. A busy cpu neither idles to drain nor
   offers user-mode ticks, so concentration is what pacing predicts here.
3. treated arm: backlog ~0, slopes gone -- **confirmed exactly**.
4. x10 retains ~10x legacy -- **confirmed, understated**: observed 37x (5.96 vs 0.160), because the
   retained fraction itself scales. The direction of the miss is the informative part.

**"Legacy" does not mean "before", and the word will mislead.** Both arms sit on top of everything
else that landed in this tree the same night -- another session's frame-pool rework, its lazy
zeroing, its page-table zeroing check -- all unconditional in both. So this A/B isolates
`--reap=thread` and nothing else, which is what it was for; but **neither arm can speak to the
pre-tonight kernel.** Anything appearing in *both* arms (the 16-byte-class residual below, 50 vs 51)
needs the pre-tonight tree to attribute, not the legacy arm. Recorded because the next reader of
these tables will otherwise take "legacy" for "baseline". (Raised by twizzler-ea about their own
A/B, and it applies here identically.)

**n=1 per arm, and what that does and does not buy.** The between-boot evidence is a single pair.
It is defensible here only on effect size -- `thr.exited_backlog` 0.160 -> 0.0000 at r2 1.0000,
`trk.kernel_used` +16,578 -> +7 frames, kalloc +539,104 B -> +32 B, three to four orders of
magnitude -- and on the 200 within-arm samples behind each slope, which is where the r2 comes from.
What n=1 cannot exclude is a boot-to-boot confound correlated with the flag. Unlikely, since the
flag is the only difference between the images, but not excluded by anything measured here. Stated
rather than left for the effect size to imply. (Prompted by twizzler-ea's n=3 spread analysis: at
small n, "clears its own spread" is a claim about the method before it is a finding about the
change -- `object_create_delete_contended` runs at +-53-59% and cannot carry an A/B at that n at
all.)

**Shipped state**: default **off** (`--reap=thread` opts in). One boot per arm, release/KVM/smp4.
Flipping the default is a behaviour change to a shared tree and gets its own announced boundary.

### The census's own floor: +-1 to 3 allocations, not bytes retained

The treated arm's `l3-thread` reads `net_bytes=32`, and `l0-null` reads the same in both arms. That
is **not** 32 bytes retained. It is `net_count = 1` in whichever size class has the most traffic:

```
l0-null    size=32  alloc=10261  free=10260  net_count=1   (legacy)
l3-thread  size=32  alloc=11061  free=11060  net_count=1   (treated -- its entire net)
```

Three things say jitter rather than retention: it does not scale with work (220 iterations give 1,
2,200 spawns give +3 in one arm and -2 in the other); its **sign varies** across arms and classes
(`size=128` reads -1, `size=64` reads both); and it lands in the hottest class, which is exactly
where a snapshot is most likely to catch an allocation in flight. The census brackets an op with two
snapshots, so one allocation that is allocated-but-not-yet-freed at the second snapshot reads as a
net of one.

**So the census's floor is +-1 to 3 allocations in the hot classes, roughly +-100 bytes per op.**
Anything at that scale is noise. Read `net_count`, not `net_bytes`, when the number is small: one
in-flight 8832-byte `Thread` would read as a 8.8 KB "leak" by the same mechanism.

**A residual the reaper does not touch, and it is real.** The same table shows, at 2,200 spawns
only:

```
l3-thread-x10  size=16  alloc=6723  free=6672  net_count=51   (legacy)
l3-thread-x10  size=16  alloc=6650  free=6600  net_count=50   (treated)
```

51 and 50 across two independent boots is far too consistent for jitter, and it is present in both
arms, so it is not reaping. ~800 bytes per 2,200 spawns (0.36 B/spawn), absent entirely at 220
spawns -- so scale- or threshold-triggered rather than a flat per-spawn rate. Four to five orders of
magnitude below the leak just fixed (~230 KB/spawn), and unattributed. `--kalloc-trap=16:16:N:8`
exists to name it and has not been run.

Cleanest positive control the census has produced, from the same table: `p1-leak-object` treated
reads **exactly +220 in the 4096-class and +220 in the 512-class** for 220 leaked objects -- one
allocation each, no more, no less, giving 5,248 B of kernel heap per leaked object. That number was
unreadable in every previous boot because the reap backlog was draining through the same window; the
fix removes a large noise source from every per-object kernel-heap measurement this harness makes.

### Default flipped on, validated across every config (reapflip-*, 2026-08-20 01:10 UTC)

`REAP_THREAD` now defaults to **true**; `--reap=legacy` restores the old behaviour and remains a
runtime flag so an A/B still needs only one build. **10 boots, 0 failures.**

| stage | configs | result |
|---|---|---|
| smoke (gates the rest) | release-kvm-smp4 | PASS 55/55, 13s |
| matrix, 2 rounds each | release-kvm-smp4, **release-kvm-smp1**, **debug-kvm-smp4** | **6/6 PASS** 55/55 |
| TCG | release-nokvm-smp1 | PASS 55/55, 55s |
| regression, **no reap flag at all** | release-kvm-smp4 | PASS |

The two bolded configs are the ones the reaper had never executed on, and smp1 is the case that
mattered: the reaper's blocking path (`Thread::drop` -> `IdCounter::release` on a sleeping mutex,
now reached from a dedicated kernel thread) competing for the single cpu with the workload producing
the backlog. Two clean rounds each.

**The regression boot is the one that proves the default rather than the mechanism.** It passes no
`--reap` flag, so everything below happened because the default took effect:

```
[reap] reaper thread started (id 11)          <- no flag passed
l3-thread       kalloc net +32 B (the census floor), kernel_used net -6
                thr.exited_backlog slope 0.0000  r2 1.0000
l3-thread-x10   kalloc net +848 B (legacy: +21,208,560), kernel_used net 0
                thr.exited_backlog slope 0.0000  r2 1.0000
                thr.reaped         10.0000/iter r2 1.0000, net 2,200
p1-leak-object  obj.objects 1.0000/iter exactly -- control valid
```

This is also an **independent replication** of leak31 in a different build: the treated condition now
has n=2 across two builds, which is the boot-to-boot confound that leak31's n=1 could not exclude.
`p1`'s kernel-heap cost reproduces to four figures as well -- 5,247.3 B/object here against 5,248 in
leak31 -- which is a stronger statement about the census's repeatability than anything designed to
test it.

**The residual hunt failed twice, on two different instrument faults, and the second is the more
instructive.** A second boot (`reap16-trap`) with the stride corrected to `every=2000` put a firing
inside the workload and named a 16-byte allocation on the spawn path (`start_new_user` <-
`sys_spawn`). But the class's net was never printed, because:

- **The symbolizing trap has an observer effect that dwarfs its target.** `panic::backtrace(true, ..)`
  loads DWARF through addr2line and retains the context: `mem.kalloc_bytes` for that op read
  **+878,008 B at duty 0.015 and maxstep 1.000** -- one single step, the harness's own gates
  correctly calling it background work rather than per-iteration retention. An instrument that
  retains 878 KB cannot measure an 800 B effect in the same window.
- **`report_kalloc` prints only the top 16 classes by `|net_bytes|`**, and that op touched 58. The
  trap's own allocations occupied the top of the table and pushed the 16-byte class out of view.

Neither is a kernel fault and both are mine. The fix's own counters were untouched by all of it --
`thr.exited_backlog` 0.0000 at r2 1.0000, `thr.reaped` 10.0000/iter, `trk.kernel_used` net 0 -- so
the validation stands; only the residual hunt failed. Next attempt wants the trap and the census in
*separate* boots, and a `--kalloc-classes` knob to print a named class unconditionally.

**That withdrawal was itself wrong, and the correction is the most instructive part of the arc.**
I wrote here that the residual was "unreproduced under a third ordering" on the strength of that
boot showing no 16-byte row. But that boot's reporter printed only the top 16 classes by bytes, and
the trap's own 878 KB of retained DWARF context filled the table -- **the class was there and the
instrument could not show it.** I read an unpowered zero as evidence, which is the exact failure
this file spends several sections pressing other people about.

Settled by fixing the reporter (`report_kalloc` now prints every class whose count does not balance,
and `LEAKCHECK-KALLOC-TOTAL` carries `unbalanced=`) and re-running with the census alone, no trap
(`reap16-repro`):

```
l3-thread-x10   net_bytes=816   classes=36   unbalanced=1
  size=16  alloc=6652  free=6601  net_count=51
```

**`unbalanced=1`.** Across 2,200 spawns and 36 size classes, exactly one class fails to balance;
every other allocates and frees in precisely equal numbers. Three boots now read 51, 50, 51. The
original claim was right, the retraction was wrong, and both were made on the same kind of evidence
-- a small number read without asking what the instrument could see.

The same boot demonstrates the census reading a true zero rather than small-number noise:
`l3-thread-userspin` reads **`net_bytes=0 unbalanced=0`**, and `l3-thread` reads `unbalanced=1` --
the single in-flight 32-byte allocation that the floor section above predicts, now visible as a
count rather than inferred from a byte total.

**Lead in hand**: the one trap firing that did land in the workload named a 16-byte allocation at
`start_new_user` <- `sys_spawn`. Attribution still wants a trap boot with the stride tuned so
firings land inside the op, run *separately* from the census for the observer-effect reason above.

**One instrument miss, recorded because it cost a boot's worth of opportunity.** The
`--kalloc-trap=16:16:1:8` firing meant to name the 16-byte residual fired eight times *in
`kernel_main` at boot* and was exhausted before the workload started. `every=1, max=8` means "the
first eight allocations of this size", and the first eight of any common size are always boot-time.
It wants a large stride (`every=5000`) so firings land in the workload, or an arm-after-N parameter.
The trap symbolized correctly and the backtraces were clean -- the instrument worked, it was aimed
badly. **The 16-byte residual remains unattributed.**

### Where this leaves the residual, and a diagnostic that must not be used again as written

**Status of the 16-byte residual: reproduced, isolated, unattributed.** Three boots read
`net_count` 51, 50, 51, and with the reporter fixed the strongest form is `reap16-repro`'s
**`unbalanced=1`** -- across 2,200 spawns and 36 size classes, exactly one class fails to balance.
The one lead is a 16-byte allocation at `start_new_user` <- `sys_spawn`, from the single trap firing
that ever landed inside a workload. Attribution needs a trap boot; two attempts have not produced
one.

**The trap is unsafe by construction and must be rewritten before it is used again.**
`maybe_trap` calls `panic::backtrace(true, ..)` from inside `GlobalAlloc::alloc`: symbolization
parses DWARF, which **allocates**, re-entering the allocation path and `GLOBAL_PAGE_ALLOC`, and it
writes through the console lock -- all with whatever locks the caller happens to hold. `TRAP_BUSY`
guards the trap against re-entering *itself*, which is the second-order hazard; nothing guards the
first-order one. The stride-2000 boot surviving was luck about where its two firings landed, not
safety. **Fix**: record the caller address (and let the count of distinct addresses be the finding),
print outside the alloc path. That also makes the trap usable at stride 1 instead of only where
firings happen to be safe. Credit: twizzler-ea reached the same conclusion independently and stated
the general form -- a trap that allocates from inside the allocation path is a self-deadlock shape.

**Two boots hung, and it was not the trap.** `reap16-trap2` died at "guest went silent", exit 36,
3m11s, with a `spinlock long pause` on `GLOBAL_PAGE_ALLOC` immediately after a trap firing -- which
looked conclusive and was not. The single-variable discriminator (`reap16-notrap`: byte-identical,
trap flag removed, **zero firings**) failed **identically**: same stall point during *secondary cpu
enumeration*, same lock, same exit code, same 3m11s to the second. Deterministic across two builds.

Bracketed: `reap16-repro` **passed** at 06:11 on the tree before another session's frame-pool
changes; both failures are after them, and the only other change in the window is this harness's
`report_kalloc` -- userspace, in a binary not yet loaded at that point in boot.

**Resolved by the owner, from that bracket and without another boot** (twizzler-ea): a `Vec::reserve`
added to `FrameAllocator::drop`. `allocate_chunk` takes `GLOBAL_PAGE_ALLOC` and calls
`GlobalPageAlloc::extend` *while holding the guard*; `extend` builds a `FrameAllocator`, which drops,
which reserves, which allocates, which re-enters `allocate_chunk` and takes the same non-reentrant
spinlock. Self-deadlock at the first heap extension. Their `poolab2-off` arm passing 3/3 pins it: the
reserve is gated on `FA_FREE_TO_POOL`, false in that arm and true in every hang. The fix moves the
reserve into `precharge()`, which already allocated there.

**Not the lock deletion, which I had named as the delta.** The bracket was right and the attribution
inside it was wrong -- the window contained more than one change of theirs, and I picked the one I
had been told about.

**My hypothesis was the right shape and wrong in the specific**, which is the lesson worth keeping:
I reasoned "hang during AP enumeration => something about `%fs` not yet installed", and the real link
is that AP TLS allocation is simply *the first allocation big enough to extend the heap*. Their
statement of it: **"hangs at phase X" points at what phase X does, not only at what phase X is.**

**Two method notes from that sequence, both of which cost something.**

- **The convincing-looking line was the wrong one.** A trap firing immediately followed by a
  spinlock stall is a complete-looking story, and it was wrong. The discriminator cost one boot and
  turned a plausible accusation into a fact -- and the fact went the other way. `spinlock long pause`
  additionally reports "held by" as the *last acquirer*, never cleared on release, so that half is
  never evidence.
- **A failure this early exonerates both mechanisms we had weighted.** Nothing has churned a frame
  at secondary-cpu enumeration, so it is neither the trap nor the load-dependent
  allocate-under-`with_disabled` hazard -- a third mechanism, which is what two people converging
  confidently on two candidates looks like when both are wrong.

**Nothing here touches the reaper result.** The fix and its 10-boot validation were built and run
before that kernel change landed; the counters, slopes and the default flip stand on those builds.

### The null path certified zero, with the detection threshold measured (leak28-cert, 2026-08-19)

leakplan §7's threshold sweep, finally run: N=1000 (5x horizon), with two micro positive controls
leaking exactly 64 and 16 touched bytes per iteration alongside the null and p1.

| arm | verdict |
|---|---|
| p2-microleak-64 | **detected** — 0.0200 pages/iter, r2 0.913, all gates (nominal 0.0156 + slab metadata) |
| p3-microleak-16 | not detected — its ~4-page staircase was swamped by a single 20-page arena event, correctly gate-rejected |
| p1-leak-object | 1.0000/iter exactly, r2 1.000, over 950 tail samples |
| **l0-null** | **no slope on any of 31 counters — no near-misses — over 950 tail samples** |

**Certificate: the null path leaks nothing measurable, in a boot where 64 B/iter was demonstrably
detectable; the detection threshold lies between 16 and 64 B/iter.** Zero page-steps in 950
samples bounds any steady page-forcing leak below ~5 B/iter; the caveat is sub-page slack (a slow
leak can pack into partially-used shard pages before forcing a fresh one — p2's step cadence shows
that slack is roughly a page per size class). Every negative verdict this harness issues is now
bounded by a measured threshold instead of an implied one.

Run note for the record: ~5 minutes of another session's parallel build+boot load overlapped this
run (agreed in advance) — all verdicts here are per-iteration frame counts, which host load cannot
move, and the time-driven-background channel was independently measured at zero (leak27
l0-slow500).

## The 16-byte residual, attributed (2026-08-20)

The last open item in this file is closed. `net_count = 51` in the 16-byte class under thread churn
is **`SlotMgr`'s lazily-allocated per-slot state cell**, and it is correctly bounded and correctly
freed. Nothing in the kernel needs fixing; what needed fixing was the statistic that called it a
leak, and that fix is in `--track` (below).

### The instrument: a live-block attributor, not a caller sampler

`--kalloc-trap` was withdrawn as unsafe by construction -- it called `panic::backtrace(true, ..)`
from inside `GlobalAlloc::alloc`, which allocates (DWARF), takes the console lock, and re-enters the
heap. But rewriting it to record caller addresses, as the previous section proposed, would still
have been the wrong instrument. **A caller histogram cannot isolate 51 blocks out of 6,652
allocations, because 6,601 of those callers also freed.** Only the live set can.

`--track lo:hi` ([kalloc_track.rs](src/kernel/src/memory/kalloc_track.rs)) arms a static,
allocation-free hash table over one size range: every allocation in range is entered with its
frame-pointer-walked return-address chain, every free of that size removes it, and the survivors at
a quiesced dump are the residual with provenance. Frame pointers are exact here rather than
heuristic -- the kernel is built `-Cforce-frame-pointers=yes`. Addresses are recorded raw and
symbolized on the host ([tools/tracksym.py](tools/tracksym.py)), so nothing symbolizes, prints, or
allocates inside the allocation path. Its one lock is private to the module, held only over pointer
bookkeeping, and taken with interrupts off -- otherwise an interrupt that allocates on the same cpu
re-enters it, which is the same self-deadlock shape as the `GLOBAL_PAGE_ALLOC` hang, one cpu instead
of two.

Armed and dumped through `sys_kalloc_track` with no kernel command-line flag, so the window is
exactly one leakcheck op and an unarmed boot is genuinely unarmed.

> **Not measurement-safe.** It takes a lock on every in-range kernel alloc and free. It answers
> *which call site*, never *how much*, and no bench or slope from a `--track` boot should be
> compared with anything.

**`live` is not the residual, and the accounting says so exactly.** With A allocations inside the
window, F_in frees of those, and F_pre frees of blocks that predate it: `live = A - F_in` while the
census's `net_count = A - F_in - F_pre`, so **`live = net_count + free_miss` identically**. A
steady-state system rotates -- blocks allocated in the window legitimately replace older ones -- and
those show up in the live set as though retained. Reporting `live` with its call sites alone would
have returned a confident wrong answer decorated with backtraces, which is the most expensive kind:
a call site makes a number feel verified. Every slot therefore carries a sequence number, and the
identity is checked on every op. It has held on all of them, including the negative ones
(`l0-null`: `net_count = -20`, `free_miss = 21`, `live = 1`).

### Control first (`track-ctl`)

`p1-leak-object` leaks one object per iteration and costs exactly one 512-class allocation each.
Armed over 512..527:

```
census   p1-leak-object size=512 alloc=737 free=517 net_count=220
tracker  live=220 inserted=737 removed=517 overflow=0 free_miss=0   sites=1
site     count=220 oldest=0 newest=726   -> Arc<twizzler_kernel::obj::Object>::new
l0-null  live=0 sites=0
```

Allocation-for-allocation agreement with the census, one site, and its live blocks span the whole
window. **The control is powered rather than merely armed** because the census prints that class
independently in the same boot: if it had not read ~220, the path was not exercised and any zero
from the tracker would have been an unpowered zero. (Credit: twizzler-6e, who nearly filed a
`DIRTY-PT = 0` tripwire as a clean validation from a 13-second boot that had almost nothing for it
to catch.)

### The finding (`track16`)

| op | live | inserted | reading |
|---|---|---|---|
| l0-null | 1 | 1 | one `ThreadSleepLinker::reserve` high-water growth |
| l0-slow500 | 0 | 0 | **the wall-clock channel is dead** -- 110 s of elapsed time, nothing retained |
| l3-thread (220 spawns) | 0 | 660 | 3 in-range allocations per spawn, all freed |
| **l3-thread-x10 (2200)** | **51** | 6651 | the residual, reproduced a third time |
| l3-x10-spaced (2200) | 0 | 6600 | same spawn count, ran after x10 |
| l3-x10-idle (2200) | 0 | 6600 | same |

One site, `count=51`:

```
sys_object_map -> map_object_into_context -> SlotMgr::begin_insert -> SlotMgr::populate
    Box::into_raw(Box::new(SlotState::Empty))
```

`SlotState` is `Empty | Installing | Present(Arc<MapRegion>) | Removing`: an 8-byte discriminant
plus an `Arc`, **exactly 16 bytes** -- matching `816 / 51 = 16.000` read off the old census line
before the boot. It is installed on first use of a slot index, held for the life of the `SlotMgr`,
and freed in `SlotMgr::drop`. The struct's own doc already said so: *"a boxed `SlotState` per
touched slot... never torn down short of `Drop`"*. Worst case per context is `MAX_SLOTS` = 262,144
cells = 4 MiB, plus 2 MiB of level tables, all reclaimed at context teardown.

The `l0-null` site is the same shape: `sys_thread_sync` -> `ThreadSleepLinker::reserve` ->
`Box::new(new_links)` where `new_links: Box<[SleepLinkNode]>` is a fat pointer, again exactly 16
bytes, again guarded by `if self.len() < count`. **Every block the instrument found retained across
six ops is a one-time high-water growth.**

### Why a delta could not see it, and what actually separates the two

The tell was age, not the call site: the 51 blocks read `oldest=33 newest=233` out of **6651**
allocations -- a burst in the first 3% of the window, not a per-spawn rate.

[The statistic](#the-statistic) argues at length that a before/after delta cannot separate a
one-time cache fill from a per-iteration leak, and gates the *slope* series three ways because of
it. The kalloc census is a raw delta between two quiesced states and has **no** such gate. That is
the entire reason this reached the Open list as an unexplained leak: the harness applied its own
best reasoning to one statistic and not to the other sitting next to it.

**A within-window statistic was drafted to fix that, and rejected after being tested against its own
cases.** A rule on the age spread labels a tight fill correctly (`33..233` of 6651, spanning 3%) and
mislabels **the same mechanism** when it converges slowly (`33..3887` of 6642, spanning 58% -- the
same op on a different boot). No single window separates a converging fill from steady retention.

**Two windows do.** `--track` now runs each op twice by default (`--track-passes N`) and labels the
passes: a per-iteration leak must retain at the same rate every time, a fill cannot.

### Retest (`track16-fix`): the discriminator, with a control on *both* sides

A repeat-based discriminator has an obvious failure mode -- one that always shows a drop would
"confirm" the fill hypothesis whatever the truth -- so the retest pairs the fill with a workload
that **must not** drop. `l1c-map-unmap-slots` walks slot `400+i` from a counter that keeps climbing
across passes, so every iteration of every pass touches a slot never touched before: genuine
accrual, one retained cell per iteration, by construction.

| arm | pass 1 | pass 2 | required |
|---|---|---|---|
| `l1c-map-unmap-slots` | **221** | **220** | must repeat -- genuine accrual |
| `l3-thread-x10` | **42** | **10** | must drop -- one-time fill |

Both hold. The census confirms each window independently (`net_count` 200/220 and 41/10), and
**`live = net_count + free_miss` holds on all four**: 200+21, 220+0, 41+1, 10+0. `track16-rpt`
is an independent replication in a different boot: 42 then 9.

The 1 extra on `l1c#1` and on `l3-thread-x10#1` is the `ThreadSleepLinker::reserve` cell, appearing
once per boot in whichever window first grows a thread's sleep-link reserve.

### Predictions, including the one that was wrong

Written before the boots, in `scratchpad/predictions.md`:

1. `l1c-map-unmap-slots` walks a fresh slot every iteration, so if the mechanism is one cell per
   newly-touched slot it must read **live = 220 at 220 iterations, exactly**. Measured: `count=220`
   at that site, plus the single `ThreadSleepLinker` cell. **Hit exactly** -- an arithmetic
   prediction a wrong mechanism fails rather than a direction it can accidentally satisfy.
2. `l3-thread-x10` repeated in the same boot would read ~51 then **0**. Measured **42 then 9**
   (`track16-rpt`) and **42 then 10** (`track16-fix`).
   The direction is decisive -- a per-spawn leak cannot drop 78% on an identical repeat -- but
   **"then 0" was wrong**: the fill converges with a tail rather than in one shot, because new slot
   indices keep appearing occasionally as the runtime's slot pool turns over. The prediction being
   wrong in its specific is what forced the `--track-passes` design over the age heuristic.
3. `l0-slow500` would read ~0, killing the wall-clock hypothesis. Measured 0.

### It saturates: four passes, and a prediction of mine that failed (`track16-records`, 2026-08-20)

The account above says "one-time fill", which is unfalsifiable after the fact -- it accommodates any
repeat count. The sharper claim, read off the runtime's slot allocator, is that the cell count is a
**running maximum**: `alloc_single` pops a LIFO stack of freed slots and only falls through to
`alloc_pair`, which scans the pair bitmap **from index 0**, so a cell is allocated *only when a slot
index is touched for the first time ever* -- i.e. only when concurrent slot usage sets a new
all-time record. Four passes each, with the **op order deliberately reversed** from `track16` so the
order confound is flipped:

| pass | 1 | 2 | 3 | 4 |
|---|---|---|---|---|
| `l3-x10-spaced` (ran first) | **42** | 0 | 0 | 0 |
| `l3-thread-x10` (ran second) | **9** | 0 | 0 | 0 |

**It saturates to exactly zero and stays there**, and every increment lands as a tight burst at the
*start* of a pass (`oldest=3 newest=193` of 6642; `oldest=159 newest=191` of 6609) -- where the
pipeline refills after the 4 s quiesce has drained the deferred releases. That is a positional
prediction, much harder to satisfy by accident than a count.

**Two of the three registered predictions failed, and they killed an explanation I had already
published.** I predicted `l3-x10-spaced#1` would be *small* (0-15) and `l3-thread-x10#1` would stay
*large* (30-60) even after 2,200 spaced spawns, on the theory that spawn **density** sets the record
via `outstanding ~ spawn rate x IDLE_TTL`. Measured: 42 and 9 -- the reverse.

So **density is not what distinguished the arms; order is.** `track16` showed `l3-thread-x10 = 51`
and `l3-x10-spaced = 0`, and I read that as rate beating count. With the order reversed, spaced pays
the full fill and x10 gets the remainder. Whichever 2,200-spawn op runs first pays; the rest pay
nothing. Both ops set essentially the same record, which a density account says they should not.
The 51-vs-0 was **the order confound this boot was built to test, and it turned out to be the whole
effect** for that arm.

What survives, and is now better evidenced than before: the mechanism (a per-first-touched-slot
cell), its saturation to zero, its positional signature, and the conclusion that **it is not a
leak**. What does not survive is my account of *why* one arm differed from another. `l3-thread`'s 0
at 220 spawns is still unexplained by anything measured here -- it differs from the others in count
*and* rate, and no boot has separated those two for that op.

### A counter is not automatically contention-proof

Worth stating here because `--track`'s output is all counts and looks immune. **`live` is not.**
A cell is allocated only when concurrent slot usage sets a new all-time record, and concurrency is
`spawn rate x IDLE_TTL` (2 s, [handlecache.rs](src/rt/reference/src/runtime/object/handlecache.rs)).
A guest stealing cycles lowers the spawn rate, lowers the high-water mark, and lowers the count --
with **no timing number anywhere in the output** to reveal it. The bias runs the wrong way:
contention during a later pass makes the decay look cleaner, i.e. toward confirming the hypothesis
under test. A contaminated run reads as a *better* result, which neither inspection nor a green
summary can catch.

> A counter survives contention only if the quantity it counts is **extensive in work**, not in
> rate. Ask what it would read if the same work ran twice as slowly. A per-call fetch count reads
> the same; `live` does not.

So every `--track` boot must hold the box. All the numbers above were taken that way. (The
complementary trap, from twizzler-6e: in a *time-budgeted* bench, raw counts are not comparable
across arms at all, because a faster arm does more iterations -- normalize by a work term before
comparing, including the counters you are arguing *from* and not only the one you are arguing
about. leakcheck runs a fixed iteration count, so it is exposed to the first trap and not the
second.)

### Two corrections to this file

- **The standing lead was wrong.** This file named a 16-byte allocation at `start_new_user <-
  sys_spawn`, from the single stride-2000 trap firing that ever landed inside a workload. Real
  allocation, different one -- and not even on the spawn path: the residual comes in through
  `sys_object_map`. A sampled caller is not a retained block, which is exactly why the instrument
  had to track pointers instead of sampling callers.
- **`--kalloc-trap` should be deleted, not rewritten** -- and is now *interlocked*, 2026-08-20.
  The previous section proposed fixing it to record caller addresses. Nothing needs it now, and
  leaving a `panic::backtrace` call inside `GlobalAlloc::alloc` in the tree is a hazard whether or
  not anyone means to use it. `set_trap` now logs why it is disabled and returns; the original body
  is preserved verbatim as `set_trap_inner` under `#[allow(dead_code)]` rather than deleted, because
  it is another session's uncommitted work and the argument parsing is not the broken part. This
  file had said "must not be used again" since the withdrawal, which is the point: **a comment
  saying so is not an interlock**, and the next person to reach for it is someone chasing a leak at
  3am. Smoke-tested by a boot that deliberately *passes* `--kalloc-trap=16:16:1:8` -- a boot that
  never calls the changed function would not test it.

### Provenance of the hooks in `allocator.rs`, because git will not say

The two `kalloc_census::record_alloc/record_free` hooks and all of
[kalloc_census.rs](src/kernel/src/memory/kalloc_census.rs) are twizzler-2b's uncommitted work from a
session that has ended. The `kalloc_track` hooks sit in the same two hunks and are separate. Anyone
reverting the census takes the tracker with it.

## l7-spawn-proc unblocked: it was the call form, not the path (2026-08-20)

`l7-spawn-proc` had been recorded as blocked by a runtime bug of unknown cause, after five boots
across four hypotheses and eight different paths, all returning `NamingError::NotFound`. The
[Open entry](#open) concluded "the fd layer and the spawn resolver disagree about what an absolute
path means". **That is wrong.**

Full path x form matrix, emitted unconditionally (`l7matrix`):

| path | `Command::spawn()` | `Command::status()` | `Command::output()` |
|---|---|---|---|
| `/pkg/twizzler/bin/leakcheck` | ok | ok | **NotFound** |
| `leakcheck` | ok | ok | **NotFound** |
| `/initrd/leakcheck` | ok | ok | **NotFound** |

**It varies entirely by call form and not at all by path.** Every path resolves; two forms out of
three work on all of them. `NamingError::NotFound` (os error 262145) is a **misleading error
surfaced from the fd-binding path**, not from name resolution — which is what made the naming
hypothesis so durable. `spawn()` and `status()` inherit stdio; `output()` sets `Stdio::piped()` on
stdout and stderr and `Stdio::null()` on stdin, so it hands a different set of `binding_info` to
`exec_spawn` through `args.fd_binds`. (A second smell in the same area, not yet implicated:
`CompartmentLoader::new` calls `load_fd_specs_from_runtime()` and inherits the *parent's* binds,
which `exec_spawn` then overwrites with the child's.)

The op now tries `spawn` -> `status` -> `output` per candidate and **runs 20/20**, producing its
first verdict ever: `LEAKCHECK-CLEAN l7-spawn-proc`. Whole-process lifecycle is back in the
catalogue.

### The method failure, which is the reusable part

Eight paths produced one error **because the path was never the variable.** Every attempt held the
call form fixed at `output()` and varied the name, so the search was exhaustive along one axis and
untouched along the other — and exhaustiveness along the wrong axis feels like rigour from the
inside. "Eight paths, one error" reads as strong evidence about paths; it is actually evidence that
paths do not matter.

The disproof was already in this file. The Open entry itself says: *"`unittest` spawns children
successfully from its own compartment... the difference between those two contexts is the next
thing to look at."* `unittest` uses `spawn()`. The right next step was named, written down, and
then five boots went to the other axis. **A recorded next step is not a taken one**, and a
comparison sitting in your own notes is worth more than another sample along the axis you have
already saturated.

One more instance of the harness's recurring shape, this time in my own change: the first version
of this reported the matrix **only on failure**, so the passing run said "some cell works" and
could not distinguish "the call form was the bug" from "someone else's fix cured it". A diagnostic
that runs only when the thing fails cannot tell you why it stopped failing. Emitting the matrix
unconditionally is what made the table above possible.

### Root cause: `Stdio::null()` opens `/dev/null`, which Twizzler does not have

Varying one redirection at a time (`l7leak`), all three paths identical:

| form | result |
|---|---|
| `Spawn` (inherit all) | ok |
| `Status` (inherit all) | ok |
| `Output` (null stdin + piped stdout + piped stderr) | **NotFound** |
| **`NullStdin`** (null stdin only) | **NotFound** |
| `PipedOut` (piped stdout only) | ok |
| `PipedErr` (piped stderr only) | ok |

**Piped stdio is innocent; null stdin is the whole of it.** The cause is in libstd, not in the
Twizzler runtime — `library/std/src/sys/process/unix/common.rs`:

```rust
cfg_select! {
    target_os = "fuchsia" => { /* fuchsia doesn't have /dev/null */ }
    target_os = "vxworks" => { const DEV_NULL: &CStr = c"/null"; }
    _                     => { const DEV_NULL: &CStr = c"/dev/null"; }   // Twizzler lands here
}
...
Stdio::Null => {
    let fd = File::open_c(DEV_NULL, &opts)?;   // opens "/dev/null"
    Ok((ChildStdio::Owned(fd.into_inner()), None))
}
```

Twizzler has no `/dev/null`, so `Stdio::null()` fails to open it **in the parent, before any spawn
machinery runs**. `exec_spawn` is never reached and the program path is never consulted. Two fixes
have precedent in that same `cfg_select!`: give Twizzler a null device, or a Twizzler arm that skips
the open like Fuchsia's.

Every observation falls out of it at once:

| observation | why |
|---|---|
| eight paths, one error | the path is never looked at; the failure precedes it |
| `spawn()`/`status()` work | they inherit; `Stdio::Null` is never constructed |
| `PipedOut`/`PipedErr` work | pipes do not need `/dev/null` |
| `unittest` works | it uses `spawn()` |
| the error says `NotFound` | **it was always truthful** — it just never said *which* name |

**A third correction, and the most instructive one.** After the form/path matrix, the plan (mine,
endorsed by twizzler-6e) was to report that the error "names the wrong subsystem" and to lead the
bug report with the misattribution. That was also wrong. `NamingError::NotFound` was correct at
every step; what was missing was never a *subsystem*, it was a **noun** — the reader supplies
"the program" because that is the name they were thinking about. An error that omits its subject
does not misreport, it invites the reader to fill the subject in, and every reader fills it in the
same way. That is a distinct failure from a wrong error code and it wants a distinct fix: name the
thing that was not found.

## l7-spawn-proc leaks ~146 KB per process spawn (l7leak, 2026-08-20)

With the op unblocked, it produces a leak verdict on every gate — the first new leak this harness
has found since the thread leak. 220 iterations, 200-sample tail, release/KVM/smp4:

```
LEAKCHECK-LEAK l7-spawn-proc trk.page_data    slope=35.6249/iter net=13100 r2=0.927 duty=0.62 maxstep=0.18
LEAKCHECK-LEAK l7-spawn-proc trk.kernel_used  slope= 4.0149/iter net= 1345 r2=0.907 duty=0.79 maxstep=0.18
```

**35.6 frames/iter = ~146 KB retained per process spawn**, plus 4.0 frames of `kernel_used`.

### What it is not

Everything the monitor accounts for is flat across 220 spawns, which excludes an entire family of
explanations before any attribution work:

| counter | net over 220 spawns |
|---|---|
| `mon.compartments` | **0** — child compartments *are* reaped |
| `mon.threads` | 0 |
| `mon.libs`, `mon.lib_handles` | 0 |
| `mon.comp_handles` | +1 |
| `self.slots` | −5 |
| `obj.objects` | +5 |

So nothing leaks handles, compartments, threads, slots or objects. **Teardown works**; the pages go
somewhere teardown does not reach.

### Where the pages go

```
89e94ea8...  328 -> 5920  (+25.42/iter)  existing  note=heap
c53fb758...  915 -> 2835  (+ 8.73/iter)  existing  note=heap
800000...8e7   0 -> 1178  (+ 5.35/iter)  NEW       note=-        <- 0x8000 prefix: Persistent
207543be...    0 ->  361  (+ 1.64/iter)  NEW       note=heap
f32bda71... 1426 -> 1649  (+ 1.01/iter)  existing  note=monitor-heap
```

Two long-lived `heap` objects carry 34.1 of the 35.6 frames/iter. The monitor's own heap is a minor
contributor at 1.01/iter, which argues against monitor bookkeeping. Kernel side: `size=16` reads
`net_count=1857` over 220 spawns, ~8.4 unfreed 16-byte allocations per spawn.

The new **persistent** object taking 5.35 pages/iter is a separate thread worth pulling: something
writes ~4.8 MB to backing store across the op.

### Whose heap: the spawner's own (l7whose, 2026-08-20)

`note=heap` says "a heap", not *whose*, and two very different bugs fit the data identically — the
**spawner** retaining per-child state, or a **service** retaining per-lookup state (`naming-srv` was
the natural suspect: every spawn runs `find_id` -> `session.get(name)`). Since compartment teardown
demonstrably works, whose heap it is *is* the finding.

`dump_self_map` ([main.rs](src/bin/leakcheck/src/main.rs)) walks every slot with
`sys_object_read_map` and prints the object mapped there, so a grower either appears in leakcheck's
own address space or it does not.

| grower | pages/iter | in the spawner's address space? |
|---|---|---|
| slot 153, `note=heap` | **24.78** | **yes** |
| slot 151, `note=heap` | **8.73** | **yes** |
| slot 23, `note=monitor-heap` | 1.14 | yes (minor) |
| persistent `0x8000...08e7` | 5.36 | no — separate thread |

**84% of all page growth (36.25 of 43.24 pages/iter) lands in objects mapped into the spawning
process's own address space**, and the top two are its local heap. So the leak is
`Command::spawn()` + `wait()` retaining **~137 KB of the parent's userspace heap per child**, with
the monitor side provably clean. One process, one heap, no cross-compartment protocol — a much
smaller fix than a service-side retention.

Not yet attributed *within* that heap. The runtime has `trace_runtime_alloc`
([trace.rs](src/rt/reference/src/runtime/trace.rs)) for exactly this, and the same argument that
motivated `kalloc_track` applies: a per-size-class total will name the class and not the call site,
and a caller histogram cannot isolate the retained blocks from the many that are freed.

### A cross-boot join that returned a clean, decisive, wrong answer

The first version of this comparison checked the **previous** boot's grower ids against **this**
boot's self-map. Every grower came back `in_self_map=0`, which reads as "none of the growers are
ours" — the opposite conclusion, arrived at confidently and in one line.

**Object ids are regenerated per boot.** Only the persistent `0x8000...` object keeps its id across
boots, which is itself the tell: it was the one id that matched across two logs and it is the one
object whose lifetime is `Persistent`. A cross-boot join on a per-boot identifier returns zero for a
reason that has nothing to do with the question being asked.

Caught because zero-for-everything was too tidy, not because anything in the output said so. The
fix is the same one this file keeps arriving at from different directions: the comparison now runs
**within one log**, and it prints a positive control (three `LEAKCHECK-SELFMAP` lines) so that
"nothing matched" and "the matcher is broken" cannot serialize to the same result.

### The n=20 run got the number wrong, not just the verdict

An earlier 20-iteration run of this op reported **CLEAN**. That verdict came from `trk.page_data`
failing the `r2 >= 0.9` and `maxstep <= 0.34` gates on a 15-sample tail — the gates rejecting a
stepped signal, which is what they do to real stepped leaks and to background work alike. Worth
recording because the failure was not only "underpowered":

- verdict wrong: CLEAN where the truth is a leak passing all three gates at n=200;
- **magnitude wrong too**: that run's slope read 117 frames/iter and its `net/iters` read 320, and
  both were quoted. The answer is **35.6** — off by 3x and 9x respectively.

A short tail on a stepped signal does not merely widen the error bar, it moves the estimate. "The
verdict is underpowered but the magnitude is roughly right" is a natural thing to assume and it was
false here in both directions at once.

And in the same run `LEAKCHECK-KALLOC-TOTAL` read `classes=0`, which reads as "no kernel-heap
growth" and actually meant **the `--kalloc-census` kernel arg was not passed, so the census never
ran**. An instrument that was never enabled and an instrument that found nothing serialize to the
same string. Caught only because `classes=0` sat implausibly next to `page_data` moving 6402 frames.

## Three catalogue ops compile to nothing, and the write-up already explained why they were clean (2026-08-20)

`l2b-heap`, `l2c-heap-2mb` and `l2e-heap-small` **do not allocate**. They never have. Verified by
disassembling the built binary rather than by reading the source:

```
leakcheck::ops::l2b_run:
    push %rbp; mov %rsp,%rbp; sub $0x10,%rsp
    call __rust_no_alloc_shim_is_unstable_v2
    movq $0x10000,-0x8(%rbp)        # store the constant 65536
    lea -0x8(%rbp),%rax; ret
```

No `call __rust_alloc`. Same shape in `l2c_run` (`$0x200000`) and `l2e_run` (`$0x40`). The pattern
is `Vec::with_capacity(N)` followed by `black_box(v.capacity())`: `capacity()` is a compile-time
constant and nothing else touches the vector, so LLVM deletes the allocation. **`black_box` on the
capacity pins a value, not an allocation.** `l2d_run` is the only one of the four that is real -- it
writes a byte per page through `as_mut_ptr()` first, and its disassembly contains `__rust_alloc` and
the touch loop. (`l2f` calls `l2d_run`, so it is safe too. Checked by twizzler-92, who went looking
for a fourth site rather than taking the list on trust.)

### The durable part is not the dud ops, it is the explanation that was already here

This file **already contained an explanation** for those three reading clean:

> Untouched allocations are invisible to a page-counting harness by construction (`l2b`/`l2c`/`l2e`
> clean says nothing about their reuse).

That sentence is *true*. It is also not what was happening. It would explain the clean reading if
the allocation happened; there is no allocation. **A right explanation attached to the wrong
phenomenon is more durable than a wrong one, because every check confirms it** (llama-twz-1f's
phrasing). It survived weeks precisely because it kept being correct.

Nothing in the harness could have separated the two. A page count, `trk.allocated`, and the object
census all report the same zero whether an allocation happened and went untouched or never happened
at all. No improvement in the precision of a page-counting instrument would have found this. It
took an instrument counting a **different quantity** -- allocator calls -- to disagree. That is the
argument for keeping two instruments that measure different things over one good one.

### How it was caught: a predicted value, not a predicted sign

The new `LEAKCHECK-UHEAP` census was validated against a control predicted at **220 allocations,
220 frees, class `le=64`**. It reported zero. That is a contradiction; "some allocations" would have
been satisfied by the same zero if the prediction had only been "nonzero". **Without a predicted
value, zero allocations reads as a fact about the system.** The disassembly was the next step, and
it acquitted the instrument and convicted the op.

The pre-registered reading rules had a branch for "everything zero including the control = dead
instrument". They did not have a branch for "the control did not run", which is the one that
happened. A reading rule can be incomplete in the same way a control can.

### Contamination beyond the three readings (twizzler-92)

- **`l2c`'s stated rationale depends on `l2b` allocating.** Its comment argues that if 2 MiB churn
  leaks while 64 KiB does not, the boundary is the large-allocation path. `l2b` never allocated
  64 KiB, so that conclusion currently rests on a contrast with an empty arm. `l2d` still measures
  what it measures; what is gone is the comparison that made it mean *large versus small*.
- **`l2e` did not merely fail to check a sysbench claim -- it retired one.** The `heap_alloc_free`
  observation (a 64-byte `Vec` alloc/free loop taking 978,441 frames and returning 400 in one bench
  interval, ~4 GB) was **formally withdrawn in `sysbench.md`**, and `l2e` is the entire evidence for
  the withdrawal, verbatim:

  > **Withdrawn as a finding too**: a purpose-built leakcheck op (`l2e-heap-small`) reads *clean* at
  > slope 0.0925/iter against a null-control floor of 0.3533 -- i.e. **below the cost of doing
  > nothing**.

  "Below the cost of doing nothing" is in the record as evidence of allocator health. It is also the
  literal truth about an op that compiles to nothing. **The tell was inside the sentence that
  quoted it.** A named finding was retired on an empty control's say-so, and the retirement is
  quotable -- this is the concrete downstream casualty, not a general worry.

  Now **reopened** (twizzler-92, annotated rather than rewritten so the original reasoning stays
  legible). Honest net state: **neither confirmed nor refuted.** The withdrawal had two arguments
  and they fail differently -- the `PERFMARK-MEM` critique stands on its own (system-wide counter,
  one interval, no slope, no r², no quiesce), but that is a reason to distrust the *observation*,
  not evidence the phenomenon is absent. Both directions are now unsupported: a worse epistemic
  position than before, and an accurate one.

  **Unknown and deliberately left unknown:** whether `heap_alloc_free` was itself elided the same
  way. If it was, it allocated nothing and 978,441 frames cannot have come from it, which would
  support misattribution independently of `l2e`. `src/bin/sysbench` is untracked, so there is no
  history to recover. Not reasoned into.
- **sysbench is clean of this pattern *now*, which is not the same as never having had it.**
  Audited: every `black_box` in the current source wraps the result of a syscall or FFI call, never
  a compile-time constant derived from an allocation. But the one bench that had this shape was
  `heap_alloc_free`, and the current source is clean *because that bench was withdrawn* -- on the
  strength of the empty control above. Stated precisely because "we audited it and it's clean" is
  exactly the reassurance that stops the next person looking.

### Proposed, not done

`l2b`/`l2c`/`l2e` are **left exactly as they are**. Changing what a named op allocates would
invalidate every earlier reading of it *silently* -- the op keeps its name and keeps producing
numbers. The proposal is to add correctly-escaping twins under new names, which keeps the old
readings interpretable as "this op did nothing" instead of making them unfalsifiable. The two new
controls `l2ctl-48k`/`l2ctl-48b` are written that way: `v.push(..)` then `black_box(v.as_ptr())`,
so the pointer escapes and the allocation cannot be elided.

### `--diag`: a flag inherited for comparability, which cost the run

`l7parts1` failed at 5m22s, exit 34, three of thirteen ops, "never shut the guest down". The guest
was not hung: **261,756 of the log's 310,065 lines were the `--diag` object dump**, 84% of the
output. It was drowning in serial. `--diag` contributes nothing to this measurement -- leakcheck's
grower attribution comes from its own `sys_object_stat` calls, not from that dump -- and it was
passed only because the baseline being compared against had it.

Same root as the bad control, and worth stating as one rule rather than two: **a principle without a
selection rule gets satisfied by whatever is nearest to hand.** A note saying "`--diag` perturbs
measurements" does not fire when the task feels like "reproduce the baseline". The selection rule
that would have caught both: *what would this choice look like if it were wrong?* A control chosen
for availability and a flag inherited for comparability both answer "exactly like it does now".

And the general form, since inheriting a configuration is otherwise good practice: **copying a
configuration for comparability makes the whole configuration load-bearing, including the parts
that were incidental when someone else chose them.**

## l7-spawn-proc: it is not the spawner's heap, and it is not a retained allocation (l7parts3, 2026-08-20)

First run with a validated userspace allocation census. **Controls passed with predicted values in
predicted classes** -- `l2ctl-48b` 220 allocations in `le=64`, `l2ctl-48k` 220/220 in `le=65536`
with net 0 -- so the census counts *and* buckets correctly, and every zero below is a fact rather
than a possibility.

### What is ruled out

| hypothesis | reading | verdict |
|---|---|---|
| a discarded free (four `dealloc` early returns) | all `drop_*` = `0/0` in all 11 ops | **dead** |
| live-block retention in the spawner's heap | net **56 bytes over 220 spawns** | **dead** |
| ferroc asking talc for more memory | `base_alloc=3/134223336`, byte-identical in all 11 ops | **dead** |

The live-set figure needs its subtraction stated: `l7-spawn-proc` reports `net_bytes=16968`, of which
**16,912 is a single one-off block that every op reports identically, `l0-null` included**. That is
what makes the remainder attributable: 56 bytes / 220 spawns = **0.25 bytes per spawn**. Every
allocation the spawn path makes is freed, and ferroc never asked for more memory while doing it.

### Where it is

Decomposition, same boot, 220 iterations each:

| op | `trk.page_data` slope |
|---|---|
| `l7p-resolve` (name resolution) | **0.0000** |
| `l7p-command` (std `Command` construction) | **0.0000** |
| `l7p-fd` (fd open + close) | **0.0000** |
| `l7p-binds` (fd-binding reads, x2) | **0.0000** |
| `l7p-loader` (`CompartmentLoader` + monitor + child) | 65.2 |
| `l7-spawn-proc` (full) | 50.5 |
| `l7-spawn-proc-b` (full, repeated after the parts) | 46.3 |

**Everything above the loader is exactly zero.** The whole effect is `CompartmentLoader` + the
monitor round trip + the child compartment. The ordering control did its job: the full op reads
50.5 before the parts and 46.3 after, so `l7p-loader`'s 65.2 is not an artifact of running first.

### The mechanism this leaves, and how to kill it

Live set flat + no new base chunks + resident pages climbing 33.6/iter across the spawner's two
`note=heap` objects (24.91 + 8.73, both confirmed in leakcheck's own slot map *within this log*)
means the pages are being faulted into **address ranges ferroc had already claimed**. ~78 KB of
per-spawn churn spread over fresh addresses inside a 128 MiB claimed region, with
`hook_decommit=0` so nothing is ever given back: the resident footprint climbs toward the region's
high-water mark while the live set stays at zero.

**This is not a leak in the "forgot to free" sense, and the recorded framing -- "the spawner retains
~137 KB of the parent's heap per child" -- is wrong.** The spawner retains nothing. Its *resident
page footprint* grows because freed addresses are not reused and decommit never runs.

**Kill condition:** if the mechanism is address churn, page growth must plateau once the churn
revisits its own range. It does not plateau over 220 spawns. If a future run shows a plateau, this
account is wrong and retention is back on the table.

### Two cautions against this result

- **The same op scored both ways in one boot.** `l7-spawn-proc` = LEAK (r² 0.93), `l7-spawn-proc-b`
  = CLEAN (r² 0.74). Identical code, identical iteration count. The slopes agree (50.5 vs 46.3);
  the *gates* are marginal on this signal. **The slope is the finding; the verdict is not.**
- **The selfmap join cannot answer for new objects.** `fba50037` (the loader's top grower) is absent
  from the selfmap because it was *created* during the previous op, and the map is dumped once at
  startup. Absence there means "not present at dump time", never "not ours" -- a different failure
  from the per-boot-id join that caught 98, with the same shape.

### Every earlier leakcheck number was taken through a console flood

`get_object_stats()` -- the handler for the **stats syscall** -- called `print_all_objects()`
unconditionally, dumping every object in the system to the console on every call. leakcheck's
sampler calls it once per iteration, so a 220-iteration op emitted ~800 console lines per sample:
115,831 `ObjectInfo` lines and 17 MB of serial from *one* op. It timed out two runs.

Now behind `crate::is_diag_mode()`. Log: 17 MB at one op -> **442 KB for eleven**. This run's
`l7-spawn-proc` slope reads **50.5/iter against the recorded 35.6**, so the earlier baselines were
measured through the flood and are not comparable.

**Verification status**: `cargo check-all --kernel` was blocked by this session's permission
classifier. twizzler-ce ran a baseline check on the tree as left, before any edit of theirs: **exit
0, no errors, and none of the eight pre-existing unused-import warnings is in `obj/mod.rs`.** Two
bounds on that, theirs: it is a *check*, so no codegen or link coverage (the `make-image` build in
the run covers that side -- between the two, both, but neither alone is both); and `check-all`
skips `#[cfg(test)]`, so any reference to this hunk from a kernel test is still unverified until a
`build-all --kernel --tests`.

**Method note, since I got this wrong first:** my initial post-mortem blamed `--diag` because it was
**84% of the log lines**. That was true and was not the cause -- removing the largest contributor
left a second one that killed the run by itself. **A share-of-output figure identifies a
contributor, not a cause**, and 84% feels conclusive in a way that 84% is not. The check that would
have caught it: predict the log size after the fix, and notice when the prediction is 40x off.

## CORRECTION: l7-spawn-proc's leak is in naming-srv, not in the spawner (l7own1/2/3, 2026-08-20)

**The recorded conclusion is wrong, and so was my own first replacement for it.** The record said
"84% of page growth lands in objects mapped into the spawning process's own address space, the top
two being its local heap... one process, one heap, no cross-compartment protocol -- a much smaller
fix than a service-side retention." It **is** a service-side retention. It is `naming-srv`.

### The measurement that settles ownership

`note=heap` is written identically by every compartment's allocator, so a grower reading
`note=heap` names a *kind*, not an *owner*. The note now carries the owning security context, and
leakcheck resolves compartment names to sctxs through the monitor:

```
LEAKCHECK-COMP name=naming  sctx=cfebe1062cab7afcff1819a53e556155
LEAKCHECK-GROWER l7-spawn-proc 1c8078b7... 328->5840 (+5512, 25.05/iter) note=heap:ff1819a53e556155
LEAKCHECK-GROWER l7-spawn-proc 906639ee...  915->2835 (+1920,  8.73/iter) note=heap:ff1819a53e556155
```

Both big growers are `naming`. **~134 KB per spawn, in the naming service's heap.** For contrast,
in the same boot the spawner's *own* heap object grows **0.05 pages/iter** -- 12 pages across 220
spawns.

### Why the address-space join said the opposite

`dump_self_map` walks the spawner's slots and finds those objects there, which reads as "ours".
**Compartments share an address space**, so being mapped in leakcheck's slot table says nothing
about which compartment's allocator owns an object. The join answered a question
(*is it mapped here?*) that looks like the question being asked (*is it ours?*). Two further flaws
in the same join, both of which produce false negatives rather than false positives: the map is a
snapshot taken at startup, so an object created mid-run can never match; and object ids regenerate
per boot, which is the trap already recorded above.

The exact form: **`__twz_rt_diag_heap_objects` walks the caller's own `oom_handler.objects`**, which
is the definitive list -- `create_and_map` is only ever called from talc's OOM handler, so an id in
that list is the compartment's and an id absent from it is not. It reports `main=1` for leakcheck
throughout: the spawner never even claimed a second heap object.

### Retention, measured directly rather than inferred

`--utrack 1:2048` records every userspace block allocated during an op and not freed:

| op | inserted | removed | **live** | overflow | truncated |
|---|---|---|---|---|---|
| `l7-spawn-proc` | 16,357 | 16,356 | **1** | 0 | 0 |
| `l7p-loader` | 6,876 | 6,876 | **0** | 0 | 0 |
| `l7-spawn-proc-b` | 15,236 | 15,236 | **0** | 0 | 0 |

Zero overflow and zero truncation, so the table lost nothing. In the spawner, across 220 spawns,
**one block survives**. Not "net zero" -- individually tracked and individually freed.

### What killed my own "allocator churn" account

I concluded the growth was high-water address churn with the live set flat. Three readings refute it
and all three were already in the data:

1. **Growth exceeds churn.** 76 KB/iter allocated against 134 KB/iter of new pages. Size-class
   rounding cannot cover 1.8x.
2. **The repeat did not reuse the footprint.** High-water churn must decelerate when the churn
   revisits its own range; `l7-spawn-proc-b` grew *more* per iteration than `l7-spawn-proc`.
3. **It is a ratchet, not a plateau.** The ~25 pages/iter grower is a *different object in each op*
   -- each fills to ~24 MB, then the next op starts on a fresh one and the filled one is never
   reclaimed.

The lesson is narrower than "I was wrong": **the census proved the spawner's allocator innocent, and
I attributed the remaining growth to the only mechanism left that I had already instrumented.** A
process of elimination is only as wide as the set of candidates, and "another compartment entirely"
was not in mine.

### Where to look next

`l7p-resolve` -- name resolution alone, 220 times -- reads **exactly 0.0000**. So it is *not* the
spawner's `find_id` lookup. The growth appears only when a child compartment is actually started
(`l7-spawn-proc` and `l7p-loader` both, and nothing above the loader). That points at naming work
done by the **child's own runtime during startup**, not by the spawner: each child's runtime opens
a naming handle (`get_naming_handle` -> `dynamic_naming_factory`), and `naming-srv` allocates
per-handle state behind a `HandleMgr`. A child that dies without its handle being released would
retain exactly this way.

**Unverified.** The next measurement is `naming-srv`'s handle count across a spawn loop, not more
inference. The 8.73 pages/iter grower is bit-identical across every spawn op and every boot
(1920 pages, three separate boots), which is a *second* mechanism and should be attributed
separately -- the "two mechanisms" prediction registered before the run, now supported.

### Two attractive mechanisms for the naming leak, both measured and both dead (namehnd1/namecache1)

The obvious reading of "naming-srv's heap grows per spawn" is that dead children's handles are not
being reclaimed. `secgate::util::HandleMgr::gc_handles` is supposed to prevent exactly that:

```rust
fn sctx_still_valid(id: &ObjID) -> bool { sys_object_stat(*id).is_ok() }
self.handles.retain(|id, sv| !sv.is_empty() && sctx_still_valid(id));
```

**~~That predicate is measurably unreliable~~ -- WITHDRAWN, the measurement was wrong.** The probe
loaded a compartment, waited for `EXITED`, dropped the handle, stat'd the child's security context
immediately and again 50 ms later, and read 28 of 60 still `ok` at both points. That was written up
as "not racy, wrong at steady state".

**Thread reap can take up to two seconds.** A 50 ms window cannot distinguish "never reaped" from
"not reaped yet", so the probe was measuring the reap latency it had failed to wait for, and the
"still ok 50 ms later" figure -- which felt like the control that ruled out a race -- was 40x too
short to be one. The conclusion does not follow from the data and is retired pending re-measurement
against the settled state.

The probe now records every child's sctx as it goes and re-stats the whole set from
`sctxlive_report`, which runs after the op's post-quiesce (4 s by default, and far longer for every
child but the last). Immediate-alive with settled-gone is a healthy deferred reap; only
settled-alive is a defect.

**And it does not matter.** Instrumenting the service directly (`NAMING-HANDLES`, every 32nd
`open_handle`):

```
opens=32  live_total=22  ns_cached=7  ns_pinned=0  names=58  order=51
opens=96  live_total=12  ns_cached=7  ns_pinned=0  names=58  order=51
opens=224 live_total=24  ns_cached=7  ns_pinned=0  names=58  order=51
```

- **Handles do not accumulate.** `live_total` oscillates between 7 and 28 over 224 opens with no
  trend. Whatever the sctx predicate does, the tables are being reclaimed.
- **The namespace cache does not grow.** `ns_cached`, `names` and `order` are *bit-identical* on
  every line, and `ns_pinned` is 0 -- so `GlobalCache`'s "excess bounded by the number of client
  sessions" is not being exceeded, and no live session is pinning a namespace against eviction.

So the amplification story -- leaked handle pins a `NameSession` pins an `Arc<dyn Namespace>` pins
an unevictable `NsCache` -- is **wrong at every link**, and it was wrong while being entirely
consistent with the code it was read from. Both mechanisms are retired rather than left as
"probably".

**The finding that survives is the localisation, not a mechanism:** naming-srv's heap grows
~134 KB per compartment load (24.6-25.05 pages/iter in a rotating object plus a bit-identical
8.5-8.73 pages/iter in a second one), while the spawner's own heap grows 0.05 pages/iter, handle
count is flat, and the ext cache is flat.

**Still-open candidates, none measured:** per-thread state created in naming's compartment when a
new thread crosses a gate into it (a cross-compartment entry allocates in the callee), anything
`NameStore`/`NamespaceObject` allocates per compartment-load, or allocations made by naming's own
runtime rather than by naming's code. The next measurement is naming-srv's own `LEAKCHECK-UHEAP`
census -- the instrument exists and is per-compartment; it has simply only ever been armed in
leakcheck. **Arming it inside naming-srv names the size class directly and ends the guessing.**

The sctx-liveness result is **withdrawn, and then refuted** (sctxlive2). Re-measured against the
settled state -- every child's sctx recorded as the op runs, the whole set re-stat'd after the op's
4 s post-quiesce:

```
LEAKCHECK-SCTXLIVE-TOTAL n=60 alive_immediately=3 alive_after_quiesce=0
```

**Zero of 60 survive.** Security contexts of exited children are reclaimed; `HandleMgr::gc_handles`'s
predicate is sound, and the original "wrong at steady state" claim is not merely unsupported but
false. It was never load-bearing here anyway -- handle count is flat because clients close properly.

One number in that line should *not* be read: `alive_immediately` was 28/60 in the bad run and 3/60
here. That figure is the pre-reap race, it is sensitive to at least three things that differ between
the runs (`--census` present/absent, two uncommitted kernel changes by another session, ordinary
timing), and **it is not attributed**. The settled figure is the one the question turns on.

**Provenance, since this boot did not use the kernel the earlier runs did:** build
`c3c9b3fc01fa71ab`, source `6ab439832ec3`, containing twizzler-ce's uncommitted Fortuna reseed fix
and their `virtmem` secctx spinlock change. A settled-state reclamation count is insensitive to
fault-path locking as far as I can reason, but that is reasoning and not measurement, so the build
id is recorded rather than the assurance.

**The method failure is the reusable part, and it is one I had already written down.** This harness
quiesces for 4000 ms before and after every op *because reclamation in this system is deferred* --
that is the first paragraph of its own module doc. I then wrote a probe with a 50 ms delay and read
its output as steady state. Worse, the second sample point *felt* like the control that excludes a
race, so having two numbers instead of one increased my confidence while adding no coverage at all:
both fell inside the same unwaited window. **Two samples inside one latency are one sample.**

## The naming-srv leak is misrouted allocation, not a dropped free (namecensus1, 2026-08-20)

Arming the per-size-class census **inside naming-srv's own compartment** -- the instrument existed
all along and had only ever been armed in leakcheck -- names it in one boot. Over 224 handle opens
(~220 compartment loads), reported as deltas every 32 opens:

```
NAMING-HEAPCENSUS ferroc=229/25363 early_cold=0/0 early_nots=2/1048576
                  drop_earlyptr=0/0 drop_nulltls=0/0 drop_nots=0/0
NAMING-HEAPCENSUS-CLASS le=524288 allocs=2 net_count=2 net_bytes=1048576
```

Summed: **15 allocations of exactly 512 KiB, 7,864,320 bytes, none of them freed.**

### The mechanism

All four *discard* branches read **zero**, so nothing is being thrown away at `dealloc`. The leak is
on the **allocation** side: `twz-rt`'s `alloc` routes any request made while the calling thread's
`THREAD_STARTED` flag is clear to the **early bump allocator**, and `dealloc` then unconditionally
drops any free of a bump-allocator pointer (`is_ptr_early_alloc` -> `return`). A bump allocator
cannot free, so every such allocation is retained for the life of the compartment.

```rust
if !ts {
    // TODO: this leaks the stuff that is allocated in libc's TLS
    let r = LOCAL_ALLOCATOR.alloc_early(layout);
```

**The TODO is accurate and the cost is now measured: 512 KiB a time, ~15 times per 220 compartment
loads = 35.7 KB/spawn**, against a measured ~134 KB/spawn total. A large share, not the whole thing.

Threads reach naming-srv by *entering its compartment through a gate call*, and one that allocates
before the runtime marks it started takes this path. **The exposure is not naming's** -- any service
that accepts cross-compartment calls has it, and the size of its leak is whatever its callers cause
it to allocate in that window.

### Why every earlier attribution missed it

The three refuted hypotheses -- allocator churn, handle retention, namespace-cache growth -- all
searched for something *naming-srv's code* was keeping. Nothing in naming's code is keeping
anything: `HandleMgr` reclaims, the ext cache is flat, sessions close. The retention is in the
**runtime beneath it**, on allocations naming never sees the lifetime of, and it is invisible to
every service-level structure you could inspect. It took a counter *inside the runtime of that
compartment* -- which is why "arm the instrument in the compartment you suspect" was the step that
worked after three that didn't.

### Not yet attributed, and stated rather than glossed

~100 KB/spawn remains unexplained. The smaller classes show positive `net_count` per window, but a
free landing in a *later* window reads as negative there, so per-window rows cannot be read as
retention -- the honest figure is the sum across all windows, which this run does not compute.
**n=15 events, one boot.** Neither the rate nor the 512 KiB size should be quoted as settled from
this alone.

**Build provenance:** `5cd237e626f4d1b5`, source `4fd6b2187e46`, which includes twizzler-ce's two
uncommitted kernel changes and two new virtmem tests. Declared before booting. A within-boot delta
of one compartment's own allocations is insensitive to kernel fault-path locking as far as I can
reason -- recorded, not assumed away.

### Reproduced on a clean tree, and the 512 KiB path *is* the deterministic grower (namecensus3)

Re-run after a poisoned build was discarded. Build `a2e156041203bc45`, source `6dbcd7a139f6`,
verified-clean tree. Cumulative over 224 handle opens:

```
NAMING-HEAPCENSUS-TOTAL net_bytes=8101233 classes=17
  le=524288  net_count=15  net_bytes=7864320    <- 97.1% of all retention
  le=16384   net_count=8   net_bytes=71408
  le=512     net_count=161 net_bytes=61104
  ... 14 more classes, ~237 KB between them
```

**15 x 512 KiB, identical to the first run** -- same count, same byte total. Deterministic, and
97.1% of everything naming-srv retains.

**The arithmetic identifies the second mechanism exactly.** 7,864,320 bytes = **1920 pages**. The
smaller of the two growers in this leak has gained **bit-identically 1920 pages** in every spawn op
across three separate boots -- recorded in the pre-registration, before any measurement, as *"a
deterministic number and a noisy one in the same op is itself a claim: two mechanisms, not one"*,
with a kill condition attached. It is the same 15 x 512 KiB allocations. **Identified by exact
equality, not by a story that fits.**

**And it sharpens what is left.** The *larger* grower -- ~25 pages/iter, ~22 MB over 220 loads --
is **not** live-block retention: outside the 512 KiB class the census accounts for only 237 KB
total. So something touches pages in naming's heap without retaining allocations. Candidates not
yet distinguished: bump-allocator placement (the early talc claims fresh heap objects as it fills,
which would match the observed *rotating* grower -- each op fills one object to ~6000 pages and the
next op starts on a new one), or footprint growth in naming's ferroc heap, whose measured churn
(~1 KB/open) looks far too small to explain 100 KB/spawn. **Open. Not concluded.**

### Provenance note: one sweep was discarded, not salvaged

`namecensus2` was built from a tree another session had deliberately broken (a falsification arm
making `getrandom` re-serve the same bytes, hence duplicate object nonces). It hung the guest,
`exit 36`, and produced no census output at all. Discarded rather than interpreted -- with duplicate
nonces a hang is close to the most benign outcome available, but that was luck about that fault, not
a property of building from a poisoned tree.

**The shared root, which cost three sessions an error each this evening: a duration assumed rather
than observed.** "The break lives 90 seconds"; "the run takes 95 seconds" (mine -- I reported a
burst window to a peer that was 2m15s too short, while they were using it to discard contaminated
rounds); "this experiment fits the remaining window". All three were predictions with no error bar,
load-bearing for somebody else's correctness, in an environment that respects nobody's schedule.
**Replace the duration with an event** -- poll for the completion artifact, announce
poisoned/clean, stop on observed elapsed time. And when an estimate is someone else's input, round
it *outward*: a too-early end marks a contaminated interval clean, a too-late one only costs a good
round.

## Both mechanisms have one root: per-thread state created by a gate call is never released (namebase1)

The remaining ~100 KB/spawn is **ferroc base chunks that are never returned**. naming-srv, over 224
handle opens:

```
NAMING-BASECHUNK alloc=3/134223336    dealloc_bytes=0 hook_decommit=0 hook_dealloc=0
NAMING-BASECHUNK alloc=24/1075146176  dealloc_bytes=0 hook_decommit=0 hook_dealloc=0
```

**21 new chunks, ~940 MB claimed, zero bytes ever handed back**, at ~44.8 MB a chunk and roughly one
per 10 compartment loads. `hook_dealloc` and `hook_decommit` are 0 for the entire run: ferroc
returns nothing to talc, ever.

That is invisible to the class census by construction -- base chunks go straight to
`LOCAL_ALLOCATOR.alloc`, not through `GlobalAlloc` -- which is exactly why a compartment with a flat
live-block set (237 KB findable across the whole run) can grow its heap object by 22 MB.

**And it explains the "rotation" that had looked so strange.** talc's heap object is `MAX_SIZE`
(~1 GB). At 1.07 GB of chunks claimed, the first object is exhausted and talc creates a second --
which is precisely the `main=2` this run reports, and precisely the pattern where each op fills one
object and the next op starts on a new one. Nothing was rotating; one object was being consumed and
replaced.

### The common root

Both growers are per-thread state created in a **callee** compartment when a thread crosses a gate
into it, and never released when that thread goes away:

| grower | object | what accumulates |
|---|---|---|
| 1920 pages, bit-identical across 4 boots | naming's **early/bump** heap | 15 x 512 KiB allocations made while `THREAD_STARTED` was clear -- routed to an allocator that cannot free |
| ~22 MB, ~100 KB/spawn | naming's **main** heap | 21 ferroc base chunks, ~44.8 MB each, never returned |

This file already records the thread-local half of the story from the other direction: ferroc
releases a thread's heap through a `pthread_key_create` destructor, and *"until
`InternalThread::drop` called `__mlibc_handle_thread_exit`, nothing on the spawn path reached
mlibc's destructor machinery at all, and every spawn took a fresh slab it never gave back."* That
was fixed for threads a compartment **spawns**. There is no equivalent for threads that **enter**
a compartment through a gate call: such a thread never exits that compartment in a way that runs
its destructors, so its ferroc context and chunks are never retired.

**The exposure is therefore every service that accepts cross-compartment calls**, and it scales with
how many distinct threads ever call in -- not with what the service itself does. naming-srv is
simply the service this workload calls most.

**Measured**: chunk counts, byte totals, `hook_dealloc=0`, the second heap object. **Inferred, and
labelled as such**: that gate-entering threads are the reason new chunks keep being requested. The
test is a per-thread heap count inside a callee compartment across a spawn loop -- not run.

### The per-thread link, measured (namethr1)

The one inferred step was "chunks accumulate per thread entering through a gate". Its premise --
that distinct threads keep arriving -- is now measured, and it holds exactly:

```
opens=0    threads=1     chunks=3/134,223,336
opens=32   threads=33    chunks=10/268,782,416
opens=96   threads=97    chunks=15/537,570,336
opens=224  threads=225   chunks=24/1,075,146,176
```

**`threads == opens + 1` at every sample.** Every single handle open arrives on a thread naming-srv
has never seen before; not one is ever reused. Against that, base chunks climb monotonically at
**~1 chunk per 10.7 entering threads**, i.e. **~4.2 MB of chunk per thread that calls in and never
comes back**.

The kill condition registered before the run was a *bounded* thread count, which would have killed
the per-thread story and the rest of the chain with it. It came back 1:1 instead.

So the chain is now measured end to end except for the ferroc-internal step (that a new thread's
heap is what consumes the chunk), which remains structural inference from ferroc's `pthread`
configuration:

1. every compartment load sends a **new** thread into naming through a gate -- measured, 1:1;
2. ferroc gives each thread its own heap, and retires it through a `pthread_key_create` destructor;
3. a thread that *enters* a compartment never *exits* it, so that destructor never runs there --
   this is the mechanism, and it is the same one this file already records as fixed for threads a
   compartment **spawns** (`InternalThread::drop` -> `__mlibc_handle_thread_exit`);
4. so its heap is never retired and its chunks are never returned -- measured:
   `dealloc_bytes=0`, `hook_dealloc=0`, 21 chunks / ~940 MB claimed and none given back;
5. and separately, allocations it makes before `THREAD_STARTED` is set go to the bump allocator that
   cannot free -- measured: 15 x 512 KiB, exactly 1920 pages.

**Every service accepting cross-compartment calls has this**, scaling with the number of distinct
threads that ever call in. naming-srv is merely the service this workload calls most.
