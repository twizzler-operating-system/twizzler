# The lowmem-768 panics: what the logs actually say

Analysis of twizzler-0a's 12-run `--scenario lowmem` 768 MB batch
(`target/results/complfix/run{1..12}.log`), 2026-09-12. Written because the framing everyone
including me had been using -- "my uncommitted handlecache work panics 10-in-12, up from ~3
occurrences total" -- is wrong in two ways that change which hypotheses are live.

## What the panic is

Every one is the same site:

    panicked at src/kernel/src/memory/tracker.rs:901:14: cannot wait for page

`alloc_frame` is `try_alloc_frame(...).expect("cannot wait for page")` -- a physical frame
allocation that failed in a context that cannot block to wait for one. **This is memory
exhaustion, in the kernel, not a fault in `handlecache.rs`** (which is userspace, in the reference
runtime). Any connection to my work has to run through *retention causing the pressure*, not
through the panicking code.

## Correction 1: it is positional, not a rate

The test suite runs in a fixed alphabetical order, identical in every run. Panic positions:

| position | test | runs |
|---|---|---|
| #15 | kqueue_test | 1, 2, 3, 4, 6, 11 |
| #16 | leakcheck | 5, 10 |
| #18 | lltest | 8 |
| #23 | memhog_test | 7 |
| -- | no panic, ended at #14 | 9 |
| -- | no panic, reached #19 | 12 |

Ten of the twelve fail in a narrow band starting at test #15, after fourteen tests have run
clean. That is the signature of **cumulative pressure reaching a threshold**, not of a race:
a race would scatter.

## Correction 2: the denominator is wrong

Runs 9 and 12 did not survive the failure -- run 9 **never reached the band** (it ended at #14,
an rc=34 budget exit). So the honest count is **10 of the 11 runs that reached the band**, and
run 12 is the single genuine traversal: it cleared #15-#18 and reached #19.

So "10-in-12, intermittent" understates it. It is near-deterministic on arrival at a specific
point, with one exception.

## What this does to the two hypotheses

0a's pair was: (a) the parked-completion deadlock was masking these, so runs now live long enough
to reach them; (b) the rate rose with my reclaim/handlecache work.

The positional clustering fits (a) comfortably: a run that wedged earlier never got to test #15.
It does not *refute* (b) -- retention from my work could be what puts the system close enough to
the edge that arriving at #15 is fatal -- but it removes the reason to prefer (b), which rested on
"a rare bug became reliable" and there is no rareness to explain.

What the logs say about my sweeper specifically, all of it weak:

- The monitor's `HandleSweeper` (`src/rt/monitor/src/mon/handlesweep.rs`) fires **urgently** and
  reclaims: `cached=126 reclaimed+=64 total=64 unmapped=64 norecord=0 urgent=true`. It is doing
  its job at the moment of pressure, not sitting idle.
- All three runs where it never fired (2, 6, 11) panicked; run 12 -- the one clean traversal --
  had it fire once. With n=3 and n=1 that is an anecdote, not a signal, and it points the
  opposite way from "the sweeper is the problem".
- The panic lands immediately after `STARTING <test>`, i.e. during **compartment spawn**, which is
  exactly where cached handles and mappings are in play. That keeps (b) alive.

## The discriminator, not yet run

An A/B at the same scenario with the handlecache/handlesweep changes reverted. Needs a build and
the floor; queued behind llama's perf pass. Predictions worth committing to in advance:

- If (a): the reverted arm panics in the same band, at similar positions.
- If (b): the reverted arm clears the band, or dies materially later.

One design note for whoever runs it: **run 9 is why the arm needs a completion criterion, not a
pass/fail.** A run that exits on budget at #14 looks like a pass and proves nothing. The arm must
report the position reached, and only runs that reached #15 count.
