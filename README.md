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

See (https://twizzler.io/about.html) for more details.

NOTE: This repo has recently been rebuilt with our pure Rust
implementation of the Twizzler kernel.  If you have previously forked
or used Twizzler you may find that some features are changed or not at
parity.  Please open an issue on our tracker if you find any
deficiencies.

## Building

See doc/src/BUILD.md for details.

## A Tour of the Repo

```
<root>
    doc -- documentation files
    src
        bin -- Twizzler userspace programs
        kernel -- the Twizzler kernel itself
        lib -- libraries for Twizzler
    target (once built) -- compilation artifacts
    toolchain -- sources for all aspects of the Rust toolchain used to build Twizzler
        install (once built) -- install location for the toolchain
        src -- sources for the toolchain
            rust -- cloned repo for Rust, modified for Twizzler userspace
    tools -- build tools, like the build system orchestrator xtask
```
