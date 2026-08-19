# leakcheck: an experiment harness for finding real leaks — plan (2026-08-18)

**Status (2026-08-19): phases 1-4 are done and the first leak they found has shipped a fix.**
Results, the as-built notes and every finding are in [leakcheck.md](leakcheck.md); the plan below
is left as written, with the amendments recorded here rather than edited into the text.

| phase | state |
|---|---|
| 1 — the instrument | **done.** Decision gate passed: `l0-null` reads zero, `p1-leak-object` reads its known size. Both controls ship and run in every boot. |
| 2 — the cheap layers | **done.** L1a/L1b/L2a/L2b all clean; L2c/L2d/L2e/L2f added and L2d found the retention property. |
| 3 — the full stack | **done for L3**, which produced the leak that shipped a fix. L6 not built. **L7 is blocked by a runtime bug**, not by the harness: every spawn from inside a compartment fails `NamingError::NotFound` for paths that demonstrably exist. It self-reports `SKIP`. |
| 4 — census and attribution | **done, and it was the deliverable.** The census named the holder by object id, which is what separated the thread leak from an unrelated term that was being summed with it. Counters alone would have sent the fix to the wrong process. |
| 5 — regression gate | **not done.** The obvious hook now exists: the destructor probe and the per-object growers are stable enough to diff across revisions. |

Three amendments worth carrying into any future phase:

- **§2.2's monitor heap counter was never needed and never written.** The census answered the
  question it was for, by naming the growing object directly. The premise was also wrong: `MonAlloc`
  in `monitor/src/main.rs` records call-site IPs rather than bytes, and is not installed as the
  `#[global_allocator]` at all — the attribute is commented out.
- **§3's three gates needed a fourth idea, not a fourth gate.** `max_step_frac` was added after the
  first run because r2 does not reject a staircase. But the gates must be applied to **level**
  counters only: a cumulative counter rises at a constant rate for any op that does work, so it has
  near-perfect r2 and tiny `max_step_frac` *by construction*. Gating those flags every working op as
  a leak, which is exactly what my first verdict table did.
- **§4's quiesce budget held**, but the operational limit turned out to be elsewhere: the full
  catalogue needs `--heartbeat-tries` raised, and the failure presents as a timeout that
  `--timeout-scale` cannot fix. See leakcheck.md's "Running it".

Four static audits exist (`kleaks.md`, `mleaks.md`, `oleaks.md`, `pleaks.md`) and every one of
them opens with some form of "nothing here was run". They are a hypothesis list, not a ranking.
This plan builds the instrument that turns them into one, so we can attack the biggest offenders
by measured size rather than by how bad the code looked.

The whole difficulty is in one sentence, which is the user's warning and also what makes the
naive design useless: **most reclamation in this system is deferred, cached, or both.** A
before/after delta around a single operation measures caching, not leaking. Everything below is
organized around that.

---

## 1. Measurement surface that already exists

| Source | Call | Gives |
|---|---|---|
| Kernel memory | `sys_info(MemoryStats)` | `total_pages`, per-level `free_pages`, `late_kalloc_bytes`, `early_kalloc_bytes`, fault/TLB counters |
| Kernel objects | `sys_info(ObjectStats)` | `nr_objects`, `nr_mapped`, `nr_pending_delete`, `nr_handles`, `nr_ties` |
| Kernel threads | `sys_info(ThreadStats)` | `nr_threads`, `nr_running`, `nr_blocked`, `nr_pending_exit` |
| Kernel sctx | `sys_info(SctxStats)` | `nr_sctx`, `nr_active`, `nr_cached` |
| Object census | `sys_enumerate(Objects, &mut [ObjID], off)` | every live object ID — identities, not just a count |
| Per-object | `sys_object_stat(id)` | `maps`, `ties_to`, `ties_from`, `pages` |
| Provenance | `sys_object_enumerate_notes` / `get_note` | creator tags; already used (`b"heap"`, `b"monitor-heap"`, `ObjectBuilder`) |
| Caller's slots | `sys_enumerate_slots` | mapped slots in the caller's address space |
| Monitor | `monitor_api::stats()` | `space.mapped`/`active`, `nr_threads`, `nr_compartments`, `nr_comp_handles`, `nr_lib_handles`, `nr_libs`, `nr_comps` |
| Kernel internals | `sys_debug_perfmark` | tracker `idle`/`page`/`kern`/`reclaiming`, alloc/free counters — **serial log only** |

That is a lot more than I expected going in. `sys_enumerate(Objects)` plus `sys_object_stat` plus
notes is the important one: it means an object leak can be reported as *these 37 IDs, holding
1.2 GB, tagged `heap`* rather than as a number that goes up.

## 2. Gaps to close first (small, high leverage)

1. **Promote the tracker counters into `MemoryStats`.** `kernel_used`, `page_data`, `idle`,
   `pager_outstanding`, `allocated`, `freed`, `reclaimed` exist in `tracker.rs` and reach the
   outside world only through `PERFMARK-MEM` serial lines. An in-guest program cannot compute a
   verdict from a serial log. `tracker_snapshot()` already returns four of them; widen it and add
   the fields. This is the single change that makes leakcheck self-contained.
   - Why it matters beyond convenience: `free_pages` alone cannot distinguish "leaked into an
     object's pages" from "parked in a thread-local frame pool" (F4c parked 175k frames that way,
     charged as allocated and invisible). `kernel_used` vs `page_data` splits exactly that.
2. **Monitor heap bytes into `MonitorStats`.** `MonAlloc`/`Track` in `monitor/src/main.rs` already
   tracks allocated bytes and has `print_alloc_stats`. `mleaks.md`'s M1 (TLS regions never freed —
   `TlsRegion` has no `Drop`) is a pure heap leak and is invisible in every counter we currently
   ship. Add `heap_bytes`/`heap_live_allocs` to `MonitorStats`.
3. **Per-compartment slot count.** Slots are finite and non-recycled; `SpaceStats` is global. Not
   required for phase 1, but it is what localizes a slot leak to a culprit.

None of these change behavior; all three are counters.

## 3. The statistic: slope, not delta

Do **not** compute `after - before` around one operation. Three different populations produce a
positive delta and only one is a leak:

- **Steady-state cost** — the first iteration populates caches that then persist. Constant, not
  growing. Not a leak.
- **Deferred reclaim** — the drop happens k iterations or t seconds later. Bounded lag. Not a
  leak, but it makes any small-N delta positive.
- **Monotone growth** — per-iteration cost stays positive however long you run. This is the leak.

So the protocol is: run the operation N times, sample every counter after each iteration, discard
the first `W` (warmup), and **fit a line to the tail**. Report slope per iteration with a
confidence interval. A leak is a slope whose interval excludes zero and which survives quiesce.

This subsumes the user's "run `ls`, then run it again" idea and fixes its weakness: two runs
cannot tell a one-time cache fill from a per-run leak, because both look identical at N=2. N=40
with a tail fit can. Keep the two-run form only as the fast interactive smoke check.

Report per counter, per operation: `slope`, `r²`, `total tail growth`, `converged after quiesce?`.
A high slope with low r² is churn or noise, not a leak — say so rather than reporting the slope.

## 4. Quiesce protocol (grounded in the actual deferral mechanisms)

I dug these out of the tree; the cadences are what set the budget.

| Mechanism | Where | Cadence / trigger |
|---|---|---|
| Object reaping (`scan_deleted`) | `main.rs` idle loop | every 1000 idle iterations, **BSP only** |
| Exited-thread reclaim | `Processor::cleanup_exited` | every 100 idle iterations, and **pops exactly one thread per call** |
| Requeue drain | `main.rs` idle loop | every idle iteration |
| Reference-runtime handle cache | `handlecache.rs` | `IDLE_TTL = 2s`, and only enforced when someone touches the cache |
| Runtime heap / thread / object caches | `twz_rt_gc()` | on demand: `gc_threads` + `heap_gc` + `gc_object_cache` |
| Monitor deferred unmap | `mon/space/unmapper.rs` | background thread, on channel send |
| Monitor thread reaping | `mon/thread/cleaner.rs` | background thread, on thread-exit sync |
| Kernel object delete round trip | `pager::Deleter` (F7) | background kernel thread |
| Thread-local frame pool trim | `tracker.rs` (F4c) | 64 frames per allocator drop, cap 2048 |

Consequences that shape the harness:

- **`quiesce()` must poke, not sleep.** The handle cache's 2s TTL is enforced lazily; a sleeping
  process never triggers it. Call `twz_rt_gc()`, *then* wait.
- **An idle CPU is mandatory.** `scan_deleted` and `cleanup_exited` run only from the idle loop.
  sysbench.md already records that smp1 boots never idle while anything runs. **Run leakcheck at
  `-smp 4` by default.** A leak that appears only at smp1 is reaper starvation, not a leak — which
  makes the smp1 run a useful *contrast* arm, not a wasted one.
- **`cleanup_exited` pops one thread per call, at 1-per-100-idle-iterations.** Thread-heavy
  operations (L3, L7) will legitimately lag by a long way. This is `kleaks.md`'s finding #1 and we
  should expect the harness to reproduce it as slow convergence rather than as a leak.
- **`twz_rt_gc()` only quiesces the calling compartment.** It does nothing for the monitor, the
  pager, or a child's leftover state. Monitor-side numbers need their own settle time and cannot
  be forced.

So: `quiesce()` = `twz_rt_gc()`, then sample in a loop (≥2s total, poll ~250ms) until two
consecutive full samples are identical, or a cap is hit. **Record whether it converged.** A
non-converging quiesce is itself a finding — report it, never silently accept the last sample.

## 5. The harness

New crate `src/bin/leakcheck`, a plain binary (not a `#[bench]` crate).

Rationale over the sysbench shape: full control of sequencing and output; no libtest stdout
capture (sysbench.md's gotcha — in-guest audit lines had to go through `sys_kernel_console_write`);
and `many.py --autostart leakcheck` already exists as the runner. Keep it runnable from the shell
too, so interactive iteration doesn't need a rebuild+boot per idea.

```
Sample  := { mem: MemoryStats, obj: ObjectStats, thr: ThreadStats, sctx: SctxStats,
             mon: MonitorStats, slots: usize, census: Option<Vec<ObjID>> }

for each op in selected:
    quiesce();  base = sample();
    for i in 0..N:  op.run();  samples[i] = sample();
    quiesce();  final = sample();
    report(op, fit_tail(samples[W..]), final - base, converged);
```

CLI: `leakcheck [--ops a,b,c] [-n N] [--warmup W] [--census] [--json] [--quiesce-ms MS]`.
Emit a machine-readable line per (op, counter) so many.py can diff across runs and across
revisions — a leak that appears between two commits is the cheapest kind to fix, and only a
stable output format buys that.

## 6. Operation catalogue

Layered cheapest-first, so a leak localizes by *which layer first shows slope*. Each layer's
result is only interpretable against the layer below it reading clean.

| ID | Layer | Operation |
|---|---|---|
| L0 | control | `sys_thread_self_id()` in a loop — pure syscall, allocates nothing |
| L1a | kernel object | `sys_object_create` + `ObjectControlCmd::Delete` |
| L1b | kernel mapping | `sys_object_map` + `sys_object_unmap` of one long-lived object |
| L1c | kernel ties | create tied object, delete the tie target |
| L2a | runtime object | `Object::map` + drop handle |
| L2b | runtime heap | alloc/free a 64 KiB `Vec` |
| L2c | runtime fd | `open` + `close` of an existing object-backed file |
| L3 | threads | `std::thread::spawn` + `join` |
| L4a | naming | `put` + `get` + `remove` |
| L4b | naming | `mkns` + `remove` |
| L5a | file | open/read/close an existing file |
| L5b | file (pager) | create + write 1 page + fsync + unlink |
| L6 | compartment | `CompartmentLoader` load + drop, no thread started |
| L7 | process | `Command::new("ls").spawn()` + `wait` — the full stack, the user's example |
| P1 | **positive control** | deliberately leak one 4 KiB object per iteration |

Start with L0, L1a, L1b, L2a, L7. That set spans the whole stack and is enough to decide whether
the instrument works before building the other ten.

## 7. Controls are not optional

Two arms decide whether any result means anything, and both must ship in phase 1:

- **L0 must read zero slope on every counter.** If the null probe shows growth, the instrument is
  broken (or leakcheck itself leaks, which is the same problem) and every other number is noise.
  L0's residual is the floor to subtract from every other op.
- **P1 must be detected, at its known size.** A harness that reports "no leaks" without ever
  having demonstrated it can *see* one is an instrument that answers the same way regardless of
  the truth. P1 leaks exactly one object and one page per iteration; the report must say so, in
  those units. Sweep P1's size down (1 page, 1 object, 64 bytes of monitor heap) to establish the
  **detection threshold per counter** — that number belongs in the writeup, because it bounds
  every negative result the harness will ever produce.

## 8. Attribution: the object census

Counters say *how much*. The census says *what*, which is what makes a finding fixable.

At `--census`: snapshot `sys_enumerate(Objects)` before and after; for each ID present only in the
after-set, `sys_object_stat` it (pages, maps, ties) and read its notes. Sort by pages descending —
that is literally the "biggest offenders" list.

Report **`nr_objects` growth against `nr_pending_delete` growth**, because the two diagnose
opposite bugs:

- objects up, pending_delete **flat** → nobody ever asked for deletion. A userspace bug, and
  exactly `oleaks.md`'s thesis (dropping a handle deletes nothing; the default constructors never
  set `DELETE` or a tie).
- objects up, pending_delete **also up** → deletion was requested and reaping is stuck. A kernel
  bug — reaper starvation, a lingering map count, or a cached handle holding the map count up
  (the handle cache's own doc comment warns about precisely this).

That split turns one number into a routing decision, and it costs nothing to compute.

## 9. Running it

- Interactive: `cargo start-qemu --autostart leakcheck --qemu-options=--nographic`
- Sweep: `many.py --autostart leakcheck --kernel-arg=--diag -r 3`
  (`--diag` is required — sysbench.md records that `--benches`/autostart boots do not set kernel
  TEST_MODE, so the idle diagnostics are off by default.)
- Default `-smp 4`. Add an smp1 arm as the reaper-starvation contrast.
- Both debug and release: a leak whose slope differs between profiles is usually a cache sized off
  a debug constant, not a leak.

Per the sweep discipline already established: never a bare `start-qemu` when other sessions share
the tree, and do not edit the tree while a sweep is building.

## 10. Phasing

**Phase 1 — the instrument.** Widen `MemoryStats` with the tracker counters (§2.1). Build
leakcheck with L0, P1, and the sampling/quiesce/tail-fit machinery. Ship nothing else. **Decision
point: does L0 read zero and P1 read its known size?** If not, fix the instrument; every later
phase is worthless until this passes.

**Phase 2 — the cheap layers.** L1a/L1b/L2a/L2b. These exercise paths `pleaks.md` and `oleaks.md`
make specific claims about, at the layer where a positive result is easiest to chase.

**Phase 3 — the full stack.** L7 (`ls`), then L6/L3. Add the monitor heap counter (§2.2) before
L6/L7, because `mleaks.md`'s M1 predicts a per-thread heap leak that no current counter can see —
running L3 without it would produce a confident false negative.

**Phase 4 — census and attribution.** `--census`, page-weighted ranking, notes-based provenance.
Rank the audits' findings by measured size. This is the deliverable the user actually asked for.

**Phase 5 — regression gate.** Once the numbers are stable, wire the report into many.py so a
slope regression is caught by the sweep rather than by an audit six months later.

## 11. Confounders to design against

- **leakcheck leaks too.** Its own allocations are in every number. L0 measures exactly that floor;
  subtract it, and treat a rising floor as a bug in the harness.
- **Objects are never reclaimed on handle drop.** A "leak" at L2a may be correct-by-design and
  wrong-by-intent. The pending_delete split (§8) is what tells them apart.
- **The 2s handle-cache TTL sets a floor on quiesce.** Anything shorter reports cached handles as
  leaked. This is the most likely source of early false positives.
- **A child process's teardown is asynchronous on both sides** (monitor `ThreadCleaner`, kernel
  `cleanup_exited` at one thread per call). L7 will need the longest quiesce; if it never
  converges, that non-convergence is the finding and it is `kleaks.md` #1.
- **The pager holds state we cannot poke.** L5b's numbers will settle on the pager's own schedule.
  Cross-check against the pager-srv watchdog, which sysbench.md rates the best wedge instrument in
  the system — and remember its *silence* is evidence too.
- **Free-page counts are not leak counts.** Frames parked in the thread-local pool are neither
  free nor leaked (F4c). Read `kernel_used`/`page_data` alongside, which is why §2.1 leads.
- **Deferred work is not free work.** A quiesce that converges only after 30s has found something,
  even though the final delta is zero. Report time-to-converge as a first-class number, not as a
  detail of how the measurement was taken.
