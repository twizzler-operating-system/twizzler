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
  1. sudo apt install build-essential python cmake ninja-build llvm-18
  2. Install Rust https://www.rust-lang.org/tools/install

Note that we depend on the system LLVM for some initial bindgen commands. The minimum version for this is 18.
On ubuntu, this can be selected for building twizzler by env vars: `export LLVM_CONFIG_PATH=/usr/bin/llvm-config-18`. 
This step is necessary for an AMD CPU machine and the toolchain will fail to compile without it. 

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
git submodule update --init --recursive
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

For the current AMD version, to run the release mode version, you can run

```
cargo start-qemu -p=release -q='-nographic'
```

## Step 4: Exiting Twizzler

At the moment Twizzler does not have a shutdown command.  To exit the QEMU based simulation use the ```Ctrl-a X``` command which is a part of the simulator.
