# Building Twizzler

A bit of a time consuming process the first time, so make sure you have some nice tea or something before you start :)

## Requirements

This build process has been tested on an Ubuntu 20.04 system with standard development tools
installed, in addition to rustup (which is required). We require rustup because we will build our
own toolchain during the build, and link the result through rustup for easier invocation of the Rust
compiler.

To build a boot image, you'll need the limine bootloader installed. In particular, we need the EFI
code to help boot Twizzler through their boot protocol.

To run qemu through the build system, you'll need qemu installed.

## Overview

Installing the tools:
  1. sudo apt install build-essential
  2. sudo apt install python
  3. sudo apt install cmake
  4. sudo apt install ninja-build
  5. Install Rust https://www.rust-lang.org/tools/install
  6. Clone submodules: `git submodule update --init --recursive`

Building Twizzler is done in several steps:

  0. Building xtask.
  1. Building the toolchain.
  2. Building Twizzler itself.

Fortunately, step 0 is handled automatically whenever we try to do anything. That's because xtask is
the "build system orchestrator". Essentially, building Twizzler requires using the right toolchain,
target specification, and compile flags at the right times, so we've placed that complexity in an
automation tool to make builds easier. To get an idea of what xtask is doing, you can run
`cargo xtask --help`. Note that this repo's cargo config provides aliases for the common commands,
as we will see below. In fact, it's advisable to NOT use the default cargo commands, and instead run
everything through xtask.

## Step 1: Building the Toolchain

This step takes the longest, but only has to happen once. Run

```
cd where/you/cloned/twizzler
cargo bootstrap
```

and then wait, while you sip your tea. This will compile llvm and bootstrap the rust compiler, both
of which take a long time. At the end, you should see a "build completed successfully" message,
followed by a few lines about building crti and friends.

## Step 2: Building Twizzler

Now that we've got the toolchain built and linked, we can compile the rest of Twizzler. Run

```
cargo build-all
```

which will compile several "collections" of packages:
  1. The build tools, for things like making the initrd.
  2. The kernel.
  3. The userspace applications.

By default all will be built in debug mode, which will run very slow. You can build for release mode
with:

```
cargo build-all --profile release
```

## Step 3: Running Twizzler

You can start Twizzler in Qemu by running

```
cargo start-qemu
```

which will bootup a qemu instance. If you want to run the release mode version, you can run 

```
cargo start-qemu --profile release
```

## Step 4: Exiting Twizzler

At the moment Twizzler does not have a shutdown command.  To exit the QEMU based simulation use the ```Ctrl-a X``` command which is a part of the simulator.
