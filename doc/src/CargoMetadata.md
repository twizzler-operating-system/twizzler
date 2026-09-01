# Cargo metadata for Twizzler crates built inside xtask

The xtask program organizes the build into a series of "collections" that get built in different environments. There are:

 - Tools (targets build system)
 - Kernel (targets arch-machine-none)
 - Userspace (targets arch-machine-twizzler, optional, default yes)
 - Userspace-static (targets arch-machine-twizzler-minruntime, optional, default yes)
 - Userspace-tests (targets arch-machine-twizzler-minruntime, optional, default no)
 - Test-only programs (part of Userspace, but built only for a tests build)
 - Kernel-tests (targets arch-machine-none, optional, default no)

Programs may select which collection to be compiled in based on the metadata value set in Cargo.toml, described in more detail below.

## Static versus non-static builds
Twizzler currently builds packages in two `target_env` settings: "minruntime" and "". This translates to two triples that are used for userspace twizzler programs: arch-machine-twizzler, and arch-machine-twizzler-minruntime. The minruntime variant is defined to be for statically linked programs, using the default minimal runtime provided by twizzler-abi. Such crates can declare that they should be compiled only in the minruntime collection by setting the key `package.metadata.twizzler-build` to "static" in Cargo.toml:

```{toml}
[package.metadata]
twizzler-build = "static"
```

## Tools

Tools should be placed in the tools subdirectory, and should set the `package.metadata.twizzler-build` key to "tool" in Cargo.toml:

```{toml}
[package.metadata]
twizzler-build = "tool"
```

## Test-only programs

Programs that only exist to be run by the test suite (the crates under `src/test`, and the
monitor's test crates) set the key to "test":

```{toml}
[package.metadata]
twizzler-build = "test"
```

These join the Userspace collection only when the build was asked for tests -- `cargo build-all
--tests`, `cargo start-qemu --tests/--benches/--bench`, or an `--autostart` run, which is how a
single test program gets driven directly. A plain `cargo build-all` or `cargo start-qemu` neither
compiles them nor packs them into the initrd, even if they are listed there. `cargo build-all
--test-programs` builds them without the `#[test]` collection; `cargo check-all` and `cargo
doc-all` always cover them, so excluding them from normal builds cannot let them rot.

## The kernel and xtask
Both the kernel and xtask themselves set the `package.metadata.twizzler-build` key to "kernel" or "xtask". Programs should not use these values. 