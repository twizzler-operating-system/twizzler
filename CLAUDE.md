# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Twizzler is a research operating system exploring novel programming models for non-volatile memory (NVM). It replaces the traditional file abstraction with persistent **objects** that support direct pointers, avoiding serialization overhead. The system is written in pure Rust and targets x86_64 (primary) and aarch64 (secondary).

Academic reference: "Twizzler: a Data-Centric OS for Non-Volatile Memory" (ASPLOS 2020).

## Build Commands

All builds go through **xtask**, a Cargo-based build orchestrator. Never use raw `cargo build`; always use the aliases defined in `.cargo/config.toml`:

```bash
# Setup (first time)
./init.sh
cargo toolchain pull          # download pre-built Twizzler Rust toolchain

# Build
cargo build-all               # debug build (kernel + userspace)
cargo build-all --profile release

# Run in QEMU
cargo start-qemu              # boot debug build
cargo start-qemu --profile release
cargo start-qemu --tests      # run integrated test suite
cargo start-qemu --benches    # run benchmarks
cargo start-qemu -q=-nographic  # headless mode

# Check and lint
cargo check-all
cargo fmt --all -- --check
# clippy runs through check-all

# Documentation
cargo doc-all
```

Exit QEMU with `Ctrl-a x`.

## Architecture

### Workspace Structure

The root `Cargo.toml` defines a workspace with 60+ crates. Crates are categorized via `[package.metadata]` field `twizzler-build`:
- `"kernel"` — compiled for bare-metal targets (`x86_64-unknown-none`, `aarch64-unknown-none`)
- `"xtask"` — build tools only
- Unmarked — standard userspace crates compiled with the Twizzler Rust toolchain

The `[workspace.metadata] initrd` list controls which crates are packaged into the initial ramdisk.

### Key Source Directories

- **`src/kernel/`** — Bare-metal kernel: memory management, object system, paging, scheduling, security contexts, syscalls. Custom target specs live in `src/kernel/target-spec/`.
- **`src/lib/`** — Userspace libraries: `twizzler` (main API), `dynlink` (dynamic linking), `pager`, `naming`, `devmgr`, `secgate`, queue primitives, futures support.
- **`src/rt/`** — Runtime layer: `reference/` (twz-rt, the reference runtime), `monitor/` (security monitor for privilege separation), `minimal/` (minimal runtime).
- **`src/srv/`** — System services: naming-srv, pager-srv, devmgr-srv, logboi-srv, cache-srv, display-srv.
- **`src/bin/`** — Userspace binaries: `init`, `bootstrap`, `unittest` (test harness), debug/trace tools, test programs.
- **`src/abi/`** — ABI definitions (git submodule): `rt-abi/` and `types/`.
- **`src/ports/`** — Third-party libraries ported to Twizzler (libc/mlibc, rusqlite, ferroc, memmap2, etc.).
- **`tools/xtask/`** — Build orchestrator handling toolchain management, cross-compilation, initrd generation, disk imaging, and QEMU invocation.

### Core Abstractions

- **Objects** — Persistent data containers identified by KOID (Kernel Object ID), supporting direct pointers without serialization.
- **Views** — Thread execution environments defining address space and capabilities.
- **Security Contexts** — Capability-based access control via Kernel State Objects (KSO).

### Dependency Patching

Many third-party crates are patched for Twizzler support via `[patch.crates-io]` in the root `Cargo.toml`, pointing to forks in the `twizzler-operating-system` GitHub org or local paths under `src/ports/`.

## Testing

```bash
cargo start-qemu --tests                    # all tests (boots QEMU, runs tests, exits)
cargo start-qemu --tests --profile release  # release mode
```

Kernel tests use `#[kernel_test]` from `twizzler-kernel-macros` (a test failure halts the system). Userspace tests use standard `#[test]`.

## Code Style

- Nightly Rust toolchain (pinned in `rust-toolchain` file)
- `rustfmt` config in `.rustfmt.toml`: edition 2021, crate-level import granularity, `group_imports = "StdExternalCrate"`, 100-char comment width
- All code must pass clippy
- Unsafe code requires safety documentation

## Adding a New Userspace Program

1. `cargo new --bin myprogram` in `src/bin/`
2. Add `"src/bin/myprogram"` to `[workspace] members` in root `Cargo.toml`
3. Add `"crate:myprogram"` to `[workspace.metadata] initrd` to include it in the boot image
4. Build and run: `cargo start-qemu`
5. Inside Twizzler: `run myprogram`

## Debugging

GDB is configured in `.gdbinit` for remote debugging on port 2159:
```bash
cargo start-qemu --gdb 2159
# In another terminal:
gdb  # .gdbinit auto-connects to target remote :2159
```
