# elephance

Benchmark harness for two claims about Twizzler as a substrate for HPC-adjacent data
(materials database / simulation-input shapes):

1. **Hydration crossover.** Time-to-first-query over a persistent dataset is
   O(pages touched) when the persistent representation *is* the in-memory
   representation (map a `PersistentHashMap` object), versus O(dataset) for any format
   that deserializes on open. The result is the dataset size where the lines cross.
2. **N-reader sharing.** N processes mapping the same dataset take one page fault
   system-wide per page and O(dataset) page-table memory (object page tables are
   linked into each context via the `OBJECT_TABLE` entry bit, so residency is shared);
   Linux takes N minor faults per page and O(N x dataset) PTE memory even when mmap
   shares the physical pages. The result is faults-vs-N and PageTables-vs-N: flat
   versus linear.

## Layout

- `core/` — `elephance-core`: the dataset and workload as pure functions of a seed.
  Every arm generates identical data from `(key_at, props_for)`, and every query is
  verified by recomputation, so there is no oracle file and a broken arm fails loudly
  instead of returning fast garbage. Zero deps; `cargo test` runs on the host.
- `twz/` — `elephance` (Twizzler binary): `build` (sharded `PersistentHashMap`
  objects, registered with the naming service, synced to disk), `query` (map +
  verified point queries with elapsed-time checkpoints and global kernel stat deltas),
  `launch` (spawn N concurrent readers, experiment 2), `stats`.
- `linux/` — `elephance-linux` (host binary, standalone crate): `build-flat` /
  `query-flat` (packed record file hydrated into `std::HashMap` — the weak,
  current-practice arm) and `build-lmdb` / `query-lmdb` (mmap-backed B-tree,
  queryable on open — the strong baseline). Reports per-process minflt/majflt, VmPTE,
  VmHWM, and global PageTables.
- `scripts/` — Linux-side sweep and N-reader launchers.

## Status / wiring

**Wired and verified** (2026-08-19: `cargo check-all` exit 0, elephance compiled in
the third-party collection). Wiring uses xtask's existing third-party mechanism (the
same one that builds `kibi`): the `[workspace.metadata.third-party]` entry compiles
`twz/` as its own ephemeral workspace over its own manifest — the crates' `[workspace]`
tables here are what that mechanism expects, and the root workspace's `members` list is
never touched. xtask was additionally taught to *skip* (with a note naming package and
path) any third-party entry whose local path is absent, so this directory's presence
alone toggles elephance in and out of the build; checkouts without it build
identically. `wire.patch` is regenerated from the live diff and is the revert
reference (`git apply -R third-party/elephance/wire.patch`).

Pending harness changes:

1. **Build-phase batching (DONE + MEASURED 2026-08-20, tag eleph-smoke2 —
   `Sharded::insert_batch` buckets per shard, one `write_session` per shard per
   256k-chunk): 227.3 s → 65.9 ms at 200k entries (3,449x), persistent 0.33 µs/op vs
   volatile 0.28 µs/op — the per-op durability round trip is gone, paid once per
   batch.** smoke1 had shown persistent-arm
   inserts at ~1.1 ms/op vs 0.35 µs/op volatile (~3,000x). Attributed from source:
   `PersistentHashMap::insert` opens a `TxObject` per call and `TxObject` syncs on
   drop (`tx.rs`: `sync_on_drop: true` → `obj.sync()`), so every persistent insert
   enqueues one non-coalescing fire-and-forget SyncRegion into the pager's per-object
   serialized queue. Full path read (see faplan.md): with PHM's flags
   (`DURABLE|ASYNC_DURABLE`) the kernel blocks per op until the pager has actually
   paged the dirty pages out (`page_out_many`) — so the persistent arm is
   blocking-durable per insert *by accident*, and the 1.1 ms is a real NVMe round
   trip. Fix: build through `write_session()` — one tx, one blocking sync, per batch;
   durability semantics preserved. The post-build explicit null-info sync
   (`t_sync_us=66`) is fire-and-forget and proves nothing; a cross-reboot test still
   owes coverage of the store layer's power-fail semantics, nothing more.
2. **Regression guard worth keeping:** in the 2-reader smoke the second reader took
   7 faults / 16 frames vs the first's ~373 / ~1088 — shared object page tables
   working. If a change ever breaks table sharing, the second reader's fault count is
   where it shows first. Stable within ±1 across two builds and a kernel boundary
   (tags eleph-smoke1, eleph-smoke2).
3. Blocked on a kernel-side boundary: `MemoryStats` grows a
`pooled_pages` field (level-0 frames parked in per-cpu precharge pools; announced as
its own rebuild boundary because inserting it shifts every later field for stale
binaries). When it lands, `launch` should read and subtract it so the singles residual
(`_mod512`) becomes an exact page-table term instead of conflating pool occupancy.

Two wiring facts future editors need: (1) the ephemeral workspace does not inherit the
root `[patch]` tables, so `twz/Cargo.toml` carries a mirror of the active entries —
keep it in sync with root `Cargo.toml` when patches change there; (2) `twz/` builds
only via `cargo check-all`/`build-all`/image builds (the third-party collection), never
via plain cargo.

The `linux/` crate builds anywhere with plain cargo: `cargo build --release` in
`linux/` (needs a C compiler for the vendored liblmdb). It must stay out of the
Twizzler build entirely.

Corroborating result from a sibling project (llama-twz): cross-process KV-cache
reuse through a Twizzler object gives ~3.2 ms time-to-first-token on a
previously-seen 512-token prompt vs ~2500 ms cold. Cite
`/scratch/dbittman/llm/llama-twz/llama.cpp/twzm/serving.md` (methodology + raw data
+ what didn't work), NOT the bare number, and carry its conditions: the claim is
**recomputation avoided** — a repeat of a prompt an earlier process computed — not
faster inference; and it currently requires a single-slot server (no speculative
decoding, no multimodal) and excludes sliding-window models pending persisted
context checkpoints. Design note relevant to us: correctness comes from keying the
cache entry to the buffer's own name so the pair is written/read as a unit — the
same shape as our verify-by-recomputation choice, avoiding after-the-fact
fingerprinting of large state.

## Dataset definition

- Key `i` of a dataset is `key_at(seed, i)`: 128 bits, unique per index (SplitMix64
  finalizer is bijective over the distinct stream states).
- Its value is `props_for(key)`: 64 bytes, eight f64 "material properties" derived
  from the key. Deterministic and exact — verification is `fetched == recomputed`,
  bit-for-bit, in every arm.
- Query `j` targets index `query_index(qseed, j, entries)` (uniform). `--disjoint`
  gives reader `id` its own stream (per-reader working sets); default is a shared
  stream (shared working set), which is the sharing-friendly and honest default for
  experiment 2 — state which one a number came from.
- Twizzler shards at ≤4M entries per `PersistentHashMap` object to stay under the
  1 GiB object bound; shard = `key.lo % nshards`; shards are named
  `{base}-{k}` and discovered by probing at open.

## Output format

Every measurement is a single self-describing line, `ELPH key=value ...`, so
interleaved console output costs one datum and names itself (never grep-count over
interleaved logs; scope values to their own line). Common keys: `os`, `role`
(build/query/launch/stats), `arm` (twz implicit / flat / lmdb), `t_open_us`,
`t_q{N}_us` checkpoints, `verified`, and the fault/page-table counters
(`faults_global_delta` and `ktables_delta_frames` on Twizzler; `minflt_delta`,
`vmpte_kb`, `global_pagetables_kb` on Linux).

## Protocol notes (what makes the numbers defensible)

- Same-host VM pairs: run the Twizzler image and a Linux guest with identical QEMU
  configs (cores, memory, disk backend) on the same otherwise-idle host. Own the box.
- Experiment 1: sweep `--entries` from well under guest DRAM to past it; fresh boot
  (Twizzler) / dropped caches (Linux) per point; plot t_q1 and t_qAll vs size. The
  flat arm is linear by construction; the interesting fight is vs LMDB. Rounds: 3 is
  a floor for detecting gross breakage, not a significance test — an n=3 A/B on this
  box cleared its spread only on deltas the change couldn't have caused (measured
  2026-08-20). For claims, run enough rounds that the effect clears the *round*
  spread, and report the spread. Contended phases are the worst case: the contended
  create/delete bench ran ±53-59% across 3 rounds (uncontended sibling ±5%), so a
  percentage-scale delta there at small n is "consistent with no change". Small n is
  defensible only when the effect is orders of magnitude with many within-arm samples
  (the reap A/B: +16,578 frames → +7 at n=1, 200 samples/arm). Applies directly to
  experiment 2, whose launch phase is a contended-by-design workload.
- Experiment 2: warm pass first, then sweep N; plot `faults_global_delta` (Twizzler)
  vs summed `minflt_delta` (Linux), and `ktables_delta_frames` vs
  `global_pagetables_delta_kb`. Run the Linux side with THP enabled *and* disabled
  and report both, or the fault counts are off by up to 512x.
- Twizzler measurement discipline (from `sysbench.md`): one tree state per
  experiment, `many.py -j1` for anything quantitative, record the build id, spawn
  serialization in the monitor means `launch` timestamps spawn and query phases
  separately.
- `faults_global_delta` is system-wide; on an otherwise-idle guest the background
  noise is small but nonzero — take an idle-interval reading before trusting small
  deltas.
- `ktables_delta_frames` reads `tracker.kernel_used`, which counts page tables *plus*
  kernel heap backing, precharge pools, and kernel thread stacks. Two known confounds
  for experiment 2: (a) unreaped exited threads pin their 2 MiB kernel stacks
  (spawn-join leak, leakcheck.md lineage — ~11% of spawns in one measured boot), which
  lands directly in a spawn-N-readers phase — and the retention is a *rate* (reaping
  runs ~1/100 idle wakeups per cpu), so a burst of N spawns can leave most of them
  unreaped at measurement time; (b) page-table frame counts are not address-span
  (`kernel_used measures address span` note). The mitigation that works is the quanta
  signature, which `launch` reports directly (`ktables_delta_div512`/`_mod512`): stack
  retention moves kernel_used in 512-frame (2 MiB) steps, page tables in single
  frames. Caveat on the singles residual (`_mod512`): once the frame-pool rework lands
  with `FA_FREE_TO_POOL` on, freed level-0 frames park in `TLS_FRAME_ALLOCATOR`
  instead of returning — parking also moves kernel_used in single frames, so the
  residual conflates page tables with pool occupancy (bounded: ≤2048 frames/cpu,
  32 MiB at smp4). Baselines taken across that landing are non-comparable; check the
  const before attributing the residual. Do NOT trust a bounded settle to clear the
  backlog: reaping runs at
  ~(idle wakeups)/100 per cpu, so quiescence *slows* the drain rather than speeding it
  — measured convergence with 24 threads / ~48 MiB still pinned, drain ~2-3
  threads/min under ideal idle (leak29-kalloc -- raw logs pruned 2026-09-12, see
  `target/results/PRUNED.md`; the number stands as recorded here, the transcript does not
  survive re-examination). `--settle-secs` therefore defaults to
  0 and reports a settled delta that must not be read as stack-free until reaping is
  prompt. `launch` also reports `exited_backlog_delta`/`reaped_delta` (ThreadStats) —
  read the backlog delta as a **positive-only** indicator: reaping is gated on the
  running thread being in user mode (stat-tick path), i.e. anti-correlated with thread
  churn, so a kernel-heavy phase accumulates backlog while a user-mode-heavy phase may
  keep the reaper fed. Nonzero ⇒ the stack confound is present; zero does NOT certify
  the phase clean. Measured magnitude (leak30-gate, legacy arm; same prune, same
  reading rule): retention scales with
  spawn rate — 16% of spawns retained at 1 spawn/iter, 57% at 10, i.e. 2.54 GiB pinned
  by a 2,200-spawn phase — so an N-reader launch sits in the high-retention regime.
  REAPER BOUNDARY, CONFIRMED: the kernel reaper thread is default-ON as of
  2026-08-20 01:10 UTC (10-boot green matrix: kvm smp1/smp4 release+debug, TCG, and a
  no-flag regression boot showing backlog slope 0.0000 / reaped 10.0/iter at r2 1.0).
  These retention/drain numbers describe `--reap=legacy` from then on. In post-01:10
  kernels `exited_backlog_delta` reads 0 in every phase — a NONZERO value is now a
  finding, and a zero is the reaper working, not a clean-phase certificate.
  Separately, kalloc-census readings have a noise floor of ±1-3 in-flight allocations
  (~±100 B/op); anything at that scale is noise, not a finding. Census sanity
  reference: `p1-leak-object` reproduces at 5,247-5,248 B of kernel heap per leaked
  object across builds. If using `--kalloc-trap`, `every=1` fires only on boot-time
  allocations — use a large stride (e.g. 5000) to reach workload-time firings. The kalloc census (`InfoKind::KallocCensus` = 7, gated behind
  `--kalloc-census`) separates heap from table growth exactly: page-table frames never
  pass through the kernel heap.
