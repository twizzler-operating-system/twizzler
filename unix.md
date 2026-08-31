# Enabling `target_family = "unix"` for Twizzler

Status: plan + stage 1 in progress. Owner: session working from `/scratch/dbittman/review/twizzler`,
started 2026-08-28. Nothing here is committed; see "Coordination" at the bottom before rebuilding
the toolchain.

## Why

Twizzler's target spec declares no `families`, so `cfg(unix)` is false for every crate built for
`*-twizzler`. The ecosystem does not have a third arm. Measured against cargo's `Cargo.lock`
(505 packages, 408 present in the local registry):

- 45 crates reference `std::os::unix::*`.
- ~12 do so under `cfg(not(windows))` or with no fallback at all, i.e. a hard compile error, not a
  missing feature: `gix-fs` (`symlink.rs` calls `std::os::unix::fs::symlink`), `gix-lock`,
  `gix-path`, `ignore`, `walkdir`, `camino`, `same-file`, `tempfile`, `filetime`, `git2`,
  `cargo-util`, `rustix`.
- `home` 0.5.12 defines `home_dir_inner` only under `cfg(unix)`/`cfg(windows)` — no third arm.
- `tempfile` compiles via `other.rs` but every operation returns "not supported".

Forking these is a standing tax that grows with each dependency bump (`gix` alone is ~40 crates
deep). Flipping the family is a one-time audit against a tree we control.

## What the flip is

One line in `compiler/rustc_target/src/spec/base/twizzler.rs`: `families: cvs!["unix"]`.
`rustc_session/src/config/cfg.rs:247` iterates `target.families` and emits both the bare `unix`
atom and `target_family = "unix"`. There is no `is_like_unix` anywhere in the compiler — `families`
is the entire mechanism, so nothing else in rustc changes implicitly.

## What it also buys

Two patches carried by the rustc port exist *only* because `cfg(unix)` is false, and both become
duplicate definitions on the flip — they get deleted, not ported:

- `compiler/rustc_fs_util/src/lib.rs:87` — generic `path_to_c_string` is `cfg(any(unix, wasi p1))`
  and uses `os::unix::ffi::OsStrExt`.
- `compiler/rustc_session/src/filesearch.rs:60` — `current_dll_path` under `cfg(unix)` uses
  `libc::dladdr`, which our libc fork already declares. Replaces the wasi-style `Err` stub with a
  real implementation.

## Stages

### Stage 1 — target spec (done first, cheap, reversible)
Add `families: cvs!["unix"]` to `base/twizzler.rs`.

### Stage 2 — std backend selection (mechanical)
Eight `cfg_select!` blocks list the unix arm *before* the twizzler arm. `cfg_select!` is
first-match-wins, so these do not error — they silently compile twizzler against `pal/unix`:

| file | unix arm | twizzler arm |
|---|---|---|
| `library/std/src/sys/pal/mod.rs` | L7 | L59 |
| `library/std/src/sys/fs/mod.rs` | L9 | L36 |
| `library/std/src/sys/env/mod.rs` | L19 | L35 |
| `library/std/src/sys/fd/mod.rs` | L6 | L14 |
| `library/std/src/sys/alloc/mod.rs` | L73 | L110 |
| `library/std/src/sys/io/mod.rs` | L26 | L42 |
| `library/std/src/sys/net/connection/mod.rs` | L3 | L24 |
| `library/std/src/sys/pipe/mod.rs` | L4 | L16 |

Fix: hoist the twizzler arm, or add `not(target_os = "twizzler")` to the unix arm. Prefer hoisting —
it matches the pattern already used in `sys/thread/mod.rs`, `sys/sync/*`, `sys/thread_local/mod.rs`.

Already correct, do not touch: `sys/thread`, all `sys/sync/*`, `sys/thread_local`,
`sys/process/unix/mod.rs` (twizzler arm precedes), `sys/process/mod.rs` (both arms resolve to
`mod unix`), `sys/stdio/unix.rs` (`any(unix, twizzler)`), `sys/os_str/mod.rs` (twizzler lands in
the `bytes` arm, which is what unix assumes anyway).

One hard error: `library/std/src/sys/io/io_slice/iovec.rs:3` double-matches
(`any(target_family = "unix", trusty, wasi)` at L3 plus a twizzler arm at L5) → duplicate `iovec`.

### Stage 3 — `std::os::unix` over the twizzler PAL (the real work)
`os/mod.rs:84` gates `pub mod unix` on `any(unix, doc)`, so the module must compile for twizzler.
Sized from a tally of every `os::unix::` path referenced across the 408 locally-present crates in
cargo's lock:

| surface | refs | cost |
|---|---|---|
| `io::*` (AsRawFd, AsFd, RawFd, BorrowedFd, OwnedFd, FromRawFd) | ~110 | free — `os/mod.rs:198` already enables `os::fd` for twizzler, and `os::unix::io` is a re-export of it. `os/fd/raw.rs` already gates its generic arm `not(target_os = "twizzler")`. |
| `ffi::{OsStrExt, OsStringExt}` | 51 | free — os_str backend is already `bytes` |
| `prelude::*` | 31 | free — re-export module |
| `fs::symlink` | 14 | one function forwarding to `sys::fs::symlink` (this is the `gix-fs` blocker) |
| `fs::{MetadataExt, PermissionsExt, OpenOptionsExt, FileExt, DirEntryExt}` | ~30 | the only substantial code. `FileAttr::objid()` is a natural `ino`; mode/times already exist |
| `process::{CommandExt, ExitStatusExt}` | 5 | cheap — `pre_exec` closures are already honored by the twizzler spawn path (`do_exec` runs `get_closures()`); `exec` stays an error |
| `net::{UnixStream, UnixListener, UnixDatagram, SocketAddr}` | ~45 | **skip** — only `mio`, `socket2`, `tokio`, `wait-timeout` reference it, none in cargo's non-dev build graph. Gate out with `cfg(not(target_os = "twizzler"))` |
| `thread::JoinHandleExt` | 1 | stub or omit |

`os/unix/mod.rs`'s private `platform` module is a list of per-`target_os` re-exports with no
fallback arm; with no twizzler entry it is simply empty, which is fine unless something references
`platform::*`. Verify during the build.

### Stage 4 — libc fork
`src/ports/libc/src/lib.rs` has `else if #[cfg(unix)]` (~L100) *before* the twizzler arm (L140), and
`unix/mod.rs`'s own target_os dispatch has no twizzler entry (ends in an `else` at L1926). Move the
twizzler arm above the unix arm. The twizzler module is self-sufficient: 4089 lines, 516 `pub fn`,
1903 type/const definitions, covering open/stat/fcntl/poll/mmap/pthread_*/dladdr — which is also
why the direct-libc crates (`rustix`, `nix`, `tempfile`, `same-file`) have a real chance of working.

### Stage 5 — delete the now-duplicate rustc port patches
`rustc_fs_util::path_to_c_string` and `rustc_session::filesearch::current_dll_path` (see above).

### Stage 6 — the OS tree itself
The flip applies to every crate built for the twizzler target, not just the toolchain. 30 files
under `src/` mix twizzler and unix cfgs, in three patterns:

- benign inclusive (`any(unix, target_os = "twizzler")`): `crossterm/src/event/source.rs`,
  `mio/src/sys/mod.rs`
- wrong arm on flip (unix arm first): `src/ports/async-io/src/reactor.rs:38`,
  `src/ports/polling/src/lib.rs:1052`
- likely duplicate definitions: `src/ports/memmap2-rs/src/lib.rs:60-81` (twizzler `path` attr first,
  then a run of bare `#[cfg(unix)]` items)

Plus forks that special-case twizzler *because* unix is off: `filetime` (lib.rs:47), `libloading`,
`rusqlite`, `termion`, `kibi`.

## The failure mode changes character

Today a crate that assumes POSIX fails to compile. After the flip it compiles, links against mlibc,
and either works or fails at runtime — `rustix`'s libc backend under `tempfile`, `nix`, `same-file`,
anything calling `fcntl` locking or `fork` directly. That is the trade we want (runtime bugs are
iterable, compile walls are not), but it converts a loud, complete list into a quiet one discovered
over time.

## Known unknown

Every existing `target_family = "unix"` target routes through `sys/pal/unix`. Twizzler would be the
first to claim the family while keeping its own PAL. Expect stage 2 to surface a second tier of
assumptions that only a real build will show. That is exactly what the stage-1+2 check is for.

## Validation

Cheapest decisive experiment, before touching the OS tree or libc:

```
# scratchpad twzcheck.sh pattern — sets PATH/TWIZZLER_ABI_*/CC_*/CFLAGS_* that xtask normally sets
./twzcheck.sh check library --target x86_64-unknown-twizzler
```

This tells us whether the std side is "eight-arm reorder plus an `os::unix` shim" or something
worse, and it does not install anything — the sysroot is a read-only input.

## Coordination

- Edits so far are confined to `toolchain/src/rust/` (compiler + library sources). The OS build uses
  the *installed* toolchain (`toolchain/install/bin/rustc`), so these edits do not affect a peer's
  `cargo build-all` or a booted guest. They DO affect anyone running `cargo toolchain ports rust`.
- `toolchain/install/` is a shared build input. Nothing here writes to it: `x.py check` does not
  install. If that changes, announce first and report the install mtime.
- The check is a 32-core-capable compile. It must not run while a peer's `--bench` sweep or
  `many.py` lanes are live — concurrent build load inflates bench numbers. Wait for the box to be
  free, and note that a peer watcher is also queued for the same free window.

## Key de-risking fact: one fd namespace

mlibc's twizzler sysdeps implement POSIX descriptors directly on the runtime ABI —
`sysdeps/twizzler/sysdeps.cpp:387` `sys_openat()` calls `twz_rt_fd_open()` and returns its fd
unchanged. So `std::os::fd::RawFd` (= `twizzler_rt_abi::fd::RawFd`) and a descriptor from
`libc::open` are the *same integers in the same namespace*.

This matters because `cfg(unix)` code routinely takes a `RawFd` out of std and hands it to libc
(`fcntl` locking, `poll`, `ioctl`). Had the two namespaces differed, the flip would have created a
silent, runtime-only corruption class across the whole ecosystem. They don't, so it doesn't.

## Considered and rejected: routing std through `sys/pal/unix`

Instead of keeping the twizzler PAL and claiming the family, we could make Twizzler a conventional
unix target: std → mlibc → twz_rt.

Removed (~3.0k lines of Rust): `sys/{fs,env,fd,thread,alloc,random}/twizzler.rs`,
`sys/pal/twizzler/*`, `sys/net/connection/twizzler.rs`, `sys/sync/condvar/twizzler.rs`,
`sys/io/is_terminal/twizzler.rs`, `os/twizzler/*` — 2958 lines measured. Stages 2 and 3 of this
plan vanish entirely: `os/unix` is written against `pal/unix` and would just work. It would also
end the standing cost of re-adding twizzler arms every time upstream reshuffles a `cfg_select!`
(the port currently carries arms in ~25 files).

Added:
- `sys/pal/unix` is 2738 lines with 237 `target_os =` cfg sites; twizzler needs an entry in the
  relevant ones (init/sigaltstack, weak-symbol machinery, os.rs, time, thread_parking).
- mlibc's twizzler sysdeps implement 151 sysdep functions but **24 return ENOSYS**, and the list
  is exactly what std's unix fs backend calls: `mkdirat`, `symlinkat`, `utimensat`, `chdir`,
  `fchdir`, `chmod`, `fchmodat`, `statfs`, `umask`, `fork`. Today's twizzler backend implements
  mkdir, symlink and chdir natively (`twz_rt_fd_symlink`, `twz_rt_set_nameroot`), so this route
  *regresses working functionality* until those sysdeps are written. The work isn't removed, it
  moves from Rust to C++ and lands behind a layer that currently answers ENOSYS.
- `fork` does not exist on Twizzler, so `sys/process` stays bespoke either way — it already has a
  twizzler arm inside `process/unix/mod.rs`.
- An extra indirection on every fs/io call for no functional gain, given fds are already shared.
- Loss of Twizzler-native surface the current backend exposes (`FileAttr::objid()`,
  `os::twizzler::fs`, naming roots).

Verdict: not worth it. The family flip gives the ecosystem everything it actually needs —
`cfg(unix)`, `std::os::unix`, and a libc addressing the same descriptors — without moving std's
implementation off the runtime ABI.

## Progress log

**2026-08-28 ~07:00 — stages 1, 2 and 3 written; not yet compiled.**

Stage 1 (`compiler/rustc_target/src/spec/base/twizzler.rs`): `families: cvs!["unix"]` added, with a
comment stating the unusual shape (unix family, non-unix PAL) and the ordering rule it implies.

Stage 2 (eight `cfg_select!` arm hoists, verified by re-grep): `sys/{pal,fs,env,fd,alloc,io,
net/connection}/mod.rs`. `sys/pipe/mod.rs` needed no change — its twizzler arm resolves to
`mod unix` exactly as the unix arm does. Two follow-on fixes the table didn't predict:
- `sys/io/io_slice/iovec.rs` — the `libc::iovec` arm now excludes twizzler, so `iovec` stays
  `twizzler_rt_abi::io::IoSlice`.
- `sys/fs/mod.rs` — the `with_native_path` fallback was `not(any(unix, windows, wasi))`, which the
  flip turns off for twizzler while the definition it would otherwise get lives in the unix arm we
  no longer select. Now explicitly `any(target_os = "twizzler", not(...))`.

Stage 3 turned out much smaller than sized, because of one discovery: `os/twizzler/mod.rs` already
contains `#[path = "../unix/process.rs"] pub mod process;`. Upstream's unix `process.rs` therefore
*already compiles for twizzler today*, and `sys/process/unix/common.rs` is shared and already
provides `uid`/`gid`/`groups`/`chroot`/`setsid`/`get_closures`. So `os::unix::process` needs no
work at all. What was written:
- `os/unix/twizzler_fs.rs` (331 lines) — `FileExt` (over `twz_rt_fd_pread`/`pwrite`),
  `PermissionsExt`, `OpenOptionsExt`, `MetadataExt`, `FileTypeExt`, `DirEntryExt`, `DirBuilderExt`,
  `symlink`. Absent-field policy is stated in the module header: fixed values, not fabricated ones.
- `os/unix/mod.rs` — `fs` swapped for the above; `net`, `raw` and `thread` gated off for twizzler
  (no AF_UNIX; `raw` needs `platform::raw` which has no twizzler arm; `JoinHandleExt` would have to
  invent a `pthread_t`), and the prelude's `JoinHandleExt` re-export gated to match.
- `sys/fs/twizzler.rs` — added `FilePermissions::{mode, from_mode}`, the only new sys-side surface.

### Policy: I/O and file paths stay off libc; other gaps may use mlibc

Stated by the owner, 2026-08-28: std should keep calling `twz_rt_*` directly for **I/O and file
functions**; for things that are currently *unsupported*, routing through mlibc is fine and gaining
functionality that way is welcome.

The flip's real risk is therefore not compile errors — it is arms that previously fell to
`unsupported` and now silently land on a libc-backed unix arm. Every such site was checked, not
just the ones that fail to build. Two exist, and under the policy above both are allowed to take
the libc path:

- `sys/net/hostname/mod.rs` — now uses the unix arm (`libc::gethostname`). This is a functional
  gain: mlibc's `sys_gethostname` is `strncpy(buf, "twizzler", bufsize); return 0`, so the call
  succeeds on the first pass and `hostname()` returns "twizzler" where it previously returned
  `Unsupported`. The errno-based retry branch is never taken, which matters because
  `pal::twizzler::os::errno` always returns 0 — if mlibc ever *does* fail here, the error surfaces
  as raw OS code 0, whose message is "operation successful". Wart, not a hang.
- `sys/thread_local/mod.rs` `key` module — now matches the generic unix arm and compiles
  `key/unix.rs` against libc's `pthread_key_*` (all four declared in the libc fork, `pthread_key_t`
  at `src/twizzler/mod.rs:24`). Dead code on this target: `key` is only reached when
  `!target_thread_local`, and Twizzler sets `has_thread_local`. Left matching upstream because
  divergence costs rebase debt and this buys nothing either way.

Checked and confirmed clean — the I/O and file path takes no new libc branch: `sys/fs`, `sys/fd`,
`sys/io/{is_terminal,io_slice}`, `sys/pipe`, `sys/stdio`, `sys/process/unix/*` are all either
twizzler-first arms or inclusive `any(unix, ..., twizzler)` lists that already included twizzler,
so nothing inside them flips. `sys/pipe/unix.rs` has a twizzler arm calling
`twz_rt_fd_open_pipe`; the `libc::pipe`/`pipe2` calls live in arms this target does not take.
`os/unix/twizzler_fs.rs`'s `FileExt` goes to `twz_rt_fd_pread`/`pwrite`, where upstream's unix
`FileExt` would have called `libc::pread`.

### Follow-up (pre-existing, not caused by the flip): `read_output` polls through libc

`sys/process/unix/common.rs::read_output` is shared code that Twizzler compiles *and runs today*:
it builds `libc::pollfd`s from twz_rt descriptors and calls `libc::poll` directly. It works only
because mlibc's descriptors *are* twz_rt descriptors, and it is demonstrably exercised on Twizzler
— the function already carries a Twizzler-specific comment about `(category << 16) | code` error
packing breaking its `EWOULDBLOCK` comparison, hit via rustc's own `Command::output` linker call.

Under the policy above this wants a twizzler arm over the runtime's own readiness primitive
(`twz_rt_fd_waitpoint`) instead of `libc::poll`. Deliberately **not** part of this change: it is
independent of the family flip, it sits in a path rustc depends on for linking, and it should be
made and validated on its own.

All 13 edited files parse (`rustfmt --check`, syntax only). Nothing has been compiled yet.

### Mechanical arm audit across all rust libraries

Two scripts (kept in the session scratchpad, `armaudit.py` / `attraudit.py`) rather than eyeballing:
one brace-matches every `cfg_select!`/`cfg_if!` block under `library/` and reports blocks where a
unix-matching arm precedes the twizzler arm (or where twizzler has no arm at all); the other scans
`#[cfg(...)]` *attributes* for three hazards — the same item defined under both a unix and a
twizzler predicate (duplicate definition), `#[cfg(not(unix))]` items twizzler silently loses, and
newly-active libc imports. Scan root: `library/`, 2352 `.rs` files. Both are sensitive by
demonstration: they found the bugs below when present and report zero once fixed.

Three real arm-order bugs, all now fixed:

1. **`libc/src/lib.rs`** — the `else if #[cfg(unix)]` arm sat *before* the twizzler arm, and libc's
   `unix` module dispatches per `target_os` with no twizzler entry (its chain ends in a bare
   `else` at `unix/mod.rs:1926`). The twizzler arm is now first. This is the crate std links
   against; note `library/libc` and `src/ports/libc` are two checkouts of the same fork, one commit
   apart (`0c139af7` vs `09ea9a75`).
2. **`std/src/sys/pipe/mod.rs`** and **`std/src/sys/process/mod.rs`** — unix arm first, but both
   arms resolve to `mod unix`, so behaviour was identical. Hoisted anyway: it keeps the twizzler
   arm authoritative if upstream ever adds a unix-only re-export, and it leaves the audit clean
   rather than carrying two known-benign hits that the next person has to re-triage.

Two real compile/behaviour errors, found only by the attribute scan:

3. **`libc/src/new/mod.rs:211`** — `if #[cfg(target_family = "unix")] { ... pub use unistd::*; }`.
   The per-target dispatch above it imports no platform module for twizzler, so `unistd` has
   nothing to resolve to: an unresolved-import error the moment the family is claimed. Excluded.
4. **`std/src/sys/fs/mod.rs`** — `set_permissions_nofollow` is `#[cfg(all(unix, not(vxworks)))]`
   with a `#[cfg(any(not(unix), vxworks))]` `unimplemented!()` fallback. The flip moves twizzler
   from the fallback onto the libc body, which sets `O_NOFOLLOW` via `custom_flags` (ignored by the
   twizzler `OpenOptionsExt`) and ends in `set_permissions` (a no-op here) — so it would report
   success having neither refused to follow the symlink nor set anything. Twizzler now stays on the
   honest `unimplemented!()`.

Checked and confirmed harmless: `backtrace` (its `gimli.rs` dispatch is a `target_os` list, so the
twizzler arm is still reached), `unwind` (twizzler sits *inside* the same `any(unix, windows, ...)`
arm), `panic_unwind` and `panic_abort` (inclusive lists), `std_detect` and `profiler_builtins` (no
unix cfgs at all). `library/test`'s `test_result.rs` flips from its `not(unix)` arm to the unix one:
it compiles (`os::unix::process::ExitStatusExt` exists via upstream's `process.rs`), the signal arm
is unreachable because twizzler's `ExitStatus::code()` never returns `None`, and the unknown-code
message gets more informative. `std/src/sys/backtrace.rs::output_filename` likewise moves to the
unix match arm, which is an improvement — `OsStr::from_bytes` instead of a lossy UTF-8 conversion —
and reaches no libc.

Attribute scan residue, all explained: 4 "duplicate" hits are one attribute containing both `unix`
and `twizzler` in a single `any(...)` list; the one "newly-active libc import" is
`sys/stdio/unix.rs:3`, which already included twizzler before the flip and imports constants
(`EBADF`, `STD*_FILENO`), not calls.

### Same audit over `compiler/` (2065 files)

`UNIX-FIRST: 0`. Four findings, all triaged:

1. **`rustc_fs_util::path_to_c_string`** — duplicate definition once the family is claimed (generic
   `any(unix, wasi p1)` body plus a twizzler one). **Deleted the twizzler copy**; the generic body
   uses `os::unix::ffi::OsStrExt::as_bytes()`, which now works here.
2. **`rustc_session::filesearch::current_dll_path`** — same duplication. **Deleted the twizzler
   stub** (which returned `Err` with a FIXME). The unix body calls `libc::dladdr`, and both
   `dladdr` and `Dl_info` are declared in the twizzler libc module, so this is a potential
   *improvement* to sysroot detection; if dladdr does nothing useful at runtime it returns
   `Err("dladdr failed")`, which is exactly what the stub returned.
3. **`rustc_data_structures::flock`** — twizzler moves from the `_ => unsupported` arm onto the
   unix `fcntl` implementation. Left as-is, deliberately, with the semantics written down here:
   mlibc's `sys_fcntl` handles `F_SETLK`/`F_SETLKW` explicitly and logs *"accepting lock without
   effect"*, and `libc::flock` plus the constants exist — so it compiles and **succeeds without
   locking**, where the fallback returned an explicit `Err` whose `error_unsupported()` told the
   caller no locking was available. Same end state (no mutual exclusion), different knowledge:
   rustc now believes it holds a lock it does not hold. This matches what std already does here
   (`sys::fs::twizzler::File::lock` is `Ok(())`), which is why it is left alone — but it is a
   one-line revert (`target_os = "twizzler" => { mod unsupported; ... }`) if the silent version is
   judged worse than the honest one.
4. **`rustc_data_structures::profiling::get_resident_set_size`** — moves to the unix arm, which
   reads `/proc/self/statm`; that read fails on Twizzler and `.ok()?` yields `None`, i.e. exactly
   the fallback's behaviour. No libc, no change, left alone.

`rustc_codegen_gcc`'s `not(unix)` hit is in its build system, which is not built here.

### Deferred: the second libc checkout

`src/ports/libc` needs the same two fixes as `library/libc`, but it is currently **clean**, so
editing it flips the parent repo's `many.py source_fingerprint()` and would trip a peer's
TREE CHANGED check mid-build. twizzler-24 is building as of this writing, so that edit waits for
the same window as the compile. `library/libc` is the one std links, so nothing blocks validation.

## Coordination outcome (2026-08-28)

Confirmed by pid, not inferred: `netfix1` sysbench (8 rounds) belongs to twizzler-db, finishing
~07:15. The sustained-free-window watcher belongs to twizzler-24, whose own arm is build + 8 rounds,
~07:15 → ~08:05; they will ping when their sweep exits (checking for a live qemu, not just the
driver). My compile window is after that — 08:15 as a fixed fallback.

Two things worth keeping from that exchange:
- A "no qemu and no many.py" detector cannot distinguish *free* from *between the arms of someone's
  paired A/B*. Hand off by explicit ping; a detector can only say the box is idle.
- `pgrep -x qemu-system-x86_64` never matches (`comm` truncates at 15 chars), and `pgrep -f` can
  match the watcher's own command line. `ps -eo comm | grep -c '^qemu-system'` avoids both.

`toolchain/src/rust` is a git submodule that was *already* dirty before these edits, so the parent's
`many.py source_fingerprint()` does not change further and no peer's TREE CHANGED check trips.
Committing inside the submodule *would* move the pointer and change the parent diff — so that waits
until nobody is mid-build.

## Stage 6 work list (OS tree, `src/` — surveyed read-only, 1719 files)

Not edited: `src/` is outside the already-dirty submodule, so touching it flips the parent's
`source_fingerprint()` and would trip a peer's TREE CHANGED check mid-build. This is the list to
work from once the std side validates and the box is free.

**Arm order (4):** `ports/async-io/src/reactor.rs:~25`, `ports/libc/src/lib.rs:~51` (same fix as
`library/libc`), `ports/polling/src/lib.rs:~83` and `~1051`.

**Genuine duplicate definitions (2)** — separated from 45 same-attribute false positives by
comparing line numbers, since `any(unix, target_os = "twizzler")` is the dominant idiom in these
forks and is harmless:
- `ports/mio/src/lib.rs`: `mod unix` at unix@76 *and* twizzler@92.
- `ports/rust_libloading/src/safe.rs`: `use super::os::unix` at twizzler@3 *and* unix@7.

**`not(unix)` items twizzler loses (9)**, of which the ones that matter:
- `ports/signal-hook/src/iterator/mod.rs` (4 items) and `low_level/pipe.rs` — these flip to the
  unix implementations, which use `std::os::unix::net::UnixStream`. **That module is gated off for
  twizzler in this plan.** This is the one place where stage 6 could force the AF_UNIX decision
  back open; the alternative is carrying more patches in the signal-hook fork. Worth deciding
  deliberately rather than discovering at link time.
- `ports/rusqlite/src/lib.rs:341` `path_to_cstring` — flips to the unix body, which is the same
  `OsStrExt::as_bytes` shape that now works. Expected to be an improvement.
- `bin/test-tiny-http/.../connection.rs:170` `to_unix` — check when reached.
- `ports/async-io/examples/unix-signal.rs` and `rust_libloading/src/os/unix/consts.rs`
  (`libloading_docs`-gated) are not built.

## Aborted first attempt (2026-08-28 07:31:50 - 07:34)

Started `x.py check library --stage 1 --target x86_64-unknown-twizzler -j 20` on the reading that
twizzler-24's timing arm had not begun: twizzler-db had explicitly released their own arm (a
per-round boolean, insensitive to CPU contention) and the box showed exactly 4 qemus, matching
their `-j4`. It had begun 23 seconds earlier and was in `cargo xtask make-image`.

**A sweep in its build phase presents zero guests**, so a guest-count check cannot distinguish
"not started" from "started and compiling" — and that blind window sits exactly where a peer is
most likely to be starting. Loadavg was 12.28 with only 4 guests up, which was already the answer;
the cheap instrument and the correct one disagreed and I read the cheap one.

Killed on request within ~2.5 min and *verified* dead rather than assumed: `ps -eo pid,ppid,etimes,
args` showed no surviving `x.py`/`bootstrap`/`cargo`/`rustc`. That check matters because stopping
the parent is precisely the case that leaves children running (twizzler-db lost a driver this
morning and kept 4 qemu + 4 xtask alive).

Related gap worth fixing at the source: `--label` is not propagated to qemu's command line, so a
guest cannot be attributed to a sweep by argument matching at all — only dated with
`ps -eo etimes`. Adding the tag to the qemu invocation would remove a whole class of guesswork.

Next window: twizzler-24 messages when `phase0828` lands, ~08:20.

## Next step

```
./twzcheck.sh check library --stage 1 --target x86_64-unknown-twizzler
```

Stage 1 matters: the family flip lives in the compiler, so std must be checked with a freshly built
in-tree rustc, not stage0's. That is the ~20-40 min part (touching `rustc_target` rebuilds most of
rustc; LLVM is already built in the 49G build dir).

Fallback if the rustc rebuild proves painful: take `--print target-spec-json` from the installed
rustc, add `"target-family": ["unix"]`, and check std against the modified sources via
`-Zbuild-std` + `__CARGO_TESTS_ONLY_SRC_ROOT`, which validates the std side without rebuilding the
compiler at all.

## Incident: the submodule pointer *is* the toolchain name (2026-08-28 08:07-08:15)

Committing the submodule pointer bump in the parent repo (`175c20d8`) broke every Twizzler build on
the box within a minute, and neither I nor two peers saw it coming.

`tools/xtask/src/toolchain/pathfinding.rs` builds the toolchain directory path as
`"toolchain/" + generate_tag()`, and `generate_tag()` hashes the submodule OIDs. So bumping the
pointer *renames the directory xtask looks for*. With only `toolchain_a61176b-…` on disk, every
build failed with "There doesnt exist a local toolchain capable of building Twizzler!" — a metadata
commit turned into a box-wide outage. twizzler-24 lost three `cache8k` builds to it.

Three facts worth keeping:

- **`generate_hash` reads `Submodule::head_id()`, i.e. the OID recorded in the *parent's* HEAD
  tree** — not the submodule's checked-out HEAD. Committing inside the submodule alone does not
  move the tag; only a parent-side commit does. This also means the obvious workaround
  (`git -C toolchain/src/rust checkout <old>`) does nothing at all.
- **With an unchanged tag, `mover.rs` does `remove_dir_all(dest)` + `cp -R` into the *live*
  toolchain.** So the choice is between an outage (bump the pointer, no toolchain until the build
  lands) and an in-place overwrite of the toolchain everyone is using, with no rollback. There is
  no third option that is both safe and invisible.
- **A killed bootstrap leaves a partial directory that xtask reports as `Active`.** After restoring
  the pointer, `toolchain list` marked an incomplete 3.7 GB tree as the active toolchain: present
  by name, unable to build, with no "not found" message to explain it. Moved to
  `toolchain/partial-2e87390-killed-0813/`. A stopped box beats a silently wrong one.

**Provenance caveat (twizzler-db's, and it applies to the toolchain now building):** because the tag
comes from the *recorded* pointer, a toolchain built from a dirty submodule working tree is named
for the clean OID. `toolchain_2e87390-…` will contain, in addition to that commit, ~70 files of
other sessions' uncommitted work in `toolchain/src/rust`. The directory name asserts a provenance
its contents do not have, and nothing downstream can detect it.

Also learned, at cost: a destructive edit and its undo must not share a command's lifetime. A
control run structured as `cp backup; edit; build; restore` was killed by a 2-minute timeout
*before its restore*, leaving the tree with the change under test deleted. Nothing errored, and
every later build would have validated the baseline while being reported as the treatment.

### Readiness has three proxies and all of them fire early

During the bootstrap, three separate signals said "toolchain ready" hours before it was:

1. **The directory existing.** A killed bootstrap's partial tree made `xtask toolchain list` report
   it `Active` — present by name, unable to build.
2. **`toolchain/install` pointing at it.** The bootstrap re-points `install` to its *in-progress*
   output directory at **startup** (symlink mtime = build start), not at completion. I told both
   peers it stays on the old tag until the swap; that was wrong, and one of them had already keyed
   a watcher on the flip. It would have fired ~90 seconds into a multi-hour build.
3. **`bin/rustc` existing.** LLVM tools and rustc are installed long before std and the sysroot, so
   a file check passes while nothing can actually be compiled.

The install pointer asserts a readiness its contents do not have for the whole build window, and
that is a property of `bootstrap`, not of the pointer-bump commit. The only signal that does not
fire early is compiling something real and seeing it succeed — which is what the completion ping
should wait for.

Method trap hit while verifying this: `install/bin/rustc --version | head -2; echo rc=$?` reports
**rc=0 for a binary that does not exist**, because `$?` is `head`'s status. Read exit codes
directly, never through a pipe — the same shape as the background task that reported "exit code 0"
while the log said `EXIT=127`.

## Method: recovering evidence from a boot failure

Two techniques from 2026-08-28 that generalise past the bugs they found. Both were available from
the first failing run; we reached for them last.

### Symbolize the fault dump before auditing source

The kernel's `fault-diag` prints the faulting rip, the instruction bytes, and eight frame return
addresses. That is enough to name the failing function and its whole call chain, with nothing but
the built binary:

1. **Identify the module** by byte-matching the dumped instruction bytes against candidate
   binaries — `grep -obUaP "$(printf '\x48\x3b\x48\x08...')" libtwz_rt.so` — which yields a file
   offset.
2. **Derive the load base**: map that file offset to a vaddr through the `readelf -l` LOAD
   segments, then `base = rip - vaddr`. In the 8c bug: vaddr `0x98001`, rip `0xc80098001`, so base
   `0xc80000000`. Both numbers were already on screen.
3. **Name every frame**: `llvm-nm --defined-only --numeric-sort <module>`, taking the greatest
   symbol address `<=` each `(frame_rip - base)`.
4. **Get the exact call site**: `llvm-objdump -d --start-address=<sym> --stop-address=<sym+n>` and
   read the `callq` just before the return address.

Why this ordering matters: a source audit can only find bugs of the shape you are auditing for. The
cfg audits run against 8c were complete *for cfg bugs* and the cause was an ordinary
`std::env::var` call, so "cfg audit clean" carried no information — not about the flip, not about
anything. A fault dump has no such shape restriction; it names the path actually taken.

### Before believing a ratio, check that both halves share a gate

Two failures of the same shape, both found on 2026-08-28 (credit: twizzler-24):

- **Ratios hide their own sample size.** A treatment round read 0.500 against a predicted 1.00 from
  a 1.937 baseline, with the treatment counter at zero — everything said the fix had landed better
  than hoped. The log was live and the round was 20 of 40 benches in. What exposed it was the raw
  numerator and denominator printed beside the ratio: **2 and 4**, against an expected 14,469.
  `0.500` is plausible; `2/4` is absurd on sight. Always read the raw pair.
- **Numerator and denominator must be sampled by the same gate.** `FSLOCK` fires when a running
  total crosses a power of two (`ext4.rs:969`); `PHASESTATS` fires on a 2-second timer. Dividing
  one by the other yields a dimensionless number that means nothing — and it had already generated
  a confident secondary finding (an apparent bimodality that was really 2^15-vs-2^16 quantisation
  in the *reporting*), nearly producing a "fix" to working code.

The metric **passed its pre-registered sanity check** ("if the control doesn't reproduce ~1.94 the
A/B isn't interpretable"; it returned 1.928). It reproduced because control behaviour is
deterministic — the check confirmed determinism, not commensurability. **A pre-registered threshold
only excludes the failure it names.**

Resolution was the ordering argument above, applied one level down: stop inferring a report's
cadence from the pattern of its values and read the code that emits it. Ask "what triggers this
line?" of the source, not of the data.

### Recover a pre-change binary from an old boot image

Old images under `boothang-work/masters-*/` still contain their initrd, and it can be pulled
straight out of the EFI partition without booting anything:

    mcopy -i <image>@@$((34*512)) ::initrd  /tmp/initrd && tar xf /tmp/initrd

That makes before/after ELF diffs possible at all — `DT_NEEDED` lists, undefined-symbol sets,
`PT_TLS` sizes. It is what established that the pre-flip monitor had the *same three* NEEDED
entries and only one undefined `_Unwind_` symbol, killing the "a DT_NEEDED went missing" theory
that two of us had been reasoning from. (Credit: twizzler-24.)

## Stage 7 — porting `rustix` and `nix` (2026-08-28)

Decision (user): do real ports rather than dodge the crates by disabling features. Submodules:
`src/ports/rustix` (branch `twizzler`, 1.1.4) and `src/ports/nix` (upstream `master`, 0.31.2).

### Why a feature flag was never going to work

Checked both before starting, because "just use their libc backend" is the obvious first idea:

- **`nix` has no backend to choose.** Its only real dependencies are `libc`, `cfg-if`, `bitflags`;
  every function is a thin safe wrapper. The failure is that `errno.rs` holds twelve hand-written
  `mod consts` blocks in one `cfg_if!` chain, one per OS, each enumerating that OS's errno values
  as an `Errno` enum — with **no fallback arm**. On twizzler the chain matches nothing, so
  `pub use self::consts::*` has nothing to resolve.
- **`rustix` has a libc backend and we are already on it.** The raw-syscall backend is gated on
  `target_os = "linux"`, so every non-linux target falls through to `backend/libc` automatically;
  `use-libc` / `rustix_use_libc` only *force* it on linux. The problem is that the libc backend is
  itself per-OS (`backend/libc/c.rs` alone has dozens of `target_os` arms, plus `linux_like`/`bsd`/
  `solarish` aliases synthesised by its `build.rs`), and twizzler is in none of them.

### Who actually pulls them (this sets the scope)

| crate | reached via | side |
|---|---|---|
| `rustix` 1.1.x | `terminal_size` <- `clap_builder` (wrap_help) + `miette` | **target** |
| `rustix` 0.38 | `gix-index`, `gix-prompt` <- cargo/xtask | host only — stays on the registry crate |
| `nix` 0.30.1 | `os_info` <- `cargo` | host only |
| `nix` 0.29 | `uucore`, `uu_cat`, `uu_sort`, `uu_yes`, `ctrlc` <- uuhelper | target, **only when uuhelper returns** |
| `tempfile`, `xattr` | cargo, tar, reqwest | host only |

So rustix was the live blocker and nix is not currently in the target graph at all.

### rustix port — done, compiles

Whole surface was 18 errors, three causes. **Sixteen of them were a bug in our own libc port**, not
in rustix: `NL0/NL1`, `CR0..CR3`, `TAB0..TAB3`, `BS0/BS1`, `FF0/FF1`, `VT0/VT1` were declared
`c_int`, but they are `c_oflag` bits and `tcflag_t` is `c_uint`. `XTABS` — same family, same field,
same file — was already `crate::tcflag_t`, which is what proves the intent. Retyped all sixteen.

The other two:
- `ttyname_r` was undeclared in our libc; mlibc provides it
  (`options/posix/generic/unistd.cpp:968`), so the declaration was added next to `ptsname_r`.
- `ioctl/mod.rs`'s `_Opcode` needed a twizzler arm: `c::c_int`, matching mlibc's
  `int ioctl(int, int, ...)`.

Plus one manifest change inside the fork: rustix 1.1.4 requires `libc = "0.2.182"` and our libc
port is 0.2.178, relaxed to match — see the trap below, which this caused.

### Trap: `cargo update` silently evicts a path patch

Adding the rustix patch and running `cargo update -p rustix@1.1.2` printed, inside a wall of
output, `Removing libc v0.2.178 (src/ports/libc)` / `Adding libc v0.2.189`. A `[patch]` replaces a
registry crate **only for requirements its own version satisfies**, so rustix's `>=0.2.182` fell
through to the registry, and because a path patch is a distinct source the two libcs coexisted
instead of unifying. Our entire libc port left the graph. The symptom was an error inside
`libc-0.2.189/src/new/mod.rs` at code I had already fixed in `src/ports/libc/src/new/mod.rs` —
which reads as "my port is broken", not "my port is not being used".

**The path in the error message is the evidence.** If it points into `~/.cargo/registry/` for a
crate that has a `src/ports/` entry, stop debugging the code and go read the resolution: check
`[[patch.unused]]` in `Cargo.lock` and grep for two versions of one crate name. Note `cargo tree -i`
re-resolves the lock as a side effect, so verify the lock in the *same* command that fixes it.

### Four more crates fell out of the same flip

- **`libloading`** (`as_filename.rs`): `#[cfg(unix)] let bytes = ...` and
  `#[cfg(target_os = "twizzler")] let bytes = ...` as sibling statements. Before the flip exactly
  one applied; now both do, so `self` moved twice (E0382). Narrowed the unix arms.
- **`async-net`**: the submodule existed but **was never patched in**, so the registry copy was
  being used. Added the `[patch]` entry and gated its `unix` module (unix-domain sockets) off.
- **`memmap2`**: `#[cfg_attr(unix, path = "unix.rs")]` *and*
  `#[cfg_attr(target_os = "twizzler", path = "twizzler.rs")]` both applied — two `path` attributes
  naming **different files**, with only a deprecation warning to show for it. The hand-written
  twizzler backend could be bypassed in favour of `unix.rs`. Narrowed the unix arm. **Unverified:**
  the `memmap2` patch reports "not used in the crate graph" in every collection (its only reverse
  deps are `gix-*`, i.e. cargo/xtask on the host), so nothing here builds it for twizzler. The fix
  is correct and preventive; it has never been compiled on this target.
- **`rust-errno`**: same duplicate-`path` shape, but both arms named `unix.rs`, so behaviour was
  unchanged — still fixed, since the duplicate attribute is slated to become a hard error.

The libloading and memmap2 cases are a gap in the earlier audit: it checked `cfg_select!`/`cfg_if!`
chains and duplicate *definitions*, but not **sibling `#[cfg(unix)]` + `#[cfg(target_os =
"twizzler")]` attributes**. `scratchpad/siblingaudit.py` covers that shape; it found exactly these.

### Two manifests, not one

`third-party/elephance/twz/Cargo.toml` is a **separate ephemeral workspace** with its own
`[patch.crates-io]` mirroring the root's ("Keep in sync with root Cargo.toml"). A root-only change
resolves fine for four collections and then fails in `third-party`. `rustix`, `async-net` and the
nix version all had to be added there too. (It also lists a `memmap2` patch, but that patch is
reported unused -- nothing in the collection depends on memmap2.)

### Unrelated landmine this surfaced: `bitflags = "*"`

`object-store` declared a wildcard, and the re-resolves moved it 2.9.1 -> **1.3.2**. bitflags 1.x
generates its own `Clone`/`Copy`, which collide with the 2.x-style `#[derive]` the code is written
against (7 x E0119). Pinned to `2` there and in `twizzler-net`, the only other `bitflags = "*"`.
Every other bitflags dep in the tree was already pinned to 2.x. The other wildcards in the tree are
pre-existing style and only bite where two majors coexist, so they were left alone.

### Out of the workspace, to be restored

- `src/bin/uuhelper` — pulls `nix` and (via `terminal_size`) `rustix`.
- `src/bin/debug` — pulls `gdbstub`, whose `conn/impls/mod.rs:11` gates `mod unixstream;` on bare
  `unix` and so imports `std::os::unix::net`. One line
  (`#[cfg(all(feature = "std", unix, not(target_os = "twizzler")))]`) but gdbstub has no port yet.

Neither is in the initrd (`#"crate:uuhelper"`, `#"crate:debug"`), so nothing ships differently —
but both must come back before the flip is called done.

### nix — decided, not yet started

Version choice matters and is not obvious: **coreutils pins `nix = "0.29"`**
(`twizzler-operating-system/coreutils`, root `Cargo.toml:312`), while the submodule is now on
upstream `master` at 0.31.2, and 0.x minor bumps are breaking — so 0.31.2 cannot satisfy a `^0.29`
request. The 0.30.1 in our lock is `os_info <- cargo`, host-side, and irrelevant.

Decision (user): **stay on 0.31.2 and bump coreutils.** That requires a change in the coreutils
fork, which cannot be pushed from here:

> in `twizzler-operating-system/coreutils`, branch `twizzler`, root `Cargo.toml:312`:
> `nix = { version = "0.29", default-features = false }` -> `"0.31"`

Until that lands, `nix` stays an unused patch and nothing is blocked, because nix is host-only
today. Port scope when it starts: `errno.rs` needs a twizzler arm (an `Errno` enum plus an
`errno_location()`), `build.rs` sets no family alias for us (`bsd`/`linux_android`/`solarish`/
`apple_targets` all false), and uucore additionally enables `fs`, `uio`, `zerocopy` and `signal`,
each with its own per-OS code — so expect a considerably larger surface than rustix's 18 errors.

## Stage 8 — boot: three bugs, all fixed (2026-08-28)

`cargo build-all --profile release` exiting 0 proved compilation, not boot. **Three** separate
failures sat behind it, each masking the next: the image would not assemble, then the monitor would
not link at load, then init faulted before starting a server. Final state: `unixsmoke4` passes
57/57.

### 8a. `make-image` could not stage `libunwind.so` — FIXED

Not a compile error. The active toolchain's sysroot was missing exactly one file, and
`xtask/src/image.rs:531` hardcodes `"libunwind.so"` in the initrd list.

Cause: `bootstrap --step rt` does not run the step that produces `libunwind.so`.
`build_libunwind` in `bootstrap/llvm.rs` only ever compiles and installs libunwind**.a**; the `.so`
comes from the rust step (`llvm-libunwind = "in-tree"`, `ports/rust.rs:178`). `mover.rs` does
`remove_dir_all` + `cp -R`, so a partial step replaced the sysroot wholesale and dropped the file.

Restored by copying from `toolchain_a61176b-5d603b4-8511b4b`, justified by content rather than by
name: the two tags differ only in the rust component, `libunwind.a` is byte-identical
(md5 `df23f73a...`) across both toolchains and the pre-bootstrap backup, and the two `.so` copies
are byte-identical (`73699a41...`). The two sysroots now list identical files.

**The restored file keeps its source mtime (2026-08-25), so it is invisible to any `-newermt`
audit run today.** A build input changed in a way that cannot be dated by timestamp — the same
shape as a const flipped before a build window.

### 8b. `monitor::monitor: needed symbol _Unwind_GetIP not found` — FIXED, cause not fully explained

Four symbols, not one: `_Unwind_GetIP`, `_Unwind_GetCFA`, `_Unwind_FindEnclosingFunction`,
`_Unwind_Backtrace`. The monitor imports five `_Unwind_*`; libstd exports exactly one
(`_Unwind_Resume`) and libc exports none, so the four that fail are precisely the four with
nowhere to resolve from. That tight mechanism/symptom fit is what made this a diagnosis.

Only `libunwind.so` defines them, and **nothing declared a `DT_NEEDED` on it**: the dynamic
collection's rustflags (`toolchain/mod.rs:272`) pass `--allow-shlib-undefined` and
`--warn-unresolved-symbols` but never `-lunwind`. Only `set_static()` (line ~293) and
`ports/rust.rs` do. So the link succeeds and the failure appears at load time.

Fix: mirror that pairing into the dynamic collection —
`-C link-arg=--no-as-needed -C link-arg=-lunwind`, trailing so it lands after rustc's own
`--as-needed`. The monitor now records `NEEDED: libunwind.so` and the errors are gone.

**Two hypotheses died on the way, both worth keeping:**
- *"Artifacts were linked before the file came back."* False. The restored `.so` has ctime
  10:06:32; a monitor linked at 10:09:41 still recorded no `DT_NEEDED`, and deleting the artifact
  and relinking reproduced that exactly. A plain rebuild was never going to fix it.
- *"A `DT_NEEDED` went missing."* Also false, and this is the part that is still unexplained. An
  Aug 23 pre-swap monitor (recovered from `boothang-work/masters-*/` via
  `mcopy -i img@@$((34*512)) ::initrd`) has the **same three** NEEDED entries and **only one**
  undefined `_Unwind_` symbol — `_Unwind_Resume`. So there was no record to lose. The real change
  is that the monitor now *references four unwinder functions it did not reference before*. The
  fix supplies them; it does not explain them. Do not treat 8b as closed.

Note `backtrace/mod.rs:184` already names `target_os = "twizzler"` inside the `any(...)` selecting
`mod libunwind`, so module selection is not gated on unix — that specific mechanism is refuted,
which is not the same as refuting the flip as the cause.

### 8c. Userspace wedge — FIXED. `std::env::var` in the allocator OOM path

With 8a and 8b fixed the guest gets much further: kernel tests run and `test result: ok.`,
userspace starts and spawns ~1350 threads, then **every thread is `Sleeping` and no test report is
produced**. The round dies at the 5m22s silence timeout (exit 34) and the log ends in the
watchdog's thread dump. This is a hang, not a link failure.

(The `panic::backtrace` frames in that log are a red herring — the `nonleaf_cow_arm_is_reached`
kernel test deliberately prints a backtrace and reports `ok` immediately after.)

Lead: a pre-swap/post-swap diff of the monitor's undefined symbols shows 62 newly undefined, 35
gone, and the new ones are std's **unix backend arriving** — `pthread_mutex_*`, `pthread_cond_*`,
`pthread_condattr_*`, `tcgetattr`, `tcflush`, `tcdrain`, `tcflow`, `tcgetpgrp`, `tcgetsid`,
`ioctl`, `isatty`, `clock_gettime`, `__errno_location`, `strerror_r`, plus
`std::sys::fs::canonicalize`, `std::path::*`, `std::io::stdio::stdin`.

Checked and **cleared**: all five sync backends (`sys/sync/{mutex,condvar,rwlock,once,
thread_parking}/mod.rs`) already put the twizzler arm above the unix arm, so std's locks are still
on the futex path, not pthreads.

A full audit of every `cfg_select!`/`cfg_if!` selector under `library/std/src/sys` leaves these:

| file | state |
|---|---|
| `sys/thread_local/mod.rs:147` (`key` module) | **best lead.** twizzler now matches the unix arm, so `mod unix;` compiles and pulls in `pthread_key_create`/`setspecific`. Pre-flip it fell through to `_ => {}`. `has_thread_local: true`, so the native TLS path is what actually runs — this explains the pthread imports but is **not proven** to cause the wedge. |
| `sys/path/mod.rs:1` | `mod unix;`, no twizzler arm. Probably desirable (unix path semantics), unaudited. |
| `sys/net/hostname/mod.rs:1`, `sys/net/connection/socket/mod.rs:16` | unix arm, no twizzler arm. Newly reachable. |
| `sys/args/mod.rs:17` | **false positive** — twizzler is inside the same `any(...)` as unix, both select `mod unix`. |
| `sys/pal/unix/weak/mod.rs:21` | comment text, not a selector. |

Next step is to work that table, not to re-run the smoke test. Note that testing any std change
requires rebuilding and installing std, and a *partial* install is exactly what caused 8a — so
that rebuild wants supervision.


### 8c, resolved

`unixsmoke4`: **57/57 tests passed, 0 failed, exit 0**, 13s run. Zero `_Unwind_` errors, zero
unhandled faults, zero supervisor exceptions; full REPORT JSON with 57 named binaries all
`Passed`, including `net_test`, `net_srv` and `stdnet_test`.

It was neither the flip nor the toolchain. Symbolizing the `fault-diag` return addresses gave the
call chain outright:

    twz_rt_malloc -> ReferenceRuntime::alloc -> LocalAllocator::alloc_early
      -> talc::malloc -> RuntimeOom::handle_oom -> talc::create_and_map
      -> twz_rt_tls_get_addr    <-- #GP

`create_and_map` (`src/rt/reference/src/runtime/alloc/talc.rs:223`) called
`std::env::var("MONDEBUG")` **inside the allocator's OOM handler**. That allocates and reads a
thread-local, and it can run before the runtime has set up TLS. `twz_rt_tls_get_addr` does
`movq %fs:0x0, %rax`; with no thread pointer that returns garbage, the null check at +9 passes
because the value is merely non-zero, and `cmpq 0x8(%rax), %rcx` at +17 faults on a non-canonical
address. The garbage value was `0x48ff3148e5894855` — bytes `55 48 89 e5 ...`, a function prologue,
i.e. it had read code as data.

That file's own module doc says it must "avoid calling into std" precisely because it allocates
before the runtime is ready, so the call violated the invariant the file is built on. Fix: consult
`MONDEBUG` at most once and only once the runtime is `READY`, caching the answer in an atomic.

**Method note, which is the transferable part.** The backtrace was in the failing log from the very
first run and I treated it as noise for three attempts while doing static cfg analysis. Symbolizing
it was cheap and decisive: the module base falls straight out of the fault (`vaddr 0x98001` at
`rip 0xc80098001` gives base `0xc80000000`), and `llvm-nm --numeric-sort` names every frame. Two
peers and I spent the morning on cfg audits, `DT_NEEDED` records and toolchain provenance; none of
those would ever have found this, because it was an ordinary `std::env::var` call and not a cfg arm
at all. **A cfg audit coming back clean was not evidence the cfg flip was innocent, and it was also
not evidence about anything else.**

Test count is now **57, not 58**: `src/bin/debug` carries one `#[test] fn test1()` and is
temporarily out of the workspace. 57 is the correct post-swap baseline; the difference is the
workspace change, not a regression.

## Stage 9 — bringing uuhelper back (2026-08-28)

Stage 7 left `src/bin/uuhelper` out of the workspace because uutils pulled `nix` (no twizzler
errno arm) and, via `terminal_size`, `rustix`. rustix is ported; this stage is the rest.

### The rebase did most of the work

`src/ports/coreutils` is now a submodule (added by the owner) of the
`twizzler-operating-system/coreutils` fork. Its `twizzler` branch was 9 commits on a March 2025
base, **6043 commits behind** upstream `main`, touching three files.

Every one of those nine commits is superseded. Upstream independently made the same fixes, and
made them better, because it migrated off `nix`:

| our patch | upstream `main` now |
|---|---|
| `FileInformation(#[cfg(unix)] nix::sys::stat::FileStat)` + a twizzler `std::fs::Metadata` arm | `#[cfg(any(unix, wasi))] rustix::fs::Stat` — no nix at all |
| twizzler arms in `from_file` / `from_path` / `file_size` / `number_of_links` / `PartialEq` | all one `rustix::fs::fstat`/`statat` path |
| `is_stdin_directory` twizzler arm returning `false` | `rustix::fs::fstat(stdin.as_fd())` |
| `sane_blksize`: `not(target_os = "windows")` -> `unix` | already `#[cfg(unix)]` upstream |
| `ln.rs`: a `std::os::twizzler::fs::symlink` import | `rustix::fs::symlink` under `any(unix, wasi)` |
| `io.rs`: `Stdio::from(self.fx)` -> `Stdio::from(self.into_file())` | upstream has a wasi arm; twizzler takes `From<OwnedFd> for Stdio`, which `os/unix/process.rs` already provides here |

This was not assumed. `git rebase --onto upstream/main` was run in an isolated clone (the box was
mid-sweep, and moving a submodule HEAD changes the parent's `source_fingerprint()`); all three
conflicts in the first commit were our arm against upstream's replacement. So the branch is now
`upstream/main` + **one** commit, and the twizzler-specific content is zero:

    6e310c8 Relax nix/libc requirements to the versions the Twizzler ports provide.

Old tip kept as `twizzler-pre-rebase-20260828`. Nothing is pushed.

That one commit exists because a *stricter* requirement does not fail loudly — see below.

### The libc patch was silently evicted, again

Re-resolving the lock moved the entire graph to registry `libc 0.2.189` and put our port in
`[[patch.unused]]`. The old lock had exactly one libc (0.2.178, ours); the new one had one too —
the registry's. Cause: **`src/ports/nix` itself requires `libc = "0.2.183"`** while
`src/ports/libc` is 0.2.178, so `[patch]` did not apply to nix's request, and a path patch being a
distinct source means the two libcs coexist rather than unify.

This is the same trap unix.md already records for rustix, hit a second time from a different
direction, so it is worth stating as a rule rather than an anecdote: **after any change that
re-resolves, check `[[patch.unused]]` and `grep -c '^name = "libc"$' Cargo.lock` in the same
command that fixes it.** Cargo reports it as a *warning* about an unused patch, buried in
`Downloaded ...` output — never as an error.

Fixed by relaxing `src/ports/nix`'s requirement to 0.2.178, plus the same relaxation in the
coreutils fork (which asks for `libc 0.2.186` / `nix 0.31.3`; our ports are 0.2.178 / 0.31.2).

### nix: the errno arm is nearly free

`errno.rs` holds twelve per-OS `mod consts` blocks in one `cfg_if!` chain with no fallback, which
is why nix does not build here. The port turned out to be two cfg edits, because mlibc gives
Twizzler the **Linux errno set**:

- Every one of the 131 `libc::E*` constants the Linux `mod consts` names is already declared in
  `src/ports/libc/src/twizzler/mod.rs` — measured, zero missing.
- Values match Linux exactly (`EPERM=1`, `EAGAIN=11`, `EDEADLK=35`, `EOPNOTSUPP=95`,
  `EOWNERDEAD=130`, `ERFKILL=132`, `EHWPOISON=133`).
- No two of the 131 share a value, which matters because the variants are enum discriminants —
  a collision would not compile.
- `__errno_location` is declared for twizzler (`libc` twizzler mod), so `errno_location()` needs
  only to be added to the arm that already calls it.

So twizzler joins the Linux arm. Mechanically: a `linux_errno` cfg alias
(`any(linux_android, twizzler)`) in nix's `build.rs`, applied to the 61 `linux_android` sites in
`errno.rs` **only**. Scoped deliberately — there are 622 more `linux_android` sites elsewhere in
nix that are real Linux syscalls, and widening the global alias would drag twizzler into all of
them. The alias name says "shares Linux's Errno variants", not "is Linux".

`desc()`'s two `all(target_os = "linux", not(target_arch = "mips"))` arms (ERFKILL/EHWPOISON) took
a twizzler entry as well; without them the match is non-exhaustive for a variant twizzler has.

The rest of nix is not yet proven. Encouragingly, `sys/mod.rs` gates its Linux-only modules
(epoll, eventfd, fanotify, memfd, personality, prctl, quota, signalfd) with **include-lists**, so
twizzler simply does not get them — a missing-functionality failure mode, not a compile one. The
unconditional modules (`fcntl`, `unistd`, `sys/signal`, `sys/stat`) are where the real surface is.

### getrandom 0.4 needed a port

`rand 0.10` <- `uu_shuf`/`uu_sort` pulls `getrandom 0.4`, whose backend chain ends in
`compile_error!("target is not supported")`. The existing twizzler forks are 0.2.16 and 0.3.1 —
0.4 is a third major in the same graph, which the root manifest already has a pattern for
(`getrandom02`).

Ported as `src/ports/getrandom` (0.4.3 plus a `twizzler` arm over `twz_rt_get_random`, ~20 lines,
the same shape as the 0.3 fork's backend), patched in as `getrandom04`. **Vendored rather than a
submodule only because the fork repo has no 0.4 branch and this session cannot push** — it should
become a `twizzler-0.4` branch of `getrandom-twizzler` when someone can.

### Other wiring

- `uuhelper`'s 38 `uu_*` deps moved from `git+branch=twizzler` to path deps into the submodule,
  `0.0.30` -> `0.10.0`; `uucore` likewise in the root manifest. The 37 stale per-util entries in
  root `[workspace.dependencies]` were dead (only uuhelper referenced coreutils, and it declared
  its own) and are gone.
- Root `[workspace] exclude` gained `src/ports/coreutils`. Without it, the uu crates' inheritance
  (`edition.workspace = true`) resolves against the *twizzler* root, which has no
  `[workspace.package]`: "error inheriting `edition` from workspace root manifest". This is why
  coreutils needs an exclude and the other ports do not — they are single crates that inherit
  nothing.
- `procfs` looked like a target-side hazard in the new 115-package graph; it is gated
  `[target.'cfg(target_os = "linux")']` in uucore, so it is never built here. The lock lists it
  because a lock is target-agnostic — do not read a lock entry as "this gets built".

### State / what is unproven

`uuhelper` is back in the root workspace members list, which means **`build-all` now builds it**
and a failure there is everyone's failure. Peers were told. `#"crate:uuhelper"` in the initrd
stays commented out, as it was in HEAD.

Nothing above has been compiled. A peer's `x.py install` has been rewriting `toolchain/install`
for half an hour, and unix.md's own rule applies: the only signal that a toolchain is ready is
compiling something real. `cargo check-all --package uuhelper` is queued behind that pid exiting.

Expect the remainder of the nix surface, and uucore 0.10's API drift against `uuhelper/src/main.rs`
(a copy of upstream's `src/bin/coreutils.rs` from 0.0.30), to be what the first compile reports.

### Host check: clean (2026-08-28 21:29)

`cargo check -p uuhelper --target x86_64-unknown-linux-gnu` — **exit 0, zero errors, 1m28s.**

Run on the host deliberately, because it is independent of the toolchain rebuild that was
occupying `toolchain/install` and it isolates the failure modes that have nothing to do with the
family flip. What it proves:

- The manifest wiring resolves: 38 path deps into the submodule, `0.0.30` -> `0.10.0`, `uucore`
  repointed, and the nested-workspace `exclude`.
- **uucore 0.10 has no API drift against `uuhelper/src/main.rs`.** This was the largest unpriced
  risk in the stage: that file is a copy of upstream's `src/bin/coreutils.rs` from 0.0.30, and
  upstream's has since been rewritten (it now uses `coreutils::validation`, `itertools`, and a
  different `usage()`). The APIs uuhelper actually calls — `uucore::args_os`,
  `set_utility_is_second_arg`, `panic::mute_sigpipe_panic`, `display::Quotable`, the
  `Args` trait, and every util's `uumain`/`uu_app` pair — all survived unchanged. No port needed.
- All 37 enabled utils still exist upstream under the same names.
- The rebased coreutils tree compiles, and the `nix` errno alias edit does not disturb the Linux
  arm (`linux_errno` is true there, as before).

What it does **not** prove: anything twizzler-specific. On this target `cfg(unix)` was already
true, `nix` took its Linux arm and `getrandom` its Linux backend, so none of the three ports above
were exercised. The twizzler check is the one that tests them, and it is still parked.

The value is in the separation: if the twizzler check now fails, the failure is in the twizzler
arms and not in the manifest, the rebase, or uucore's API — those are excluded by a green run
rather than by argument.

### nix: the fallback-less-chain class is already clear

The errno failure was a `cfg_if!` chain with no `else` arm — twizzler matches nothing, and what
the chain defines does not exist. That is the class worth auditing for, because it produces
unresolved-import errors rather than missing-item ones, so a script found all of them:
12 fallback-less chains in nix, of which

- `errno.rs:21` — fixed above (the only one that was blocking).
- `sys/sendfile.rs` (x2), `sys/socket/{addr,mod,sockopt}.rs` (x5), `sys/inotify.rs`,
  `sys/resource.rs` — all inside modules twizzler never compiles. `sys/mod.rs` gates these with
  **include-lists** (`any(linux_android, freebsdlike, apple_targets, solarish)` and friends), and
  `socket`/`resource` are not among the features uucore enables (`dir`, `fs`, `poll`, `signal`,
  `uio`, `user`, `zerocopy`). Note `zerocopy` does *not* rescue `sendfile`: the module carries an
  OS include-list on top of the feature gate, and twizzler is not in it.
- `fcntl.rs:294` — the one hit in an unconditionally-compiled module, and a false positive on
  inspection: it *defines* Linux-only items (`ResolveFlag`, `openat2`) rather than selecting an
  implementation. No else arm means they simply do not exist here, which is only an error if
  something unconditional references them. Nothing does.

So no selection chain blocks twizzler outside errno.rs. What remains is the other failure class —
`libc::` items referenced from unconditional code that our twizzler libc module does not declare.
A name-diff of nix's 1201 `libc::` references against the module's 2435 exports leaves 387
candidates, but that number is not usable: it is dominated by BSD `AF_*`, Linux filesystem magics,
`aio_*` and `ALG_SET_*` inside cfg-gated code, plus false positives like `c_int`/`c_char` (re-exports
from libc's root, not the twizzler module). Reachability is the missing half and a grep cannot
supply it. The compiler names exactly the reachable ones, so that is the instrument — recorded here
only so the next person does not redo the grep and mistake 387 for a workload.

### Twizzler check: 35 -> 6 -> 11 -> ? (2026-08-28 21:44 onward)

The gate worked as designed: waited on the xtask pid (not the inner `x.py`), confirmed
`libunwind.so` in the sysroot, then ran at load 11.8.

Errors moved *outward* each round, which is the shape you want — each round's fixes were real and
the frontier advanced to the next layer:

| round | errors | where |
|---|---|---|
| 1 | 35 | `rustix` 22, `nix` 11 |
| 2 | 6 | `rustix` 1, `nix` 3 |
| 3 | 11 | `uucore` only — **nix and rustix compile** |

Both failure classes were the predicted ones: missing `libc` declarations, and twizzler falling
into a *default* arm meant for something else. The second is the more interesting half and is the
mirror image of the errno bug — not "no arm matches" but "the wrong arm matches", which the
fallback-less-chain audit could not have found.

**Twizzler kept falling into BSD-shaped `not(linux)` defaults.** `nix` gave three:
`dir.rs` read `de.d_fileno` (our `dirent` has `d_ino`), `unistd.rs` wanted `pw_class`/`pw_change`/
`pw_expire` (our `passwd` is the 7-field glibc shape), and `signal.rs` selected the BSD signal set.
Fixed with a single `linux_abi` cfg alias (`any(linux_android, twizzler)`) applied *only* at sites
that assert Linux's ABI — never at one selecting a Linux syscall. That is why the alias is not
`linux_android` widened: 622 sites elsewhere in nix are real Linux syscalls Twizzler does not have.
`sigsuspend`'s gate (`signal.rs:608`) was left alone deliberately even though mlibc implements it —
it is a functionality gain, not a build fix, and belongs in its own change.

**Facts about mlibc worth recording, each of which cost a lookup:**

- `struct sigaction.sa_flags` is `unsigned long`, not glibc/musl's `int`. Our libc is right and
  nix's `SaFlags_t = c_int` was wrong; twizzler now joins the `uclibc` arm. Anything that assumes
  the POSIX-usual `int` here is wrong on Twizzler.
- `struct msghdr` is **musl-shaped**: `msg_controllen` is `socklen_t`, `msg_iovlen` is `int`. Linux
  glibc uses `size_t`/`size_t`, which is why rustix's `msg_control_len` needed twizzler moved into
  the *first* arm — the one whose other members are musl and the BSDs, not the Linux one.
- `siginfo_t` is the musl layout, so its accessors were taken verbatim from libc's own musl target
  rather than indexed off `_pad` — offsets nothing would check.
- `statfs` is a **Linux option** in mlibc (`options/linux/`), `statvfs` a POSIX one
  (`options/posix/`, degrading to ENOSYS). uucore must take the statvfs arm; the statfs one would
  not link.
- Present and implemented, just never declared in our Rust libc: `fchdir`, `chroot`, `nice`,
  `get/setpriority`, `get/setrlimit`, `killpg`, `truncate`, plus `F_RDLCK`/`F_WRLCK`/`F_UNLCK`.
  All added. Absent from mlibc and therefore gated *out* rather than invented:
  `FALLOC_FL_NO_HIDE_STALE`, `IPPROTO_MPTCP`.

### Two defects in our libc port, found on the way

**1. `SIGINFO: c_int = 33` is fabricated.** mlibc's twizzler `signal.h` defines no `SIGINFO` (nor
`SIGEMT`); Twizzler has Linux's signal set, where 33 is in the realtime range. Nothing in the
uutils graph references it any more now that nix takes the Linux arm, so it is inert today — but it
is a constant that will silently do the wrong thing for the first caller who trusts it. Not removed
here: deleting a public constant is a wider change than this stage should make, and it wants its own
look at who else reads `libc::SIGINFO` under a `not(linux)` cfg. **Flagged, not fixed.**

**2. Unexplained: `libc::SIGEMT` resolved, to 0.** Round 1's E0080 showed
`const SIGNALS: [Signal; 31]` with index 29 — `SIGEMT` — holding `0x00000000`, an invalid `Signal`
discriminant. For that to be a const-eval error rather than E0425, `libc::SIGEMT` must have
resolved; name resolution runs first and would have aborted otherwise. But it is not there:
`grep`, `git grep`, and an exhaustive Python walk of every `.rs` under `src/ports/libc/src` agree
that the twizzler module defines no `SIGEMT`, every one of the crate's 20 definitions is in a
module twizzler does not compile, and all 20 are `7`, never `0`. `lib.rs`'s twizzler arm pulls in
`primitives` and `twizzler` only. Round 1's log never names `SIGEMT` and contains no E0425 in nix.

The fix made it moot — twizzler now takes the Linux arm, which has no `SIGEMT` variant — so this is
**not** a blocker. It is written down because the mechanism is still unaccounted for, and if some
`libc::SIGEMT` really is reachable at 0, any *other* crate with a `not(linux)` arm naming it will
hit the same thing and the next person should not re-derive this from scratch. Do not treat the
disappearance of the error as an explanation of it.

### Round 5: green. `build-all` passes with uuhelper in the workspace (2026-08-28 22:05)

`cargo check-all --package uuhelper`: **0 errors, exit 0.** `cargo build-all`: **0 errors, exit 0,
2m36s.**

Checked that the green run was not a no-op, because a check that silently skips its package is
indistinguishable from one that passes: the log shows `uuhelper`, `uucore`, all **38** `uu_*`
crates and the vendored `getrandom 0.4.3` actually compiled, and
`target/dynamic/x86_64-unknown-twizzler/debug/uuhelper` exists as a twizzler ELF whose `NEEDED`
list (`libstd`, `libc`, `libunwind`) is identical to `shell` and `init`.

Final round's fix, and a good example of the whole stage's shape: `FsMeta::block_size` returned
`self.f_bsize` unconverted, in an arm written for Linux glibc's `statfs` where `f_bsize` is `i64`.
Our `statvfs.f_bsize` is `c_ulong` — musl's shape again. Twizzler moved to the
`try_into().unwrap()` arm whose other members are musl and the BSDs.

**The pattern, stated once because it recurred in every layer:** the family flip's failure mode is
not mainly "no arm matches". It is "the *wrong* arm matches". Six of the twelve fixes in this stage
were twizzler falling into a `not(any(linux_android, ...))` default shaped for BSD. A
fallback-less-chain audit — the thing that found the errno bug — is blind to all of them, because
nothing is missing; a default silently fits. Anyone auditing cfgs for the flip should read
`not(...)` lists first and `cfg_if!` chains second.

The twelve fixes, by home:

- `src/ports/libc` (2): nine declarations for functions mlibc already implements
  (`fchdir`, `chroot`, `nice`, `get/setpriority`, `get/setrlimit`, `killpg`, `truncate`) plus
  `F_RDLCK/WRLCK/UNLCK`; and musl-shaped `siginfo_t` accessors (`si_pid`/`si_uid`/`si_status`/
  `si_utime`/`si_stime`) copied from libc's own musl target.
- `src/ports/nix` (5): the `linux_abi` alias over errno/signals/passwd, `SaFlags_t = c_ulong`,
  `d_ino`, `features.rs`'s missing `mod os`, and the libc version relaxation.
- `src/ports/rustix` (3): `msg_control_len` (both arms), `FALLOC_FL_NO_HIDE_STALE` and
  `IPPROTO_MPTCP` gated out.
- `src/ports/coreutils` (6 arms, 1 commit): `ALL_SIGNALS`, `StatFs`/`statfs_fn` -> statvfs,
  `read_fs_list` -> the "no mount table" arm, `block_size`.
- new `src/ports/getrandom`: 0.4.3 + a twizzler backend.

### Provisional: every result above ran against a partial sysroot

The user's `cargo toolchain ports rust` **failed** at 21:43:46 after 51 minutes, leaving
`sysroots/x86_64-unknown-twizzler/pkg/rust` wiped-and-partial. Round 1 started 21:44:10 — twenty-four
seconds later. So all five checks and the build ran against it.

They evidently work (real code compiled and linked; the errors moved outward coherently rather than
collapsing into "can't find crate for std"), but **nothing here has been reproduced on a completed
toolchain**, and that is the difference between "looks sound" and "is verified".

The reason I did not know: **my readiness gate was vacuous.** It keyed on `libunwind.so`, which
lives at `sysroots/<triple>/lib/`, *outside* the `sysroots/<triple>/pkg/rust` prefix that
`ports/rust.rs:91` wipes. It is the restored copy from the 8a incident, mtime 2026-08-25, untouched
by tonight's run — so it read "ready" whether the step ran, finished, or died. A constant reads as
a pass. (Credit twizzler-39 for catching it, and for the corrected rule.)

**The rule, in its full form:** a readiness witness must be produced only at the *end* of the step
**and** lie *inside the scope the step mutates*. unix.md already had the first half; the second half
is what was missing, and it is the half that made `libunwind.so` look like a good choice. For this
step a real witness is something under
`sysroots/<triple>/pkg/rust/lib/rustlib/<triple>/lib/`.

### Why the toolchain build failed — a fourth manifest

    error[E0425]: cannot find type `_Opcode`
      --> ~/.cargo/registry/.../rustix-1.1.2/src/ioctl/mod.rs:289

Registry rustix 1.1.2, not `src/ports/rustix` 1.1.4 — and `_Opcode` is exactly the arm the port
adds (stage 7). `toolchain/src/rust/Cargo.toml:96` patches `libloading` to `src/ports/rust_libloading`
but has **no rustix entry**, and that workspace carries its own lock, so the root `[patch]` never
reaches it. Consumers are `tempfile` and `xattr` — both on this file's "host only" list, and both
now reachable for the target *because of the flip*.

So the earlier note ("two manifests, not one") is understated. The rule is: **any workspace with
its own lock needs the patch set repeated.** There are at least four — the root,
`third-party/elephance/twz`, `toolchain/src/rust`, and now `src/ports/coreutils` (which is why it
needed a root `exclude` rather than a patch). The path in the error message is the tell, every time.

**Not fixed here, deliberately.** `toolchain/src/rust/Cargo.toml` is the user's active build input,
adding a patch re-resolves that lock — the exact eviction trap that removed our libc from the graph
earlier tonight — and re-running costs ~51 minutes. It needs the owner's decision, not a fix
slipped in behind a green build. (Diagnosis: twizzler-39.)

### Retraction: the results are NOT provisional (2026-08-28 22:20)

The section above says every check ran against a partial sysroot. **That is wrong, and this
supersedes it.**

The directory `ports rust` wipes is `sysroots/<triple>/pkg/rust`. What a build or check *consumes*
is `sysroots/<triple>/lib` — `pathfinding.rs:71-73` builds the sysroot path as
`sysroots/{triple}/lib`, and `grep -rn 'pkg/rust' tools/xtask/src` returns exactly one hit,
`ports/rust.rs:93`, i.e. the step that writes it. Nothing on any compile or link path reads it.
`pkg/rust` is the rust-on-twizzler *package* that ships into the disk image, not the
cross-compiler. Verified here directly, not taken on report. (Correction: twizzler-39.)

So rounds 1-5 and `build-all` ran against a sysroot that was never degraded, and the coherent
outward movement of the errors was not luck. The green result stands on its own.

What *is* still true from that section, and is the part worth keeping: **the gate was vacuous
anyway.** `libunwind.so` sits outside the wiped subtree, so it would have read "ready" whether the
step ran, finished or died. It happened not to matter because the wipe never touched a build input
— but a witness that cannot fail is not evidence, whatever the outcome. The corrected rule (produced
only at the end, *and* inside the mutated scope) stands unchanged.

Both halves of this were scope errors of the same shape, made in both directions: asserting what a
step destroys without reading the path, then inheriting that scope without checking it. The cheap
question that would have settled it at any point is **"what reads this?"**, not "what does this
step do?" — consumers bound the damage, the writer does not.

### It boots, and coreutils runs (2026-08-28 22:5x)

Five utils, five separate `--autostart` boots, all exit 0:

    autostart: /initrd/ls ["/"]
    data  etc  ext  initrd  pkg  sysroot  tmp

    autostart: /initrd/ls ["-l", "/initrd"]
    total 117839
    lrwxrwxrwx 1 0 0        0 Jan  1  1970 base32 -> /pkg/twizzler/bin/uuhelper
    lrwxrwxrwx 1 0 0        0 Jan  1  1970 base64 -> /pkg/twizzler/bin/uuhelper

    echo hello twizzler   -> hello twizzler
    seq 1 5               -> 1 2 3 4 5
    printf %s-%s a b      -> a-b

The multi-call dispatch works end to end: `run_autostart` resolves `/initrd/<util>`, init has
symlinked that to `/pkg/twizzler/bin/uuhelper` on the disk image, and uuhelper reads `argv[0]`'s
file stem to pick the utility. **uuhelper is not in the initrd and does not need to be** — it is
staged into the disk image sysroot by `copy_twizzler_build`.

`ls -l` is the one worth reading closely: it exercises `std::os::unix::fs::MetadataExt` over the
twizzler PAL, which is stage 3's work, and the output shows the absent-field policy holding
exactly as written — `nlink` 1, `uid`/`gid` 0, and an epoch mtime. Fixed values, visibly fixed,
not fabricated ones.

### The boot bug the green build could not see

Between "build-all exits 0" and this, twelve consecutive sweep rounds died identically at
`monitor::monitor: needed symbol fstatfs not found`, failing `ctx.relocate_all(monitor_id)` in
bootstrap. The monitor never loaded, so init never ran. (Found by twizzler-db, at the cost of a
whole sweep.)

**Cause: cargo feature unification, not anything in the changed code.** rustix is not in the
monitor's dependency graph at all — `cargo tree -p monitor -i rustix` returns "did not match any
packages" — yet the monitor's binary contains `rustix::backend::fs::syscalls::fstatfs` and an
undefined `fstatfs`. rustix reaches it via `dynlink -> miette -> terminal_size`, and adding
uuhelper switched on rustix's `fs` feature, which cargo applies to the *single* rustix build
shared by the whole collection. `statfs`/`fstatfs` live in mlibc's **Linux** option group, which
Twizzler does not enable, so the installed `libc.so` exports neither.

The generalisable form, which is worse than it first looks: **adding a package to the workspace
can change what an unrelated compartment requires at load time**, with no edit to that compartment
and nothing visible in its own manifest. "Not my area" is not a sound inference in this build
model.

Fixed by excluding twizzler at **four** rustix sites — the backend `statfs`/`fstatfs`, the public
`fs::statfs`/`fs::fstatfs` wrappers, and `Dir::statfs` with its import. Four separate builds found
them one at a time, because a failed build reports only what it reached.

### The instrument this should have had

`build-all` exit 0 plus "the ELF exists and its NEEDED list matches `shell`" **cannot see an
unresolved symbol** — that check is precisely the one that defers resolution to load time
(`--allow-shlib-undefined` is in the dynamic collection's rustflags). Both statements were true
at once: the NEEDED list was identical *and* the symbol was missing.

`scratchpad/undefsyms.sh <profile>` closes it: undefined **C-ABI** symbols per built binary, minus
everything the sysroot's libraries export. Two things about it that matter more than the script:

- **Restricted to C symbols deliberately.** The first version included Rust-mangled ones and
  emitted thousands of false positives, because the sysroot's `libstd-b6d4b6a092bb23cc.so` is not
  the `libstd-68c4d1fd25e5258c.so` the binaries name. mlibc gaps are always C symbols, so nothing
  is lost.
- **Emptiness is not the signal; the delta is.** `monitor_rt_*`, `__TWIZZLER_SECURE_GATE_*`,
  `logboi_*`, `rust_eh_personality`, `std_entry_from_runtime` are legitimately undefined and
  resolved by the monitor at load. Against the known-bad release build the delta was exactly
  `{fstatfs}`; that is what was trusted, not a clean run.

It was validated against a positive control (the stale release tree, which still had the bug) —
and the control is what exposed the libstd false-positive flood. An all-clear from an instrument
that has never printed anything else is worth nothing.

### One rule covering three of tonight's failures

From twizzler-db, and it is the most reusable thing here: **a health check must not be satisfiable
by evidence that survives the failure.**

- Their sweep denominator counted `test result:` — printed early by the *kernel* suite — so it
  reported 12/12 booted on a sweep where 0/12 completed userspace.
- My readiness gate keyed on `libunwind.so`, which sits outside the subtree `ports rust` wipes, so
  it could not have failed.
- Their hang classifier had a branch that could never fire.

Three incidents, one defect: the evidence outlives the thing it is supposed to detect.

### Removing the 37 unresolvable libc declarations (2026-08-28 23:xx)

`fstatfs` was not special. Diffing every extern declaration in
`src/ports/libc/src/twizzler/mod.rs` (514 of them) against the union of what *every* sysroot
library exports — not just `libc.so`, since `dl_iterate_phdr` comes from `libtwz_rt.so` and a
libc-only denominator overstates the gap — leaves **37 that resolve to nothing**:

    acct brk cfsetspeed clone eventfd fallocate freeifaddrs fstatfs getifaddrs madvise memalign
    mount pthread_getattr_np quotactl readahead reboot sched_getaffinity sched_getscheduler
    sched_rr_get_interval sched_setaffinity setfsgid setfsuid signalfd splice statfs swapoff
    swapon sync_file_range syscall tee timerfd_create timerfd_gettime timerfd_settime umount
    umount2 vhangup vmsplice

Each was a boot-killer waiting for its first caller, on the mechanism that cost a twelve-round
sweep: the call compiles, the link succeeds under `--allow-shlib-undefined`, and the monitor dies
at relocation. Deleted, on the owner's instruction. **This converts a load-time failure in an
arbitrary compartment into a compile error at the call site, with the caller named.**

Checked before deleting, and worth recording because the greps look alarming and are not:
textual references exist for most of the 37 across `nix`, `rustix`, `memmap2`, `ferroc` and
libstd (`alloc/unix.rs`, `pal/unix/stack_overflow.rs`, `thread/unix.rs`, `kernel_copy/linux.rs`).
A grep cannot tell reachability. The compiler can, and did: **exactly one** of the 37 had a
reachable caller.

That one is `cfsetspeed`, from rustix's termios backend. Fixed by *implementing* it rather than
re-declaring it — POSIX defines it as setting both speeds, and mlibc exports `cfsetispeed` and
`cfsetospeed` (verified against `libc.so`, not against mlibc's source). It goes in the twizzler
module's `f!` block, so it is a Rust body, not a symbol reference. Net functional gain.

Verified: `check-all` exit 0, `build-all` exit 0, `undefsyms.sh` shows no `fstatfs` and nothing
outside the known monitor-resolved floor, and the guest boots and runs coreutils —
`autostart /initrd/seq finished with code 0`, output `1 2 3`.

**Two mistakes made while doing this, both of the shape this file keeps recording:**

- `cargo build-all --keep-going` exits **2 on argument parsing** (`--keep-going` is a `check`
  flag, not a `build` one). That presented as "EXIT=2, build failed" with zero compiler errors —
  a build that never ran, wearing the costume of a build that failed. Read the error count, not
  the status.
- The first `cfsetspeed` insertion landed inside a `cfg_if! {` block, because the anchor `"f! {"`
  is a **suffix of both `cfg_if! {` and `safe_f! {`**. Five matches in the file, and the script
  was running in the background so its failed assertion was invisible. Anchor on the whole line
  (`l == "f! {"`), and do not background an edit whose failure you have not seen succeed once.

### uuhelper's aliases moved from init to image creation (2026-08-28 23:2x)

They were made on every boot, by `init`, as naming-server nodes at `/initrd/<util>` — state that
was already known when the disk was written, rebuilt from scratch each time, and which never
survived into the image at all. They are now **ext4 symlinks written by `xtask disk`** at
`/sysroot/pkg/twizzler/bin/<util>` -> `/pkg/twizzler/bin/uuhelper`.

Three pieces, because moving the links moves how they are found:

- `disk.rs` creates them in `copy_twizzler_build`, guarded on `uuhelper` actually being staged —
  a link to a binary that is not there gives a name that resolves and then fails to spawn, which
  is worse than a name that does not exist.
- `init` gained `/pkg/twizzler/bin` in `PATH`, after `/initrd` so a boot-image copy of a program
  still wins over the disk copy. `exec.rs::find_id` searches `PATH` for bare names, and the shell
  reaches it through `Command::new`, so `ls` still works from the prompt.
- `run_autostart` gained a third fallback, `/pkg/twizzler/bin/<name>`. Without it
  `--autostart="ls /"` breaks, since its second fallback was the `/initrd/<util>` link that no
  longer exists.

**The util list is parsed from `src/bin/uuhelper/Cargo.toml`, not copied.** The first version
hardcoded the 37 names in `disk.rs` and was stale **within minutes**, when another session added
15 utilities (`b2sum`, the `sha*sum` family, `wc`, `expr`, ...). The manifest's `feat_os_twizzler`
list is not merely a fresher copy: it is the only list with a causal link to what got compiled,
because it decides which `uu_*` crates are built in. A hardcoded copy can be correct and still
drift; a derived one cannot be wrong without the build also being wrong. `cargo_toml` was already
an xtask dependency.

Verified on a `disk -f reset` image: **52 symlinks, 52 entries in `feat_os_twizzler`** — the 15
additions propagated with no action from either session. `autostart: /pkg/twizzler/bin/ls`
resolved through the new disk fallback, `ls -l /pkg/twizzler/bin` rendered the farm, exit 0, and
zero `/initrd/<util>` resolutions anywhere in the log.

**Not yet verified:** that the staged binary dispatches one of the 15 *new* names. 52 links
matching 52 features proves the list is in step with the manifest, not that the binary is — the
release `uuhelper` predates the feature addition, so an image staged from release would carry 52
names over a 37-util binary, which is precisely the resolves-then-misbehaves case the derivation
exists to prevent. Mine was a debug image, so this is a gap in coverage rather than a known
failure. `b2sum` is the one-command test; it waits for a box that is not mid-benchmark.

### Gap closed: a newly-added util dispatches, and its output is correct (2026-08-29 00:0x)

The open item above — that 52 links matching 52 features proves the *list* is in step with the
manifest, not that the *binary* is — is now closed:

    autostart: /pkg/twizzler/bin/sha256sum ["/etc/services"]
    a1c2d8af47c1a951f39f11cf53160703f282946d9821068eadf97b7d43208a34  /etc/services
    autostart /pkg/twizzler/bin/sha256sum finished with code 0

`sha256sum` is one of the fifteen utilities added *after* the image's `uuhelper` existed, so this
exercises the whole chain the derivation was built for: manifest -> `xtask disk` symlink ->
`/pkg/twizzler/bin` PATH entry -> `run_autostart`'s disk fallback -> multi-call dispatch on
`argv[0]`.

And the digest is checked against ground truth rather than assumed from the exit code:
`disk.rs:174` copies the **host's** `/etc/services` into the image, so the same bytes are hashable
on both sides. GNU coreutils on the host produces the identical digest. That verifies the read
path end to end — ext4, pager, external namespace, `std::fs`, uucore — not merely that a process
exited 0.

**Two instrument failures on the way to this, both mine, both of the night's recurring shape:**

- The first attempt piped each boot through `grep -E 'autostart|finished with code|...'` before
  writing the log. Three boots produced no matching line and I read that as three failed boots.
  It was a **filter that discarded the evidence of its own failure** — a build error, had there
  been one, could not appear. Capture whole, filter when reading.
- The gated runner was launched as `script &` inside a background call, so the tool reported
  "completed" for the *wrapper* while the script ran on undetached. A completion notice for the
  thing that launched the work is not a completion notice for the work.

## Follow-up: stop keeping a second copy of `libtwz_rt.so` (2026-08-29, not started)

Raised by the owner off the back of the libc collapse: since `toolchain/src/rust` now
*references* the in-tree ports by path (`libc`, `libloading`, `rustix`) instead of carrying its
own, twz_rt should probably work the same way rather than being copied into the sysroot.

The premise checks out. There are **three** copies of twz_rt in play and the build already knows
one of them is a lie:

| copy | written by | state on 2026-08-29 00:0x |
|---|---|---|
| `target/dynamic/<triple>/<profile>/libtwz_rt.so` | every OS build | 08-29 00:03, md5 `d34398c3` |
| `toolchain/install/sysroots/<triple>/lib/libtwz_rt.so` | `build.rs:593-611`, **release only**, under `only_runtime` (i.e. `bootstrap --step rt`) | 08-28 09:14, md5 `533bca7f` — **15h stale, different content** |
| `/sysroot/lib/libtwz_rt.so` in the disk image | `disk.rs:276` | fresh, copied from the build output |

`disk.rs:161` **deletes** the sysroot's copy while staging the image
(`ext4.remove("/sysroot/lib/libtwz_rt.so")`) and `copy_twizzler_build` puts the freshly built one
back. Its own comment says why: the staged copy "is written at toolchain-install time and goes
stale as soon as the runtime is rebuilt". So the workaround for the stale artifact already exists
downstream; what is missing is not having the stale artifact.

Note the staleness is structural, not an accident of this tree: the sysroot copy is refreshed
*only* by `--step rt`, and *only* in release, so an ordinary `build-all` — let alone a debug one —
cannot update it. It is stale by construction between bootstraps.

**New evidence (2026-08-30):** a fresh full bootstrap (toolchain_4ab785b) produced a sysroot with
*no* `libtwz_rt.so` at all — the first `build-all` afterward failed linking `monitor`
(`ld.lld: unable to find library -ltwz_rt`) until `bootstrap --step rt` was run by hand. So the
copy is not merely stale between bootstraps; after a from-scratch bootstrap it is absent, and the
OS cannot be built. Whichever design wins has to make first-build-after-bootstrap work without a
manual step.

### Two candidate designs

1. **Do not stage it at all.** Drop the copy in `build.rs`, drop the delete in `disk.rs`, let
   `copy_twizzler_build` be the only producer. Removes an artifact whose sole remaining purpose is
   to be deleted.
2. **Symlink** the sysroot entry at the build output so it cannot go stale on the host.
   `copy_sysroot`'s symlink branch would recreate it as a *dangling* link in the image (the target
   is a host path), which is harmless only because `disk.rs:161` removes that entry anyway — so
   this design quietly depends on the delete staying, which is the kind of coupling that breaks
   later. Weaker than (1) unless (1) is impossible.

Note the analogy to the libc collapse is not exact, and that is the whole difficulty: `libc` is a
*Rust crate* and can be path-referenced from a manifest. `libtwz_rt.so` is a built shared object
that has to physically exist in a `-L` search path at the moment something links against it.

### The question that decides it — answer this first

**Is the sysroot copy a build input for `ports rust`'s hosted std?** `ports/rust.rs:169-171` marks
the native std with `--cfg twizzler_hosted` so that "`-ltwz_rt` can be emitted from libstd itself
and a bare `rustc prog.rs` links without extra flags", and notes the *cross* std is built without
it "(it builds libtwz_rt.so in the first place)". That reads as a deliberate ordering — cross std,
then twz_rt from the OS tree, then hosted std, which links against it — and if the hosted-std link
really does resolve `-ltwz_rt` out of `-L <sysroot>/lib`, then the sysroot copy is **not**
vestigial and design (1) breaks `ports rust`. In that case the fix is to keep the copy but make it
un-stale: refresh it from every runtime build rather than only from `--step rt`.

Settle it by reading the actual link line for the hosted std in a completed `ports rust` log, not
by reasoning from the comment.

### What is already established

- **No C port links `-ltwz_rt`** — `grep` over `ports/*.rs` finds it only in `rust.rs`.
- **mlibc's dependency is load-time, not link-time.** The sysroot's `libc.so` has 39 undefined
  `twz_rt_*` symbols, resolved by dynlink at load; the dynamic collection passes
  `--allow-shlib-undefined` precisely so this links.
- So the only *identified* consumer of a `libtwz_rt.so` on a `-L` path is an **on-target** rustc,
  and that one reads the image copy, which is already fresh.

### Why it was not done now

`ports rust` (attempt 3) was running out of that exact sysroot at the time this was written, and
this change edits what it reads. Do it when the toolchain has settled — and the completed run is
also what answers the open question above.
