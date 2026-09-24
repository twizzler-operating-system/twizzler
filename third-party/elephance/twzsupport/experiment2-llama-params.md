# Experiment 2 with llama-completion as the witness

Denominator supplied by llama-twz-72 on 2026-09-12, captured from the live staging log and
cross-checked against `twzm-info` run inside the guest (boot 4), rather than re-derived later.

## The object under test

| | |
|---|---|
| weight segment (view 0 segment 0, the only bulk object) | guest ObjID `3e153245f9dd38c24bd108475e25caae` |
| size | 1,063,571,968 bytes |
| demand-faultable pages | **259,662** (259,661 full + 1 partial of 512 B), plus null/meta |
| vocab | separate object, 15.4 MiB |
| root | 56.6 KiB, in the initrd |

Arithmetic confirmed independently: 1,063,571,968 / 4096 = 259,661 r512.

**The object bound is handled, and gemma being single-object is incidental.** `MAX_SIZE` is
exactly 1 GiB (`src/abi/rt-abi/src/object.rs:631`); this segment clears it by 9.7 MiB. That
headroom is luck and nothing depends on it: the `.twzm` format is v4 multi-object (tonight's
staged store printed "TWZM root object, version 4" with view/segment tables from inside the
guest), and the converter enforces a 1 GiB - 12 KiB cap -- the null page at the bottom plus
meta+FOT at the top, which is the `MAX_SIZE - NULLPAGE_SIZE` truncate ceiling with the extra
reserved pages accounted. Real models already split: llama-3.2-1b q8_0 is 2 objects (1.223 GiB),
qwen3-30b-a3b is 19 objects (17.3 GiB), both already converted under
`/scratch/dbittman/llm/models`.

**Ownership, per Daniel 2026-09-12: objects being too big is the llama side's to fix, not ours.**
So nothing here scopes work on `MAX_SIZE` or the object-size path, and elephance should treat the
1 GiB bound as a fixed property of the substrate when sizing its own datasets (which it already
does -- sharding at <=4M entries per `PersistentHashMap`, see ../README.md).

So the interesting rungs past gemma are about *read paths*, not about the cap: llama-3.2-1b
exercises a 2-segment read on Twizzler for the first time, and qwen3-30b is the `/ext`-only case
now that the external-file length bug is retired -- 17.3 GiB will never ride an initrd. For
elephance specifically, qwen3-30b is also the more interesting experiment-2 subject, because a
17.3 GiB store against guest DRAM is the regime where the N-reader sharing claim and the
hydration crossover of experiment 1 stop being separable questions.

## What the arms should show

The witness is **N `llama-completion` processes** mapping this one object, not llama-server (its
core is the vendored HTTP server and does not cross-build).

- **Twizzler**: ~260k system-wide pager fetches for the segment *regardless of N*. Object page
  tables are linked into each context via the `OBJECT_TABLE` entry bit, so residency is shared and
  readers 2..N should approach zero faults. The magnitude is what makes this clean: at 259,662
  pages, "flat versus linear" is not a subtle effect.
- **This is a floor, not an estimate.** `fault_around()` returns width 1 immediately for
  pager-backed objects (`kernel/memory/context/virtmem/region.rs:336`), so the first reader takes
  one fault per page with no batching -- there is no fault-around term to subtract.
- **Linux**: N minor faults per page and O(N x dataset) PTE memory. Run with THP on *and* off and
  report both, or the counts differ by up to 512x.

## Confounds that apply here specifically

- `faults_global_delta` is system-wide; take an idle-interval reading first.
- `ktables_delta_frames` conflates page tables with kernel heap, precharge pools and thread
  stacks -- use the quanta signature (`_div512`/`_mod512`), and see the retention notes in
  ../README.md.
- `DUPSRC` (kernel/pager/inflight.rs:471) counts overlapping in-flight page-data requests. Near
  zero in steady state; it spikes only where demand faults race. That makes it the before/after
  for `OBJECT_CMD_PRELOAD` as well as a check that the N readers are genuinely sharing rather
  than racing to fetch the same ranges.


## The instrument (added 2026-09-12, NOT YET COMPILED)

`elephance witness` in `../twz/src/main.rs`: runs an arbitrary program N times and reports what
each run costs in page-ins. Written because llama's warm result -- second process starting in
~80 ms against Linux's ~137 ms -- is *consistent with* sharing and does not demonstrate it. A
timing inversion has two explanations and only a counter separates them: pages genuinely resident
and shared across compartments (the claim), or the second process enjoying a warm host page cache
underneath the guest (which the Linux arm gets too).

    elephance witness --exe llama-completion --n 2 [--concurrent] [--idle-secs 2] -- <child args>

- **sequential** (default) answers *does the second reader pay?* -- per-process fault deltas, and
  the N=2 case is the sharing claim reduced to one number.
- **--concurrent** answers *does the cost scale with N?* -- one aggregate delta, because the
  counter is global and overlapping children cannot be attributed. Reporting a per-child number in
  that mode would be inventing one.
- **--idle-secs** samples the fault counter before the first spawn. `faults_global_delta` is
  system-wide; "about zero" means nothing without a measured floor.

Recipe for the witness itself comes from llama-twz-72: two processes in one boot via
`shell -c "llama...;llama..."` with `-f /ext/prompt.txt --prompt-cache`, so the second process is a
real warm reader running a real workload rather than a synthetic one. They also offered to fold the
no-`MEXT_SIZED` preload fallback check into the same boot.

**Status: written, not compiled.** twizzler-0a holds the floor for two benchmark arms, and a
`check-all` would contend for CPU with a bench in flight. Compile at their FLOOR-BACK, before
running anything.
