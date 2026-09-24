# Experiment 2, stage 1: instrument validation (synthetic witness)

Boot of 2026-09-12, release/KVM, `elephance witness` driving three sequential `elephance query`
readers over a 200k-entry volatile dataset built in the same boot.

    role=witness_idle  secs=2   faults_delta=0        <- measured floor, not assumed

| reader | witness faults_delta | child's own query-phase faults | wall_us |
|---|---|---|---|
| 0 | 189 | 14 | 13,121 |
| 1 | 175 | 11 | 11,460 |
| 2 | 173 | 10 | 11,997 |

## What this establishes

- **The instrument works**, and the idle floor is genuinely 0 faults over 2 s, so every delta
  below is attributable rather than swimming in background noise.
- **Data-phase cost is flat and tiny**: 14 / 11 / 10 faults for 20,000 verified queries each. The
  builder made the object resident in this same boot, and three successive *processes* then read it
  for ~10 faults apiece. Residency survives process exit and is shared — which is the claim, at a
  scale too small to be impressive.
- **Per-process startup dominates and does not shrink**: the witness-level delta is ~175 faults
  per reader, flat across all three. That is compartment spawn plus runtime, and sharing does not
  reduce it. Worth stating plainly because it bounds what the sharing claim can ever buy: the data
  gets free, the process does not.

## What this does NOT establish, and I am not claiming

- `ktables_delta_frames` was +4253 for reader 0 against +159 / +124 for readers 1 and 2, and
  `pagedata_delta_frames` was **-3147** for reader 0. The ktables pattern is the shape the
  page-table-sharing claim predicts, and I am deliberately not quoting it as evidence: this
  dataset is ~16 MB, whose page tables are single-digit frames, so +4253 frames (~17 MB) cannot
  mostly be page tables. It is kernel heap and thread stacks, which `kernel_used` also counts
  (see ../README.md on that confound). The negative `pagedata` delta is unexplained. At n=1, with
  a counter known to conflate three things, the honest reading is "not yet interpreted".
- Nothing here scales the claim. A 200k-entry dataset is small enough that every number is close
  to the floor.

## Why stage 2 is the real test

llama's weight object is **259,662 pages**. At that size the difference between "reader 2 pays for
residency" and "reader 2 pays nothing" is five orders of magnitude, not a handful of faults, and
`kernel_used`'s confounds stop mattering because the effect dwarfs them. Stage 1's job was to prove
the instrument reports something sane before pointing it at a workload someone else's result
depends on; it did.

## Harness defect found and fixed

`--prebuild` was whitespace-separated. `init` splits the autostart string on whitespace and does
**not** honour quotes, so `--prebuild "build --entries=..."` reached the program as `"build`,
`--entries=200000`, ... with the quote characters still attached (clap exited 2). Now
comma-separated. **No quoted argument can survive `--autostart`** — one token per argv entry.
