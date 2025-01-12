# Obliviate

Obliviate is an interposition layer for efficient portable, transparent, and
crash-consistent secure data deletion.

## Building

Obliviate is written in Rust, and thus requires a Rust installation to be
compiled. It is recommended to use [rustup](https://rustup.rs/) to manage the
installation. To build Obliviate, clone this repo, `cd` into it, and run:

```sh
make            # Builds the binaries and shared libraries.
make install    # Installs the binaries and shared libraries.
```

## Running

After building/installing Obliviate, the `lethe-cli`, `lorax-cli`, and
`usdb-cli` binaries should be available (assuming binaries installed via `cargo`
are available through your `PATH`).

## Structure

The following diagram annotates the Obliviate file tree starting from the root.
The file tree is simplified and thus doesn't include all modules/system
components -- just the ones of note.

```
.
├── crates              # Additional crates
│   ├── lru-mem         # Memory-bounded LRU cache
│   ├── naslr           # Disabling of ASLR
│   ├── shmem           # Shared memory allocator and primitives
│   └── tempfile        # Temporary files
├── obliviate-cli       # Obliviate CLI
├── obliviate-core      # Core Obliviate library
│   ├── io              # Composable encrypted IOs
│   ├── kdf             # Key derivation functions
│   ├── kms             # Key management schemes
│   ├── state           # Shared Obliviate state
│   ├── bufcache        # Buffer cache
│   ├── enclave         # Virtualized secure enclave
│   └── wal             # Secure write-ahead log
├── obliviate-shims     # Obliviate shim layers
│   ├── lethe           # Tiered-KHF shim layer
│   ├── lorax           # Stable SDB shim layer
│   └── usdb            # Unstable SDB shim layer
└─ obliviate-trap       # Obliviate trap
```
