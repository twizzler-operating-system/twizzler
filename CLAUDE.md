# CLAUDE.md
This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository, Twizzler.

## Core Architecture Links
- General Directory Map: @.claude/repo_index.md
- Systems & APIs Overview: @.claude/architecture.md

## Code Style & Conventions
- Preferred syntax: Rust, matching project style.
- Limited comments: I only want good, useful, and succinct comments.

## Essential AI Guardrails & Preferences
- Think Before Coding: Always state architectural assumptions out loud before writing lines.
- Surgical Scope: Touch only what the specific task requires. Never modify or refactor neighboring files unless explicitly prompted.
- Simplicity First: Write the minimal code required to satisfy the logic targets. Avoid speculative abstractions.
- Clarification Policy: Stop and ask if a requirement or ambiguous parameter compromises implementation quality. Do not guess.
- Do not write big explainer block comments. Instead, expect an expert to be reading the code, and never describe
what is happening, only comments for things that are definitely confusing or outliers.
- Never run xtask commands anywhere except the repo root.

## This repo

Twizzler is a research OS (pure-Rust kernel + userspace) exploring an
object-based, invariant-pointer programming model for byte-addressable
persistent memory. Built via a custom cross-compiling toolchain and a build
orchestrator (`xtask`).

For a full file-by-file map of the repo and a deeper architecture writeup
(crate dependency graph, key types, syscall/secgate/IPC surfaces, boot
sequence), see `.claude/repo_index.md` and `.claude/architecture.md`. This
file stays intentionally short; those are the exhaustive references.

## Build & toolchain

First time setup: `./init.sh` (installs host deps: QEMU, `mke2fs`,
build-essential, etc. — Debian/macOS), then `cargo toolchain pull`
(downloads a prebuilt toolchain). Alternatively, `flake.nix` +
`.envrc`/direnv provides a reproducible dev shell with the same host deps.
If no prebuilt toolchain exists for your OS/arch:
`git submodule update --init --recursive && cargo toolchain bootstrap`
(builds LLVM/rustc/libc from source, 1-3 hours, ~50GB).

Everything else goes through `cargo xtask`, aliased in `.cargo/config.toml`:

- `cargo build-all [--profile release]` — build the tools, kernel, and
  userspace collections.
- `cargo check-all [--kernel] [--package <p>] [--bin <b>]` — fast type-check
  without a full build; prefer this over a bare `cargo check` while iterating.
- `cargo start-qemu [--profile release] [--tests] [--benches] [--gdb <port>]`
  — build and boot the result in QEMU.
- `cargo make-image` — build a bootable disk image without starting QEMU.
- `cargo doc-all` — build rustdoc across all collections.
- `cargo toolchain <pull|bootstrap|ports>` — manage the custom
  toolchain/sysroot; `cargo toolchain ports <name>...` builds/installs
  ported third-party libs into the sysroot (`cargo xtask disk -f reset`
  afterward to refresh the disk image).

Don't invoke plain `cargo build`/`cargo check` on individual kernel or
userspace crates — xtask owns picking the right target triple/flags for
each. Plain cargo is fine only for host-only tooling (`tools/xtask` itself).

Requires nightly Rust (pinned in `rust-toolchain`, picked up automatically
by rustup) and system LLVM 18
(`LLVM_CONFIG_PATH=/usr/bin/llvm-config-18` on Ubuntu), plus `mke2fs`. CI
(`.github/workflows/build-and-test.yml`) additionally sets
`CC=clang-18`/`CXX=clang++-18`/`LD=clang++-18`; match that locally if you
hit compiler-driver mismatches building C ports/toolchain bits.

## Testing

There's no host-side `cargo test`. Tests run *inside* a booted Twizzler
instance:

```
cargo start-qemu --tests --qemu-options=--nographic
```

boots a test-enabled image and runs both kernel and userspace test suites,
exiting with the aggregate result (this is exactly what CI does, see
`.github/workflows/build-and-test.yml`). `--benches`/`--bench <name>` runs
benchmarks the same way.

Kernel-space tests use `#[kernel_test]` (from `twizzler-kernel-macros`), not
`#[test]` — a failing kernel test halts the whole system, so you read
results from the boot/serial output rather than a per-test process exit
code. Userspace crates use ordinary `#[test]` and are picked up by the
`Userspace-tests` build collection (see below).

## Formatting

`rustfmt` config is in `.rustfmt.toml` (edition 2021,
`imports_granularity = "Crate"`, `group_imports = "StdExternalCrate"`). The
local pre-commit hook runs `cargo fmt --all`.

## Architecture

### Repo layout

- `src/kernel` — the kernel itself (`no_std` Rust binary).
- `src/lib` — shared libraries: `twizzler-abi` (syscall/ABI layer),
  `dynlink` (userspace dynamic linker), `secgate` (secure cross-compartment
  call gates), `twizzler` (high-level userspace object API), plus
  drivers/protocol libs (`twizzler-driver`, `nvme-rs`, `virtio-gpu`,
  `virtio-net`, `twizzler-net`, `twizzler-io`, `twizzler-queue*`,
  `twizzler-futures`, `naming`, `pager`, `devmgr`, `twizzler-security`,
  `twizzler-display`).
- `src/rt` — userspace runtimes: `reference` (the default dynamic-linking
  runtime — link against the wrapper crate `rt`, not `reference` directly),
  `minimal` (static/`no_std` runtime), `monitor` (the userspace security
  monitor that manages compartments/dynamic linking) + `monitor-api`.
- `src/srv` — userspace system servers, each usually pairing with a
  `src/lib` crate of the same purpose: `pager-srv`, `naming-srv`,
  `devmgr-srv`, `logboi-srv`, `cache-srv`, `display-srv`, `net-srv`.
- `src/bin` — userspace programs (`init`, `bootstrap`, `shell`, various
  test/debug utilities).
- `src/abi` — C headers + bindgen glue for the Twizzler runtime ABI
  (`rt-abi`).
- `src/ports` — third-party software ported to Twizzler (openssl, the
  ferroc allocator, lwext4-rs, etc.).
- `tools/xtask` — the build orchestrator; `tools/image_builder` /
  `tools/initrd_gen` are the disk/initrd builders it drives.
- `doc/src` — the mdBook source (`doc/src/SUMMARY.md` is the ToC). Read the
  relevant page here before making conceptual changes — it covers Objects,
  Lifetime, Pointers, Permissions, Views, KSOs, Gates, and the thread model
  in more depth than is reasonable to infer from code alone.

### The "collections" build model

xtask compiles the workspace as separate **collections**, each with its own
target triple: Tools (host), Kernel (`*-none`), Userspace (`*-twizzler`,
dynamically linked against the reference runtime), Userspace-static
(`*-twizzler-minruntime`), plus opt-in Userspace-tests/Kernel-tests. A crate
opts into a collection via `package.metadata.twizzler-build` in its
`Cargo.toml` (`"tool"`, `"static"`, `"test"`, or unset = default dynamic
userspace); `"kernel"`/`"xtask"` are reserved for the kernel and xtask
themselves. `"test"` marks a test-only program (everything under `src/test`,
plus the monitor's test crates): it is compiled into the Userspace collection,
and packed into the initrd, only for a tests build
(`--tests`/`--benches`/`--bench`/`--autostart`, or
`build-all --test-programs`); `check-all`/`doc-all` always cover them.
Programs that should ship in the boot image must additionally be listed in
the root `Cargo.toml`'s `[workspace.metadata] initrd = [...]`.

### Core OS model

(Full detail in `doc/src/*.md` — this is the minimum to not misread the code.)

- **Objects** are the fundamental persistent-memory abstraction: related
  data with shared lifetime/permissions, referenced by 128-bit IDs. The
  kernel is involved only at create/delete; access and modification are
  enforced by userspace + hardware, not kernel mediation.
- **Kernel State Objects (KSOs)** are ordinary objects the kernel also
  reads/writes given the use permission (e.g. thread control objects,
  security contexts).
- **Threads** are described by a control object with an `ExecutionState`
  (`Running` / `Sleeping` / `Suspended` / `Exited`). `Running` covers both
  "actually executing on a CPU" and "runnable, waiting on a run queue" — the
  state alone doesn't tell you whether a thread is on-CPU right now.
- **Security contexts** and **gates** (`secgate`) govern cross-compartment
  calls and privilege changes; the **monitor** (`src/rt/monitor`) is the
  userspace process that manages compartments/dynamic linking under this
  model.

### Kernel internals (`src/kernel/src`)

- `arch/` (per-ISA: amd64, aarch64) vs `machine/` (per-platform: pc, arm)
  splits hardware-specific code from the generic kernel.
- `obj/` — in-kernel object system: page tables, control objects,
  thread-sync/wait-word handling.
- `processor/` — scheduler, run queues, per-CPU state, IPIs.
- `thread/` — thread lifecycle, suspend, per-thread timing.
- `syscall/` — syscall entry points.
- A thread can simultaneously be a candidate for several different
  intrusive-linked-list wait/run queues (scheduler run queue, mutex wait
  queue, condvar queue, suspend list, sync/sleep-word waits, a requeue
  list). These are hand-rolled with `intrusive_collections`; when touching
  any one of them, check the add/remove symmetry across *all* of them
  rather than just the list you're editing — it's easy to remove a thread
  from one queue without correctly ensuring it lands on another (or gets
  scheduled), which manifests as an apparently-stuck or "orphaned" thread.