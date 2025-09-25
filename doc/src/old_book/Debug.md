# Debugging

Debugging support is in-progress on Twizzler. Currently, you can debug userspace programs via GDB in a limited way.

## Using the debug stub in QEMU

To debug a program in Twizzler from within QEMU, you can run it under the debug stub program:

```
debug run <your-program>
```

The program will start in a suspended state and wait for a debugger connection. From the host machine,
start GDB in the twizzler source directory. This will run the .gdbinit script and will configure GDB for
debugging Twizzler userspace programs. Note that this file may trigger a warning that it needs to be allowed
to run. If that warning pops up, follow its instructions.

At this point, you should see the (gdb) prompt and it should have connected to the debug stub. If not, you can
try connecting manually via `target remote :2159`. Note that many features are missing, but are in the process
of being added.

## Using the debug stub to attach to a crashed compartment

TODO

## Some other debugging techniques

 - Loading of compartments and libraries can be printed by specifying MONDEBUG=1 as an environment variable when starting programs.
 - Many system services use the tracing crate for logging. You can increase the verbosity by changing the log level in the relevant initialization routines. TODO: get these from environment variables.
 - The kernel uses the log crate, and its verbosity is controlled in main.rs.

## Working features

 - Reading registers
 - Reading and poking memory
 - Tracing stopped threads

## Planned additional features

 - Setting / clearing breakpoints (in-progress)
 - Single stepping (in-progress)
 - Memory map reading
 - Multithreaded debugging
 - Signals
 - Support for aarch64
