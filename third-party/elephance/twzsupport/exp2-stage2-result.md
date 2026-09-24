# Experiment 2, stage 2: the sharing claim, measured

Boot of 2026-09-12 on llama-twz-72's staged witness image (`disk-5116e2bbac5d82a4` + appended
llama entries), release/KVM, `-smp 4`. Two sequential `llama-completion` processes over the same
gemma-3-1b-q8_0 `.twzm` store; child args identical to llama's clock run, same 109-token prompt.

| | run 1 (cold) | run 2 (warm) |
|---|---|---|
| **pages fetched (`pagedata_delta_frames`)** | **+284,172** | **-3,049** |
| faults (`faults_delta`) | 8,409 | 1,842 |
| page-table/kernel frames (`ktables_delta_frames`) | +4,914 | -3 |
| tlb shootdowns | 5 | 13 |

Idle floor for this boot: **170 faults / 2 s** (not 0 — see caveats).

## The result

**Run 2 fetched zero pages.** Not "few" — the delta is *negative*, so nothing was paged in and a
little was released. Run 1 paged in 284,172 frames; the pre-registered denominator for the weight
segment was **259,662 pages**, and the 24,510 excess is the vocab object (~3,942), the KV, the
runtime and the prompt cache. Prediction and measurement agree to within the parts that were never
counted.

So: **one page-in system-wide, amortised across every later process.** That is the claim, with a
counter, and it is the thing llama's 80 ms-vs-137 ms timing was *consistent with* but could not
demonstrate — a warm host page cache underneath the guest would have produced the same clock and a
very different `pagedata` number.

**And the claim is narrower than "run 2 is free".** Run 2 still took 1,842 faults. Those are PTE
population in its own address space plus process startup — per-process work that sharing does not
remove — against **zero** page-ins, which are system-wide and happened once. That distinction is
the whole result: *page-ins amortise, address spaces do not.* Stage 1 showed the same shape at
small scale (~175 faults of fixed startup, ~10 of data).

## Caveats, stated rather than buried

- **The idle floor was 170 faults / 2 s, not the 0 of stage 1.** This image's `display_srv`
  panicked once during boot (`start_display_direct`, unrelated to this experiment), and the guest
  was not as quiet. Run 2's 2.9 s therefore carries roughly 250 faults of background, so ~1,600 of
  its 1,842 are attributable. This does not touch the headline, which is the `pagedata` counter.
- **Wall-clock from this boot is contaminated and is not quoted.** llama was booting the
  platform-test check on a separate copy concurrently. Fault counters are per-guest so the primary
  measurement is unaffected; `wall_us` is not, and was predicted to be unusable before the run.
- **`ktables_delta_frames` is suggestive, not quoted as evidence.** +4,914 then -3 is the shape
  page-table sharing predicts, but 259,662 pages need only ~510 frames of leaf page tables, so the
  bulk of that 4,914 is kernel heap and thread stacks, which `kernel_used` also counts. What is
  safe to say is the *sign*: run 2 added none.
- n=1. The effect is five orders of magnitude, so it does not need statistics to be believed, but
  it has not been repeated.

## Status of the runtime work this depended on

All four files of the PRELOAD change have now executed. llama's `twzm-platform-test` run reports
**65 checks, 0 failed**, including the new "preload of an unsized object falls back and succeeds"
case — the one path in my arms that had never run. The original 57/1 is fully closed.
