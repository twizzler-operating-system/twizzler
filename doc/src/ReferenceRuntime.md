# The Twizzler Reference Runtime

The primary runtime environment supported by Twizzler is the _Reference Runtime_. It provides userspace functionality for programs, the ability to load programs and libraries, and the ability to isolate those programs and libraries from each other.

It is a work in progress.

## Stdio

The runtime provides three types of stream-like interfaces for basic IO, which should be familiar to most: stdin (for reading input), stdout (for writing output), and stderr (for reporting errors). Each of these can be handled by either a thread-local path, or a global path. When writing to stdout, for example, the runtime first checks if the thread-local path has a handler. If so, the output gets sent to that handler. If not, the runtime checks if there is a global handler registered for stdout. If so, the output goes there. If not, it gets sent to the fallback handler, which can be configured to either drop the output or send it to the kernel log.