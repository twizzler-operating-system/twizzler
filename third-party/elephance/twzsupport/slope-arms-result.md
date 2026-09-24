# The 768 slope arms: the ratchet is real, and it is not the reclaim feature

Two runs, 2026-09-12, `xtask test --scenario lowmem --profile release --enable-kvm --memory 768`,
`FRAME_SLOPE=true` in both, `RECLAIM_CLEAN_BACKED` the only variable.

Free level-0 frames at each test's start (fixed alphabetical suite order):

| # | test | reclaim ON | reclaim OFF |
|---|---|---|---|
| 1 | net_test | 7664 | 7063 |
| 2 | async_test | 6704 | 6103 |
| 3 | brush | 2656 | 2563 |
| 4 | cache | 2840 | 2777 |
| 5 | cache_srv | 2609 | 2585 |
| 6 | ctl | 2740 | 2524 |
| 7 | devfs_test | 2521 | 2478 |
| 8 | devmgr | 1845 | 1988 |
| 9 | devmgr_srv | 2163 | 1910 |
| 10 | display_srv | 1910 | 1825 |
| 11 | dynlink | 1910 | 1825 |
| 12 | femto | 1910 | 2165 |
| 13 | hmrepro | 2183 | 1788 |
| 14 | init | 960 | 631 |
| 15 | kqueue_test | 866 | 1357 |
| 16 | leakcheck | 566 | — |
| 17 | llt | 218 | — |
| outcome | rc=34, budget exit at #17 | **rc=35, panicked at #15** |

## 1. The ratchet exists

Not a sawtooth with a flat envelope. Free frames fall from ~7,000 to a few hundred across
fifteen-odd test compartments, with small partial recoveries (#3→#4, #8, #12) that never restore
the level. Frames do not fully come back at compartment exit. By the reading guide fixed before
the data, that is hypothesis **(b): retention**.

This is the `cannot wait for page` panic caught in the act rather than inferred from where it
landed: the system arrives at the #15 band with a few hundred free frames, and whether it survives
is then a coin toss.

## 2. It is NOT the reclaim feature

**Both arms ratchet at the same rate.** The two series track each other within noise the whole
way; the OFF arm is slightly lower at most points and slightly *higher* at #8, #12 and #15, which
is the signature of run-to-run variation, not of a mechanism. Disabling clean-backed reclaim
entirely does not flatten the decline.

So `RECLAIM_CLEAN_BACKED` is exonerated for the panics, by the same standard that exonerated it
for the net regression earlier: a matched pair with the mechanism as the only variable.

The outcome difference (ON reached #17, OFF panicked at #15) is **one run each** against a band
known to vary from #15 to #23 with one clean traversal in twizzler-0a's twelve. It is consistent
with reclaim helping marginally and is not evidence of it.

## 3. What is actually being retained: frames, not heap

This is what the `kalloc` columns were carried for, and they answer it:

| | first test | last test | change |
|---|---|---|---|
| kernel heap (`kalloc_late`) | 4.6 MiB | 5.4 MiB | **+0.8 MiB** |
| free frames | 7664 | 218 | **-7446 (-29 MiB)** |

The kernel heap is essentially flat while 29 MiB of frames disappear. Page-table frames never pass
through the kernel heap, so the retention is in frames — object pages, page tables, or thread
stacks — and **not** in allocations. That rules out a kernel-heap leak and points at the frame
accounting, which is where the known unreaped-thread-stack retention lives (2 MiB per unreaped
thread; see ../README.md).

## 4. What this leaves open

The ratchet is real, unattributed, and **predates or ignores the reclaim feature**. The next
candidate is the one the counters point at rather than the one anybody suspected: per-compartment
frame return at exit. `exited_backlog_delta` / thread-stack retention is the obvious first place to
look, and it is a different investigation from either of tonight's.
