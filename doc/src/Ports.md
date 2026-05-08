# Ports

Twizzler currently supports a number of third-party ported libraries and programs.
The current list may be viewed by running ```cargo toolchain ports```.

## Compiling a specific port

The xtask program currently does not enforce dependencies for these ports, so they
will only build properly if the necessary dependencies are built first.

Note that after compiling a port, you need to update the nvme image with `cargo xtask disk -f reset`.

## Adding a port

Adding a port involves extending the data structures and loops in xtask/src/toolchain/ports.rs,
and then adding a file in xtask/src/toolchain/ports/ to handle building the program or library.
I recommend copying from an existing port file depending on if it's cmake or autotools based.
In the future, we'll abstract these into a better system, but it's small enough for now.

## Notes on ports

Ports are installed (on the host, when compiled) into toolchain/install/sysroot/x86_64-unknown-twizzler/pkg/port-name.
(of course, depending on architecture and port name). When the nvme image is built, xtask will
copy the sysroot/triple directory to nvme:/sysroot, and will be available from within Twizzler under /sysroot (and
so ports can be found in /sysroot/pkg/port-name). The shell automatically searches ports' bin directories for
commands, and libraries are loaded from respective lib directories.
