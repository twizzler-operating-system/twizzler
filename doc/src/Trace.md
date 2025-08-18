# Tracing and Performance Debugging

Twizzler supports tracing facilities supported by the kernel. A utility program, `trace`, enables
users to trace programs and collect statistics, thread sampling events, and other kernel events that
may impact performance. The twizzler-abi crate provides a trace submodule that contains data definitions for tracing events.

Do note that this work is in early stages, and as such, not all defined tracing events may be supported or generated. If you would like one added, feel free to open a PR or issue! Also note
that the standard issues of tracing performance issues apply: you are interfering with the system
by tracing it, and thus it may behave differently under tracing. The tracing system attempts to be
as lightweight as possible so as to minimize this effect, but it cannot be nullified completely.

## Tracing user programs

The `trace` program provides functionality to trace user programs. It allows users to specify the target program and the events to be traced:

```
trace -e syscalls ls
```

This will trace the `ls` program and collect information on each syscall that the tracee makes.
The currently supported events for -e are (specified abbreviations also work): syscalls (sys), page-faults (pf), tlb. More will be added in the future, as well as a flag to ask trace what
events are supported directly.

```
trace -s ls
```

This will sample the threads running in the `ls` compartment at regular intervals and collect statistics on each thread's execution. It may be useful to specify the MONDEBUG=1 environment
variable so as to show which libraries are loaded where.

## TODO

  - Allow changing sampling rate
  - Allow more fine-grained sampling (requires kernel scheduler changes)
  - Provide list of -e events from trace program directly
  - Parse the debug info for the compartment so that we can output symbol names instead of addresses.
