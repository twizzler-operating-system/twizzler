
# Twizzler Thread Model

Threads in Twizzler are similar to threads in other operating systems, virtualizing a processor into
many units of execution that are scheduled by the kernel. In other words, the kernel runs a number of
kernel-supported threads on the available processors of the system, scheduling them according to some
scheduling policy. A thread is fundamentally architecture-specific, and so cannot migrate across processors
of different architectures.

## Control Objects

A thread has a _control object_ associated with it that also acts as the authoritative name for the thread.
The control object has the following structure:

| Offset from Base |      Type | Description
| ---------------- | --------- | -----------
|                0 |       u32 | Version number, must be 0
|                4 |       u32 | Flags, must be 0
|                8 | AtomicU64 | Status (see below)
|               16 | AtomicU64 | Code (meaning depends on Status)

### Status Values

The value of status reflects the thread's current execution status, and is comprised of the following fields:

| Bits | Description
| ---- | -----------
|  0-7 | Execution State (see below)
| 8-63 | Reserved

The execution state may be one of the following:

- *Running* (execution state value 0). The thread is currently running or ready to run. It may be currently executing or in a scheduling queue waiting to run. The value of `Code` is reserved.
- *Sleeping* (execution state value 1). The thread is currently sleeping, but will automatically return to the running state when some condition is met. The value of `Code` is reserved.
- *Suspended* (execution state value 2). The thread is currently suspended and will not automatically be rescheduled until the execution state
is returned to 0. The value of `Code` is reserved.
- *Exited* (execution state value 255). The thread has exited and will never run again. The value of code is the exit code specified by the thread (see Termination, below).

#### Changing Status Value

The status field is an atomic u64, and thus must be updated using only atomic instructions (using, at minimum, release semantics). Both the kernel and userspace may update the field, however userspace may only do so if the updating thread has permission to write to the control object. After updating the status field, the updating thread must perform a thread-sync wake operation on the status field to ensure the effect occurs. If the kernel is updating the status field, the thread-sync wake will be performed by the kernel automatically. The value of `Code` only has meaning after the thread has exited, will only be written to by the kernel during the termination process, and will be written to before the final write to set the execution state to Exited.

The kernel will update the status value over the thread's lifetime, changing it between running and sleeping as execution evolves until the
thread is terminated through some mechanism, after which the kernel will update the thread's state to exited. The state in only ever changed to suspended by the kernel if a synchronous event occurs and the thread is setup to suspend upon fatal events (see Synchronous Events, below).

Updates to the field from userspace may take effect anytime after the write to the status field occurs, but _will_ be before the return of the
thread-sync wake operation. However, if other write to the status field occurs (either by userspace or by the kernel), the previous effect is
overwritten. One exception is that if the kernel ever witnesses a value of 255 in the execution state field, the thread will exit and future values will be ignored.

## Creation

Threads are spawned via the `sys_spawn` system call, which takes a `ThreadSpawnArgs` struct as an argument and returns a `Result` of `ObjID`
on success and `ThreadSpawnError` on failure. The object ID returned by a successful invocation is the new thread's control object.

The `ThreadSpawnArgs` struct has the following contents:

```{rust}
#[repr(C)]
struct ThreadSpawnArgs {
    entry: usize,
    stack_base: usize,
    stack_size: usize,
    tls: usize,
    arg: usize,
    flags: ThreadSpawnFlags,
    vm_context_handle: Option<ObjID>,
}
```

- entry: a linear address within the new thread's VM context at which to start execution. The ABI for this execution entry must be `extern "C"`, and it must accept a single argument, which will be the `arg` field of the struct.
- stack_base, stack_size: this defines a region in the linear address space that the kernel will set the thread's stack to.
- tls: initial value of the thread-local storage pointer, if one exists for this architecture.
- flags: reserved for future use.
- vm_context_handle: if None, the VM context of the parent thread will be used. If an object ID is specified, and that object is a VM context handle, that VM context will be used.

When spawning a thread, the kernel creates the control object and initializes the structure before returning from `sys_spawn`. Once returned, the new thread is running.

## Termination

Threads may be terminated in several different ways:

- The thread may call the `sys_thread_exit` system call. The code value is specified by the call, and this call _always_ succeeds.
- Another thread writes a value of Exited (255) to the thread's status field. The updating thread is responsible for updating the Code field before updating the Status field (see above for synchronization rules for updating the status field).
- The thread encounters a synchronous event that it cannot or does not handle (see below).
- Power is lost. In this case, the value of Status and Code are irrelevant.

Once a thread is terminated, it never resumes. The thread control object is then subject to standard object deletion and cleanup rules.

## Events

A thread may be sent events, which may be either synchronous or asynchronous.

### Synchronous Events

A synchronous event is one that interrupts execution of the thread immediately, causing it to jump to a different execution location. These
events are typically used to indicate exceptions or memory errors which cannot be ignored. For example, a thread reading some unmapped memory, dividing by zero, trying to read a mapped object which does not exist, etc. The kernel is the only thing that may send a thread a synchronous event; userspace programs wishing to communicate with threads should use the asynchronous event mechanism, detailed below.

When a thread receives a synchronous event, it jumps to the thread's upcall pointer (see Thread Kernel State, below). If no pointer is registered, or an invalid pointer is registered, all synchronous events result in thread termination. If the upcall would cause the stack to be overflowed, the thread is terminated.

A thread may, optionally, choose to be suspended upon receiving a synchronous event. If this is the case, the thread's status field is updated to be Suspended after the stack frame for the upcall is initialized and the instruction pointer is set to the upcall pointer. A thread-sync wake operation is then performed on the status field.

For a list of currently defined synchronous events, see [List of Synchronous Events](SyncEvents.md).

### Asynchronous Events

Asynchronous events are less urgent than synchronous events and are more akin to messages. They are sent to a thread via a Twizzler queue object that the thread registers with the kernel if it wants to receive async events. By default no queue is registered for new threads, they must do so manually. If no queue is registered, all async events are ignored, and the sender is notified via a failure to send an async event.

All async events have the following structure:
```{rust}
struct AsyncEvent {
    sender: ObjID,
    flags: AsyncEventFlags,
    message: u32,
    aux: [u64; 7]
}
```

Standard fields include:

- sender: The sending thread's control object ID.
- flags: standard flags for the message:
   - NonBlocking: the send message call was non-blocking. The receiving thread should still issue a completion notification.

The other two fields, message and aux, contain data that is interpreted by the receiving thread.

The kernel may also send async events to the thread (usually these must be asked for). For a list of pre-defined async events, see
[List of Async Events](AsyncEvents.md).

#### Completion

A thread should, once it is finished processing the async event, submit a completion notification on the queue. If it does not, the queue
could fill up, causing future message send calls to block. The queue completion structure for an asynchronous event contains data that is passed back to the sending thread (should it choose to wait for a completion), and has the following structure:

```{rust}
struct AsyncEventCompletion {
    flags: AsyncEventCompletionFlags,
    status: u32,
    aux: [u64; 7],
}
```

Standard fields include:

- flags: standard flags for the completion notification, reserved for future use.

The other two fields, status and aux, contain data that is interpreted by the original sender, upon return from the call to send.

### Sending an Async Event

When sending an async event, the sending thread calls the `sys_thread_message` syscall, which has the signature:

```{rust}
fn sys_thread_message(target: ObjID, flags: AsyncEventFlags, message: u32, aux: &[u64]) -> Result<AsyncEventCompletion, AsyncMessageError>;
```

If the flags argument contains NonBlocking, then this call returns immediately with `AsyncMessageError::NonBlocking`.

## Thread CPU State

A running thread has a purely volatile CPU state, containing the registers, stack pointer, instruction pointer, etc. This volatile state
is only ever stored loaded in the CPU, or in kernel space inside an in-kernel thread control block. However, the registers _can_ be made
available for other threads to read or write through the use of an introspection system call:

```{rust}
fn sys_thread_read_register(target: ObjID, register: ArchRegister) -> Result<RegisterValue, ThreadRegisterError>;
fn sys_thread_write_register(target: ObjID, register: ArchRegister, value: RegisterValue) -> Result<RegisterInfo, ThreadRegisterError>;
```

The `register` argument specifies which register to read from. The enum `ArchRegister` is an architecture-specific enum that lists
all the registers accessible by this function. The return value is either a `RegisterValue` struct upon success, or a `ThreadRegisterError` on failure. Note that the thread must be suspended for these calls to succeed.

The `RegisterValue` enum allows for different-sized registers, containing either a u8, a u16, etc, up to a u128. Larger registers must be read
in parts.

A thread may call these functions on another thread only if that thread has write permission on the target thread's control object.

## Thread Kernel State

The kernel also maintains some internal state per-thread that can be manipulated or set by userspace. These values may be read or modified via the `sys_thread_control` syscall or helper functions:

- TLS Pointer: This is a pointer that is loaded into an architecture-specific register whenever the thread is running, and is commonly used to denote thread-local storage areas. For example, on x86_64, this pointer value is loaded into the `fs` register upon context switch.
- Upcall Pointer: This pointer is used by the kernel to invoke synchronous events. It is called by the kernel with a special ABI defined in the twizzler-abi crate and should not be used directly by end users.
- Affinity, see below.
- Priority, see below.

These values may be set or read by another thread only if the calling thread has write access to the target thread's control object.

### Affinity

By default, a thread has no specific affinity, and may run anywhere. However, with a call to `sys_thread_control`, a thread may select a specific subset of cores upon which it is allowed to run. This is an _allow_ list, ensuring the thread only runs on CPUs that it has specified as allowed.

### Priority

TODO: discuss with Allen and George a good priority definition and API.