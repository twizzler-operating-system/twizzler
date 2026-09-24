# Exposing object preload through the C ABI

Scoped 2026-09-12. Not yet implemented: twizzler-0a's reap arm rebuilds the tree per round, so
the edits wait for their CLEAR.

## Why

`twz_rt_map_object` installs a slot mapping; PTEs populate on fault, and `fault_around()` returns
width 1 immediately for pager-backed objects (`kernel/memory/context/virtmem/region.rs:336`). A
model segment is pager-backed, so loading gemma's 259,662-page weight object is 259,662 separate
faults with no batching. The kernel already has the batching primitive; userspace C cannot reach
it.

## What exists, and the trap in it

| | |
|---|---|
| `ObjectControlCmd::Preload` | whole object. **Submits one range of `MAX_SIZE / PAGE_SIZE` = 262,144 pages regardless of the object's real size** (`syscall/object.rs:858-866`), then faults the meta page separately. |
| `ObjectControlCmd::PreloadRange` | `&[PreloadRangeSpec { start_page, nr_pages }]`, capped at `MAX_PRELOAD_RANGES = 16`. Ranges past the cap are **silently ignored, not rejected** (`object_control.rs:108`). |
| C ABI | `object_cmd` carries only DELETE/SYNC/UPDATE (`object.h:74-76`). `twz_rt_object_cmd(handle, cmd, data)` already has the right signature to carry more. |

The over-ask is the finding. It is ~1% for a segment at 99.9% of the object cap, which is llama's
case and why it does not hurt them:

    MAX_SIZE                262,144 pages
    gemma weight segment    259,662 pages   ->  1.01x over-ask
    a 15.4 MiB object         3,942 pages   -> 66.5x over-ask

An API whose cost is fine for the caller who asked for it and 66x wrong for the next caller is
worth ten lines to avoid.

## Design

**One command, no payload, correctly sized by the runtime.**

- C ABI: add `OBJECT_CMD_PRELOAD` to `object.h`. Nothing else crosses -- no `PreloadRangeSpec`
  array, because the one consumer maps exactly what it touches (dense repacked segments, every
  tensor in the mapped view used) and a range array would cross the boundary for nothing.
- Reference runtime `object_cmd`: read `MEXT_SIZED` off the handle, issue `PreloadRange` with a
  single spec covering `ceil(len / 4096)` pages. Fall back to whole-object `Preload` when the ext
  is absent, since then no length is knowable.
- Identical behaviour for a dense segment; no gigabyte request for a small object.

Synchronous, because the ABI call is synchronous. Async is the *caller's* policy: llama mirrors
its Linux `twzm_populate_async` -- background walk started after serving-ready, cancellable when
first-token wants the bandwidth -- so putting preload inside `twz_object_map` would move the whole
fault bill into load latency, which is the cost being hidden. One command, two call patterns.

## Files

- `src/abi/include/twizzler/rt/object.h` -- the constant. **`src/abi` is a git submodule and
  already carries uncommitted work from another mission** (nlink/`FD_CMD_SET_TIMES`/`twz_rt_fd_link`
  in fd.h and thread.h, plus a `set_meta_ext` dirty-store fix in rt-abi/src/object.rs). Establish
  authorship before adding to that pile; the preload change does not collide textually.
- `src/abi/rt-abi/src/object.rs` -- `ObjectCmd` enum + `TryFrom`.
- `src/rt/reference/src/runtime/object.rs` -- the `object_cmd` arm.
- bindings regenerate via `build.rs` bindgen; no hand edit to `bindings.rs`.
- **`src/rt/minimal/src/runtime/syms.rs:788-801` -- NOT optional.** Its `object_cmd` match is
  exhaustive (`ObjectCmd::Sync | ObjectCmd::Update => NOT_SUPPORTED`), so adding a variant to the
  enum breaks the minimal runtime's build, not just its behaviour. Implement it there too rather
  than widening the NOT_SUPPORTED arm: `find_meta_ext` is on the shared rt-abi `ObjectHandle`, so
  the sized-range version is the same few lines in both runtimes. This is the
  "a new ABI value needs both runtimes" rule arriving as a compile error rather than a runtime
  surprise, which is the good version of it.

## Measurement

`DUPSRC` (`kernel/pager/inflight.rs:471`) counts overlapping in-flight page-data requests, which is
what racing demand faults produce. It should go to **zero for the load phase** under preload. That
is the before/after, and it is independent of wall-clock -- which is the right property on QEMU,
where timing means nothing.
