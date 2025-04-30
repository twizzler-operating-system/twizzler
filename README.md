# The Twizzler Operating System

Twizzler is a research operating system designed to explore novel
programming models for new memory hierarchy designs. We are focused on
providing an environment designed around invariant data references and
long-lived pointers, thus being well suited for byte-addressible
non-volatile memory and multi-node networked applications.

This repo contains source code for the kernel and userspace, along
with a build system that bootstraps a Twizzler userspace. You can
write code for it and play around! We're not quite production ready,
but we're getting there! :)

See (https://twizzler.io/about/) for more details.

NOTE: This repo has recently been rebuilt with our pure Rust
implementation of the Twizzler kernel.  If you have previously forked
or used Twizzler you may find that some features are changed or not at
parity.  Please open an issue on our tracker if you find any
deficiencies.

## Building

See [BUILD.md](doc/src/BUILD.md) for details.

## Contributing

See [develop.md](doc/src/develop.md) for details.

## Reporting Bugs

All bugs found and features requested must be reported through our [github issue tracker](https://github.com/twizzler-operating-system/twizzler/issues).
Please add the appropriate label, ```bug``` or ```feature```, and also give as much detail as possible, including backtraces or such for bugs.

If you find a security vulnerability that needs responsible disclosure please contact the administrators of the project directly and we
will work with you on the fix and the disclosure credit.

## Code of Conduct

See [conduct.md](doc/src/conduct.md) for details.

## A Tour of the Repo

```
<root>
    doc -- documentation files
    src
        bin    -- Twizzler userspace programs
        kernel -- the Twizzler kernel itself
        lib    -- libraries for Twizzler
        rt     -- Runtime libraries and security monitor
        ports  -- Third-party explicitly ported software
    target (once built) -- compilation artifacts
    toolchain -- sources for all aspects of the Rust toolchain used to build Twizzler
        install (once built) -- install location for the toolchain
        src -- sources for the toolchain
            rust  -- cloned repo for Rust, modified for Twizzler userspace
            mlibc -- cloned repo for mlibc, a C library, patched for Twizzler
    tools -- build tools, like the build system orchestrator xtask
```
