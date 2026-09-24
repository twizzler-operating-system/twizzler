# Preload A/B: the predicted mechanism failed, and the reason is by design

Two boots, 2026-09-13, quiet box, `-smp 4`, same store / same object ids / same data.img.
Arm A = llama cold. Arm B = `elephance preload` of the weight+vocab objects first, then the same
llama run.

|  | arm A (cold) | arm B (preload first) |
|---|---|---|
| preload: weight object | — | ok=true, **60 pages**, 1.7 ms |
| preload: vocab object | — | ok=true, **-3 pages**, 3.3 ms |
| **llama's own pages fetched** | 284,809 | **281,282** |
| llama's faults | 8,365 | 7,179 |
| llama `load time` | 522.87 ms | **303.80 ms** |
| llama prompt eval | 1246.92 ms | 1238.70 ms |
| llama eval | 23.52 tok/s | 23.24 tok/s |
| total wall | 4,585,672 us | 4,374,845 us |

## The prediction was wrong

I predicted the preload lines would carry ~284k pages and llama would drop to the idle floor.
**Preload fetched 60 pages out of 284,809.** llama still paid 281,282 — 1.2% less than arm A, i.e.
nothing. The mechanism I proposed does not work.

## Why, from the source

`ObjectControlCmd::Preload` submits one `PagerFlags::PREFETCH` request. The pager **declines
prefetches outright** above a very low cap (`MAX_INFLIGHT_PREFETCH = 2`,
`pager-srv/src/request_handle.rs:49,439-446`) and answers them with `Okay | DONE` **carrying no
pages**. The comment states the policy plainly: *"Speculation must never crowd out a demand fault
... over the cap we simply decline: the kernel never waits on a prefetch, so acking it is a
complete answer, and the pages get read on demand if they are ever actually wanted."*

So a whole-object preload is **not a bulk loader and was never meant to be**. It is an advisory
hint the pager is free to ignore, and for a 1 GB object it essentially always does. Two consequences:

- The `cache-srv preload` recommendation would not have worked even if `cache-srv` were in the
  image (it isn't — also commented out of the initrd list).
- The 66x whole-object over-ask I wanted this run to test **was not exercised at all**. Nothing was
  fetched, so the vocab result is vacuous rather than reassuring.

## What did happen, and it is unexplained at n=1

`load time` fell **522.87 → 303.80 ms (-42%, -219 ms)**, and total wall fell 4.6% — consistent
with each other. But it cannot be the data, because the data was not prefetched. The likely
candidate is the object-metadata round trip: `Preload` performs `lookup_object_and_wait`, which
fills the object's meta page (`request_handle.rs`'s note that `lookup_object` now does real I/O and
a physrw round trip), so llama's open path finds it warm. `ktables_delta` also fell 4,925 -> 667,
which is the right shape for object/page-table setup being prepaid — but `kernel_used` conflates
page tables with heap and stacks, and **n=1 with no spread is not a result**. Recorded as an
observation, not a finding.

## What would actually work

Preload cannot bulk-load because the pager declines speculation. The paths that can:

1. **Touch the pages.** Map the object and read one byte per page: those are *demand* faults, which
   the pager never declines, and they already batch ~34 pages/fault. Cost is the same I/O, moved to
   where we want it. Entirely userspace, no kernel change — this is what llama's Linux populate
   thread does.
2. **Issue non-speculative ranges.** `sys_object_preload_range` exists but the kernel flags both
   preload variants `PREFETCH`, so it inherits the decline. Making bulk preload issue demand-class
   requests (or chunk below the cap) is a small kernel change with a real policy question attached:
   the cap exists to stop speculation starving real faults, and a bulk loader is exactly the thing
   it was written to prevent.

Option 1 is the next test and costs nothing but a boot.

---

# Arm C: touching works, and the access pattern matters more than expected

Three back-to-back boots, 2026-09-13, quiet box held by llama-twz-72, same store / ids / data.img,
`elephance warm` (map + `read_volatile` one byte per page) as the prebuild, then the same llama run.
`llama-completion` bit-identical to arms A and B, so the only delta from A is the warm step.

| | arm A (cold) | arm B (preload) | **arm C (warm), n=3** |
|---|---|---|---|
| prebuild pages moved | — | 60 | **264,400** (260,300 weights + 4,135 vocab) |
| prebuild wall | — | 19 ms | **209.5 +/- 3.7 ms** |
| llama pages fetched | 284,809 | 281,282 | **19,971** (-93.0%) |
| llama faults | 8,365 | 7,179 | 7,143 |
| llama `load time` | 522.87 ms | 303.80 ms | **102.8 +/- 6.4 ms** (-80%) |
| llama prefill | 1246.92 ms | 1238.70 ms | ~1,240 ms (unchanged) |
| llama-visible TTFT | ~1,770 ms | ~1,543 ms | **~1,343 ms** |

Pre-registered by both sessions before the run: warm carries ~281k pages, llama drops to floor,
load reaches the 80 ms class, prefill unchanged, TTFT ~1.33 s. **Measured: 1.343 s.** The
prediction was right to three significant figures, which is worth more than the number itself --
the model of what preload should have done was correct; only the mechanism was wrong.

## The touched count is exactly the denominator

`touched=259662`, against the 259,662 weight pages pre-registered from the staging map a day
earlier, on a different staging with re-minted ids. The instrument is measuring the object we
think it is.

## Sequential touching batches 14x better than llama's own access

> **RETRACTED 2026-09-14 — see the CORRECTION section at the end of this file.**
> `pages/fault` mixes fault classes; widening fires under llama's access order too
> (cold llama measures the same 512/512/512 merges). The measured quantities in arm C stand;
> this explanation does not.

    warm:  259,662 pages / 555 faults  = 468 pages per fault, 5.08 GB/s
    llama: 284,809 pages / 8,365 faults = 34 pages per fault  (arm A)

Same bytes, same device, same kernel. The *access pattern* is the difference: a linear sweep lets
the pager widen into large sequential runs, while llama's load order fragments into many smaller
ones. That is why the whole gigabyte costs **209 ms** here against the ~500 ms this was expected to
take, and it is the real reason the win is as large as it is.

## What this does and does not buy

- **Does**: removes 93% of llama's page-ins and 80% of its model-load time. If the warmer runs at
  boot or in idle, that 420 ms leaves the request path entirely.
- **Does not**: touch the 1,240 ms prefill, which is compute on a cold prompt. Only the (separate,
  llama-side) KV-sync work addresses that, and it applies to *known* prompts rather than new ones.
- **Is not free if serialised**: warm (210 ms) + llama (1,343 ms) = 1,553 ms against arm A's
  1,770 ms. Run before the request it is a 24% TTFT cut; run inline it is 12%.

## Residual

llama still fetches ~20,000 pages after a full warm. Unexplained; candidates are the KV buffer,
the prompt-cache object, and runtime/libraries that the warm step does not cover. Worth one look
before anyone calls the cold path solved.

## Method note

All three runs exit `rc=1`. That is qemu's `isa-debug-exit` encoding of guest shutdown code 0
(`(0 << 1) | 1`), visible as `performing debug shutdown with code 0` in every log -- not a failure.
Arms A and B did the same; I had not checked their exit codes either.

---

# Persistence probe: runtime-created NAMES do not survive reboot

Two boots, same kernel (`.text` md5 `c2f1f8df…` verified identical before and after boot 2's
rebuild, so this is not a build artifact), same data image.

- **Boot 1**: `elephance build --name=persistprobe --entries=5000` — `persist=true`, objects created
  via `ObjectBuilder` with `LifetimeType::Persistent`, registered with the naming service,
  `t_sync_us=332`. Exit 0.
- **Boot 2**: `elephance query --name=persistprobe` — **panics: "no shards found under
  persistprobe"**. The failure is at `nh.get(&name)` (shard.rs:96), i.e. the **name lookup**, before
  any attempt to map the object.

So the probe I built to separate "naming path creates volatile objects" from "persistence is
broken" hit a third thing first: **the name is gone.** Whether the object survived is still unknown
— nothing got far enough to ask.

## Why that is probably the root cause for llama too

`naming-srv::namer_start` builds its store as:

    let namer = Namer::new_with(bootstrap).or::<ErrorKind>(Ok(Namer::new())).unwrap();

If loading the root from the bootstrap object fails **for any reason**, it silently constructs an
empty store and boots normally. There is no warning, no log line, and every name created by a
previous boot is simply absent. A caller sees "not found", which is indistinguishable from "never
existed".

llama's KV restore looks its entries up **by key name** under `/twzm`. So even if the KV objects
were persistent and synced correctly, the restore would still miss — the name needed to find them
is gone. That makes naming persistence a *prerequisite* for their reboot-warm work, sitting
underneath the object-lifetime question rather than beside it.

## What is now established vs still open

- **Established**: runtime-created names do not survive a reboot in this configuration; the fallback
  to an empty root is silent.
- **Open**: whether the persistent *objects* survive (untested — the name failed first); whether the
  root's bootstrap object is persistent and synced; whether `Namer::new_with` is failing or was
  never given a saved root to load.

The next probe is to look the object up **by id** rather than by name after a reboot, which
separates the two cleanly. That needs the id carried across boots out-of-band — printing it in
boot 1 and baking it into boot 2's autostart.

---

# The naming persist fix: validated on the path that was broken

Landed 2026-09-13 (Daniel's call, 0a's diagnosis, option (a) read-back).

**The fix.** `NsShared` gains `obj_persist`, read from the object itself via
`sys_object_stat(id).life == Persistent` at map time; `create_file` branches on that instead of the
`persist` flag threaded down the path walk. A failed stat falls back to the inherited flag, i.e. to
the old behaviour. `NamespaceObject::new` sets it directly — the object was just built with that
lifetime, so statting it would be asking the kernel what we just told it. Separately,
`naming-srv`'s silent empty-store fallback now emits a `tracing::warn!` naming the bootstrap object.

**The validation.** `elephance mk-file` creates a file through `std::fs::File::create` — the fd
path that `create_file` serves, *not* `ObjectBuilder`, which was never broken:

    ELPH role=mkfile_mkns parent=/twzm r=None
    ELPH role=mkfile path=/twzm/persistcheck bytes=65536 ok=true id=b21fea2a2fa4950a5551b7d14d0e15a9

Offline, in the store:

    /ids/b2/b21fea2a2fa4950a5551b7d14d0e15a9   69,632 bytes

69,632 = 65,536 of data + the 4 KiB null page. **A guest-created file now reaches the disk store,
with its contents.** Before the fix, llama-twz-72's identical path produced *zero* guest-written
objects in `ids/` after a full boot with `SYNC_FLAG_DURABLE` syncs returning success.

**What is established, and what is inference.** Established: with the fix in, an fd-created object
lands in the store at the right size. The counterfactual is llama's prior measurement on the same
path rather than a re-run with the fix reverted — **I did not run a with/without A/B**, and the
attribution rests on that plus 0a's mechanism trace. Given the mechanism is understood and the
prior negative is direct, I am satisfied; anyone who is not should revert the const and re-run.

**A count-delta check I attempted is void**: I recorded 748 objects before and 295 after, but I
changed the shell filter between the two samples, so they do not measure the same thing. The
object-id lookup above does not depend on counting and is the result. Do not quote the counts.

**Still broken, untouched by this:** runtime-created *names* do not survive a reboot
(`namer_start`'s fallback to an empty store). llama's KV restore needs both. This fix removes the
one that made durable syncs succeed while doing nothing.

---

# CORRECTION (2026-09-14): the "14x better batching" claim above is withdrawn

Arm C's section claims:

> *"Sequential touching batches 14x better than llama's own access — warm: 468 pages/fault,
> llama: 34 pages/fault. The access pattern is the difference."*

**That is wrong.** `pages/fault` mixes fault classes, and the ratio is not measuring batching.

What settled it: a cold llama run with `--diag=pager` (llama-twz-72, 2026-09-14) returned
`LARGEPAGE: 512 object-aligned candidates, 512 also phys-aligned, 512 merged` — byte-identical
to a linear sweep. The kernel widens a fault into an empty 2 MiB region to a **1024-page**
aligned request (`obj/data.rs:1031`), and that widening **fires under llama's access order
too**. I had predicted the opposite from the 34 pages/fault figure; the boot refuted it.

Working it through with widening established for both:

    sweep:  259,662 pages /   555 faults  -> 259,662/1024 ~ 254 widening faults ~ 555 observed.
                                             The sweep's faults are ~all page-in faults.
    llama:  284,809 pages / 8,365 faults  -> its page-in faults are also ~250-500.
                                             The other ~8,000 fetch NOTHING.

Arm C confirms it directly and was in the data at the time: llama took **7,143 faults while
fetching 19,971 pages**. Faults barely moved (8,365 -> 7,143) while page-ins fell 93%. Not a
batching difference — **~7,000 faults per run that transfer no data**, i.e. pure address-space
work. That is the same phenomenon as "page-ins amortise, address spaces do not", which both
0a and I had already written down and neither of us connected to this number.

**What survives unchanged:** every measured quantity in arm C — pages moved, load time
523 -> 103 ms, TTFT 1,770 -> 1,343 ms. The warming win is real and is prepaid I/O. Only the
*explanation* ("the access pattern batches better") is withdrawn.

**What this opens:** the residual is mostly the faults, not the pages. ~7,000 no-data faults
per run, at an unmeasured per-fault cost, against a warm `load time` of 103 ms. Measure the
per-fault cost before acting on it; the fault path is already instrumented (`FAULT_PROFILE`).

## Large-page coverage, measured

`--diag=pager`, `elephance warm` over W (1,063,571,968 bytes) + V:

    LARGEPAGE: 512 object-aligned candidates, 512 also phys-aligned, 512 merged

100%, and the same cold. The 1 GiB weight object is backed by ~507 **2 MiB** leaves. Linux
serves the identical file-backed mapping as **262,144 4K PTEs** (`FilePmdMapped: 0`, measured
in-guest by llama-twz-72; Linux THPs the anon KV/scratch at 86 MB but never file-backed
weights), against a ~1.5k-entry L2 STLB. A structural advantage Linux cannot match on this
path, present in every run, and the standing mechanism candidate for the ~4% prefill edge.

Mechanism, verified statically end to end:
- Large leaves form by **exactly one path**, `merge_frame` (`pager/queues.rs:437-475`). The bulk
  install path caps provider offers at 4K by design (a huge leaf over 512 separately-owned
  frames would take one refcount and free one on unmap). So the LARGEPAGE counter is a
  **complete census**, not a sample.
- `pager-srv/src/data.rs:368` `try_alloc_pages` asks for a 2M-aligned 512-page physical run when
  the object offset is 2M-aligned and the ask is >= 512 pages; otherwise it allocates only to the
  next 2M object boundary, re-aligning the cursor. The pager is built to emit mergeable runs.
- `Table::object_map` splices the **object's own page table** into each context rather than
  copying PTEs, so coverage is a property of the object, inherited free by every mapper — and
  fault order cannot affect large-page formation.
- `mod largepage`'s own doc comment predicting `phys_ok` "well below candidates" is **stale**:
  the failure it warns about was fixed and never measured until now.

Not worth chasing: 1 GiB leaves. 507 entries already fit an STLB comfortably; going to 1 would
buy nothing.
