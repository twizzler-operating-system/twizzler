# Twizzler Architecture

Companion to [repo_index.md](repo_index.md). That file maps every source file to a one-line
description; this file maps how the pieces fit together: the object/permission model the whole
system is built on, the crate dependency graph, the boot sequence, and — per subsystem — the key
types and the closest thing this OS has to "API endpoints" (syscalls, secgate cross-compartment
gates, and queue-based IPC protocols).

## Contents
- [System model, in code terms](#system-model-in-code-terms)
- [Build collections and target triples](#build-collections-and-target-triples)
- [Crate dependency graph](#crate-dependency-graph) / [Boot sequence](#boot-sequence)
- [Kernel](#kernel-srckernel)
- [Core libraries](#core-libraries-srclib--abi-twizzler-dynlink-secgate-security-queue-futures)
- [Driver/server-support libraries](#driverserver-support-libraries-srclib--driver-nvme-rs-io-net-display-virtio-naming-pager-devmgr-logboi)
- [Runtimes & ABI](#runtimes--abi-srcrt-srcabi)
- [Servers & programs](#servers--programs-srcsrv-srcbin)

## System model, in code terms

Full conceptual detail lives in `doc/src/*.md` (Object, Lifetime, Pointer, Permissions, Views, KSO,
Gates, ThreadModel); this is the subset needed to read the code below without getting lost.

- **Objects** are the unit of persistence: related data with shared lifetime/permissions, named by a
  128-bit `ObjID`. The kernel (`src/kernel/src/obj/`) mediates only create/delete and page-level
  mapping; reading/writing mapped memory involves no kernel call. `twizzler::object::Object<Base>`
  (and its transactional/mutable siblings `TxObject`/`MutObject`) is the userspace handle type.
- **Pointers** come in two forms: *persistent* pointers (an `ObjID` + offset, i.e. `GlobalPtr<T>` —
  context-free, like a filename) and *dereferenceable* pointers, which only make sense inside a
  mapped **View** (a per-thread/per-context address space assembled from mapped objects).
  `InvPtr<T>` ("invariant pointer") is the pointer type actually stored *inside* objects on disk —
  it's resolved through an object's **Foreign Object Table (FOT)** rather than being a raw address,
  so it survives remapping across processes/reboots. `Ref`/`RefMut`/`RefSlice`/`TxRef` (in
  `twizzler::ptr::resolved::*`) are the guard types you get back after resolving an `InvPtr`.
- **Permissions**: read/write/execute/use/delete, checked against a thread's attached **security
  context** (`SecurityContext`/`SecCtxMgr` in the kernel, `twizzler-security`'s `Cap`/`SecCtx` in
  userspace — capabilities are ECDSA-signed and cached per-context).
- **Kernel State Objects (KSOs)** are ordinary objects the kernel also reads/writes, given the `use`
  permission — security contexts, device/bus objects, thread control objects are all KSOs.
- **Secure gates** (`secgate`) are how a thread calls from one compartment into another (e.g. a
  client program calling into `naming-srv`) without either side being able to corrupt the other's
  memory: `#[secgate::gatecall]` marks the caller-facing stub, `#[secgate::entry]` marks the real
  implementation, and `dynlink` discovers each library's gate points via an ELF section
  (`.twz_secgate_info`) at load time. The userspace **monitor** (`src/rt/monitor`) owns compartment
  loading/lifecycle and is the thing that actually enforces the boundary.
- **Threads** are control-object-backed (`ThreadRepr`/`ExecutionState`: Sleeping/Running/
  Suspended/Exited — "Running" covers both on-CPU and runnable-on-a-runqueue). The universal
  blocking/wake primitive is `sys_thread_sync` (`ThreadSync`/`ThreadSyncSleep`/`ThreadSyncWake`),
  which every higher-level primitive (mutexes, condvars, queues, device interrupts) is built on.

## Build collections and target triples

(Full detail: `doc/src/CargoMetadata.md`, summarized in root `CLAUDE.md`.) xtask compiles the
workspace as separate **collections**, each with its own target triple, selected per-crate via
`package.metadata.twizzler-build` in `Cargo.toml`:

| Collection | Triple shape | Selector | Examples |
|---|---|---|---|
| Tools | host triple | `"tool"` | xtask, image_builder, initrd_gen, serialtest |
| Kernel | `arch-machine-none` | `"kernel"` | `src/kernel` |
| Userspace (default) | `arch-machine-twizzler` | *(unset)* | most `src/lib`, `src/srv`, `src/bin` crates — dynamically linked against the `reference` runtime |
| Userspace-static | `arch-machine-twizzler-minruntime` | `"static"` | `bootstrap`, `twizzler-minruntime` itself |
| Userspace-tests / Kernel-tests | same as above | *(opt-in, off by default)* | enabled via `cargo start-qemu --tests` |

## Crate dependency graph

Layered by what depends on what (verified directly from every workspace `Cargo.toml`'s
`[dependencies]`/`[build-dependencies]` sections — not inferred). Each layer only depends on layers
above it in this list.

```
L0  Foundation (no internal deps)
    twizzler-types  ──►  twizzler-rt-abi  ──►  twizzler-abi
    (src/abi/types)      (src/abi/rt-abi)      (src/lib/twizzler-abi)

L1  Built directly on twizzler-abi / twizzler-rt-abi
    secgate (+secgate-macros)   twizzler-queue-raw   twizzler-futures
    twizzler (+twizzler-derive)  twizzler-minruntime  twizzler-kernel-macros

L2  One layer up
    dynlink            (secgate, twizzler-abi)
    twizzler-queue      (twizzler-queue-raw, twizzler-futures)
    twizzler-security   (twizzler)
    twizzler-driver      (twizzler, twizzler-futures)
    twizzler-kernel      (twizzler-abi, twizzler-queue-raw, twizzler-security, twizzler-kernel-macros)

L3  Cross-compartment / device-facing clients
    monitor-api  (dynlink, secgate)
    naming-core, pager-dynamic, devmgr, logboi, pager, sgtest, twizzler-io   (secgate / monitor-api)
    nvme-rs (standalone protocol library, no internal deps)

L4  Protocol libraries built on L3
    naming (naming-core)         twizzler-net (twizzler-driver, twizzler-io, twizzler-queue)
    twizzler-display (secgate, twizzler)
    virtio-gpu, virtio-net (devmgr, twizzler-driver, [virtio-net also: twizzler-net])

L5  Runtimes (link L0-L4 into a running program)
    rt (wrapper, forces link against reference's cdylib)
    reference/twz-rt   (dynlink, monitor-api, naming-core, pager-dynamic, twizzler-net)
    monitor             (dynlink, monitor-api, naming-core, twizzler-security)
    twizzler-minruntime  (twizzler-abi, twizzler-rt-abi only — no dynlink/monitor-api/secgate)

L6  System servers (src/srv/*, cdylibs loaded by init via the monitor)
    cache-srv, devmgr-srv, display-srv, logboi-srv, naming-srv, net-srv, pager-srv (+object-store),
    sgtest-srv — each pairs with its L3/L4 client crate of the same base name

L7  Userspace programs (src/bin/*)
    bootstrap → monitor → init → {logboi,devmgr,pager,naming,cache,net,display}-srv → sshd → shell
    plus ~30 standalone tools/tests/demos consuming the client crates above
```

### Boot sequence

Traced directly from `src/bin/bootstrap/src/main.rs` and `src/bin/init/src/main.rs`:

1. Kernel loads and jumps to **bootstrap** (`Userspace-static`, no dynlink context yet of its own).
2. Bootstrap resolves `libtwz_rt.so` and `monitor` from kernel init info, builds a `RuntimeInfo`, and
   jumps into the loaded **monitor**.
3. Monitor starts **init**.
4. Init loads servers in dependency order via `monitor_api::CompartmentLoader`, blocking on each
   compartment's `READY` flag before continuing: **logboi-srv → devmgr-srv → pager-srv**
   (`pager_start`) **→ naming-srv** (`namer_start`, needs pager's bootstrap object ID) → [symlink
   `/pkg`, `/sysroot`, `/etc`; cache library name→ObjID mappings] **→ cache-srv → net-srv →
   display-srv → sshd** → optionally **unittest** (if booted with `--tests`/`--bench(es)`) → symlink
   `uuhelper` under every coreutils name → set up a PTY on the kernel console → loop launching
   **shell** (restarting on exit), or run a single named `autostart` program instead.
5. `naming-srv`'s "external" (POSIX-file-backed) namespace support depends on `pager-srv`'s
   `pager_lookup_external`/`pager_create_external`/etc. gates; `pager-srv` and `net-srv` both depend
   on `devmgr-srv` (`get_devices`) to find their NVMe/virtio-net PCIe hardware; `display-srv` talks
   to its virtio-gpu device directly, bypassing devmgr.

---

## Kernel (src/kernel)

### Key structs/enums/traits
- `Object` (obj/mod.rs) — the in-kernel representation of a Twizzler object: backing pages, page tables, metadata, notes, tie tracking. Nearly all kernel subsystems (memory, syscalls, pager, security) operate on `ObjectRef = Arc<Object>`.
- `ObjectPageTable` (obj/pagetables.rs) — per-object page table wrapper around the generic `Mapper`; owns COW setup, dirty tracking (`DirtyList`), and invalidation batching for a single object's backing pages.
- `PageNumber` (obj/mod.rs) — typed page-index-within-object newtype used throughout object data/page-table code.
- `Table`/`Entry`/`EntryFlags` (arch/{amd64,aarch64}/memory/pagetables/{table,entry}.rs) — arch-specific page-table leaf/level representations; abstracted over by the generic `Mapper`.
- `Mapper` (memory/pagetables/mapper.rs) — the architecture-independent page-table manipulation API (map/unmap/change/object_map/cow/zero-range) built on top of arch `Table`/`Entry`.
- `MappingCursor` / `MappingSettings` / `MappingFlags` (memory/pagetables/{cursor,settings}.rs) — the generic vocabulary (address range + perms/cache/flags) used to describe a mapping operation across all page-table code.
- `Consistency` / `DeferredUnmappingOps` (memory/pagetables/consistency.rs) — batches TLB/cache-line invalidation work generated by a mapping change and defers frame frees until after invalidation is safely complete.
- `VirtContext` (memory/context/virtmem.rs) — the concrete `UserContext`+`KernelMemoryContext` implementation: per-security-context slot table (`Slot`), region manager, arch page tables; this is "the" memory context type referenced elsewhere as `ContextRef`.
- `MapRegion` / `RegionManager` (memory/context/virtmem/region.rs) — one object-mapping-into-a-context record, and the interval-tree collection of them per `VirtContext`.
- `Frame` / `FrameRef` (memory/frame.rs) — per-physical-page metadata (refcount, COW/zeroed/wired/kernel flags) and its static-lifetime reference type; the physical-memory currency of the whole kernel.
- `FrameAllocator` / `MemoryTracker` (memory/tracker.rs) — precharge-based physical frame allocation with a background `ReclaimThread` for memory-pressure handling.
- `Thread` / `ThreadRef` (thread.rs) — the thread control block: `ExecutionState`, priority, flags, critical-section depth, arch register state, links into every wait/run structure it can simultaneously belong to.
- `RunQueue<N>` (processor/rq.rs) — per-CPU array of intrusive linked lists forming the realtime/background scheduler queues; `TimeshareQueue<N>` (processor/timeshare.rs) is the calendar-queue variant used for User-class threads.
- `Processor` (processor.rs) — per-CPU kernel state: current thread, idle thread, stats, rebalance flag; indexed by `all_processors()`/`current_processor()`.
- `SleepInfo` / `ThreadSleepLinker` / `SleepEntry` (obj/thread_sync.rs) — per-object, per-offset wait-word sleep queues (`RBTree` keyed by byte offset) that `sys_thread_sync` sleeps/wakes threads on; a thread can be linked into one of these while also being off the run queue.
- `Requeue` (syscall/sync.rs) — the deferred-wake list threads are placed on so that waking can happen outside a lock/interrupt-disabled critical section, then be pushed back onto the scheduler via `requeue_all`.
- `CondVar` / `InnerCondVar` (condvar.rs) — condition variable keyed by thread ObjID over an intrusive `RBTree` of waiting `ThreadRef`s; layered under `Mutex`.
- `Mutex<T>` / `LockGuard` (mutex.rs) — sleeping mutex; explicitly documented as unsafe to use in critical/interrupt contexts because it can put the current thread to sleep.
- `GenericSpinlock<T, Relax>` (spinlock.rs) — the base spinning lock, parameterized by a `RelaxStrategy` (`SpinLoop` vs. `Reschedule`, the latter yielding to the scheduler after enough spin iterations).
- `LockTracker` / `LockTrackerInner` / `Lock` (thread/locktrack.rs) — per-thread bookkeeping of currently-held and intended-to-acquire mutexes/spinlocks, used to detect stuck/deadlocked lock waits.
- `SecurityContext` / `SecCtxMgr` (security.rs) — capability-based security context object and the per-thread manager tracking active/inactive attached contexts; wraps `twizzler-security`'s `Cap`/`SecCtxBase`.
- `Inflight` / `InflightManager` / `ReqKind` / `Request` (pager/{inflight,request}.rs) — the kernel-side state machine for requests outstanding to the userspace pager server (page-in, sync, create, delete, pager-memory).
- `TraceMgr` / `TraceSink` / `TraceEvent<T>` (trace/{mgr,sink}.rs) — the kernel tracing subsystem: per-target sinks (object-backed ring buffers) fed by sync/async enqueue paths and a dedicated flush thread.
- `IsrContext` (amd64) / `ExceptionContext` (aarch64) — arch-specific trap/exception register frames, both convertible to the arch-neutral `UpcallFrame` used to deliver faults/signals to userspace.
- `Once<T>` / `OnceWait<T>` (once.rs) — one-time lazy-init primitives used pervasively for global singletons (`static X: Once<T>`) across nearly every subsystem.

### Syscall surface
Dispatch table lives in `syscall/mod.rs::syscall_entry`, matching on `twizzler_abi::syscall::Syscall`:
- `Null` — no-op.
- `ObjectCreate` — create a new object (`syscall::object::sys_object_create`).
- `ObjectMap` / `ObjectUnmap` / `ObjectReadMap` — map/unmap an object into slots of a memory context; read back mapping info.
- `ObjectCtrl` — object control commands (sync, delete, preload, add/remove/get/enumerate notes) via `object::object_ctrl`.
- `MapCtrl` — control commands on a mapped region (`object::map_ctrl`).
- `ObjectStat` — populate `ObjectStats`.
- `Enumerate` — enumerate objects or threads (`obj::enumerate_objects` / `thread::enumerate_objects`).
- `Spawn` — spawn a new user thread (`syscall::thread::sys_spawn` → `thread::entry::start_new_user`).
- `ThreadCtrl` — thread control operations (`syscall::thread::thread_ctrl`): get/set upcall target, resume from upcall, get/set TLS, exit, yield, get self ID, get/set active security context, get stats, read registers, change execution state, get/set trace events, send message.
- `ThreadSync` — the futex-like wait/wake primitive (`syscall::sync::sys_thread_sync`), operating on object-relative or virtual-address wait words.
- `NewHandle` / `UnbindHandle` — acquire/release kernel handles (e.g. VM-context or pager-queue handles).
- `SctxAttach` — attach a security context to the current thread.
- `Kaction` — device/KSO control-plane dispatch (`device::kaction`).
- `KernelConsoleWrite` / `KernelConsoleRead` — kernel console I/O.
- `Ktrace` — add/remove a trace sink (`trace::sys::sys_ktrace`).
- `ReadClockInfo` / `ReadClockList` — clock enumeration/info (`clock::fill_with_*`).
- `GetRandom` — CSPRNG output (`random::getrandom`).
- `SysInfo` — general/memory/thread/security/lock/syscall/object stats (`syscall::stat::write_sys_info_values`).

### Critical module relationships
- `syscall/` is the top of the call graph for userspace-triggered work: it calls into `obj/`, `memory/context/`, `security.rs`, `thread.rs`, `pager.rs`, and `trace/`, but nothing calls back into `syscall/` from those lower layers (syscall handlers are leaves from those modules' perspective).
- `obj/` (objects) sits below `memory/` conceptually but is intertwined with it: `obj::pagetables::ObjectPageTable` is built directly on `memory::pagetables::Mapper`, and `memory::context::virtmem::VirtContext` maps `ObjectRef`s into per-context `Slot`s via `MapRegion`. Page faults (`memory::context::virtmem::fault`) drive both COW handling in `obj::data`/`obj::pagetables` and pager requests in `pager.rs`.
- `memory::frame` (physical frames) underlies `memory::tracker` (allocation policy/reclaim) which underlies both `memory::pagetables` (page-table-frame allocation) and `obj::data`/`obj::pagetables` (object backing-page allocation).
- `arch::memory::pagetables::{Entry,Table}` and `arch::context::ArchContext` are the only pieces of the page-table stack that differ per-ISA; everything in `memory::pagetables::*` (Mapper, cursor, consistency, reader, settings) is written against those two arch types generically.
- `processor/sched.rs` is the scheduler brain; it's invoked from `spinlock.rs` (`Reschedule` relax strategy), `mutex.rs`/`condvar.rs` (sleep/wake), `interrupt.rs` (`schedule_maybe_preempt`), `clock.rs` (hard/stat ticks), and `syscall::sync` (`finish_blocking`/`requeue_all`). It in turn depends on `processor::rq`/`processor::timeshare` for queue storage and `processor::ipi` for cross-CPU rebalancing/wakeups.
- Threads can simultaneously sit on several independent hand-rolled intrusive-linked-list structures: the scheduler run queue (`processor::rq::RunQueue`/`timeshare::TimeshareQueue`), a mutex's internal wait list (`mutex.rs`), a `CondVar`'s `RBTree` (`condvar.rs`), the suspended-threads `RBTree` (`thread::suspend`), an object's wait-word `SleepEntry`/`RBTree` (`obj::thread_sync`), and the deferred-wake `Requeue` list (`syscall::sync`). Code adding/removing a thread from any one of these needs to check the others for add/remove symmetry — CLAUDE.md flags this as an easy source of "orphaned"/stuck-thread bugs, and it's visible directly in the code: e.g. `syscall::sync::finish_blocking` and `thread::suspend::unsuspend_thread` both call `requeue_all`/`add_to_requeue` after manipulating a thread's membership in one of the other structures, precisely to route it back onto the run queue.
- `pager.rs`/`pager/*` bridges the kernel object system and a userspace pager process: `obj::data::ensure_in_core_pager` and `memory::context::virtmem::fault` call into `pager::ensure_in_core`/`lookup_object_and_wait`, which create `pager::request::Request`s tracked by `pager::inflight::InflightManager` and shipped to userspace via `pager::queues` (built on `queue.rs`'s `QueueObject`/`ManagedQueueSender`).
- `security.rs` (`SecCtxMgr`) is consulted by both the page-fault path (`memory::context::virtmem::fault` calls `SecCtxMgr::search_access`) and `syscall::object`/`syscall::thread` for permission checks and context switching; it depends on `memory::context` to map/read the security-context KSO itself.
- `trace/` is a cross-cutting concern instrumented from many subsystems (`memory::allocator`, `memory::pagetables::consistency`, `memory::context::virtmem::region`, `thread.rs`, `syscall::mod`/`syscall::sync`) via `trace::new_trace_entry` + `trace::mgr::TRACE_MGR`; it does not gate or block those subsystems (best-effort async enqueue with an internal flush thread).
- `once.rs`/`spinlock.rs`/`mutex.rs` form the low-level synchronization layer nearly everything else (including the scheduler itself) is built on; `spinlock.rs`'s `Reschedule` strategy creates a dependency from the lowest sync primitive back up into `processor::sched`, so lock ordering discipline (see `utils::lock_two`/`spinlock_two`, which canonicalize lock order by address to avoid deadlock cycles) matters kernel-wide, not just within a single subsystem.

---

## Core libraries (src/lib — abi, twizzler, dynlink, secgate, security, queue, futures)

### Key structs/enums/traits

- `Syscall` (twizzler-abi/syscall/mod.rs) — the enum enumerating every synchronous kernel syscall; `as u64` gives the syscall number passed to `raw_syscall`.
- `ThreadSync`/`ThreadSyncSleep`/`ThreadSyncWake`/`ThreadSyncOp`/`ThreadSyncReference` (twizzler-abi/syscall/thread_sync.rs) — the universal blocking/wakeup primitive: sleep on or wake threads waiting on a memory word's value; underlies mutexes, queues, device interrupts, thread-exit waits.
- `UpcallInfo`/`UpcallData`/`UpcallTarget`/`UpcallFrame` (twizzler-abi/upcall.rs, arch/*/upcall.rs) — kernel->userspace exception/signal delivery ABI.
- `KsoHdr`/`KactionCmd`/`KactionValue` (twizzler-abi/kso.rs) — Kernel State Object header and the `kaction` device-control command ABI.
- `RequestFromKernel`/`CompletionToPager`/`ObjectInfo` (twizzler-abi/pager.rs) — the pager <-> kernel wire protocol structures (paging requests/completions).
- `ThreadRepr`/`ExecutionState` (twizzler-abi/thread.rs) — thread control object layout and the Running/Sleeping/Suspended/Exited state machine.
- `DeviceRepr`/`PcieDeviceHeader` (twizzler-abi/device/*, device/bus/pcie.rs) — device/bus KSO representations used by userspace drivers.
- `Object<Base>` (twizzler/object/object.rs) — the primary handle to a mapped Twizzler object, generic over its base type; converts to `TxObject`/`MutObject`.
- `TxObject<T>` (twizzler/object/tx.rs) — a transactional object handle with commit/abort semantics and in-transaction bump allocation.
- `MutObject<Base>` (twizzler/object/mutable.rs) — mutable-mapped object handle with explicit `sync()` (durability flush).
- `ObjectBuilder<Base>` (twizzler/object/builder.rs) — fluent constructor for new objects (sources, ties, persistence, notes).
- `GlobalPtr<T>` (twizzler/ptr/global.rs) — an (ObjID, offset) address — the "portable" pointer representation.
- `InvPtr<T>` (twizzler/ptr/invariant.rs) — the invariant pointer actually stored inside persistent objects, resolved via the foreign-object-table (FOT).
- `Ref<'obj,T>` / `RefMut<'obj,T>` / `RefSlice` / `RefSliceMut` / `TxRef<T>` / `TxRefSlice<T>` (twizzler/ptr/resolved/*) — the family of "resolved pointer" guard types: dereferenceable views of an `InvPtr`/`GlobalPtr`, split by read/write and transactional/non-transactional.
- `Allocator` trait / `ArenaAllocator` / `InvBox<T,A>` / `OwnedGlobalPtr<T,A>` (twizzler/alloc*) — the persistent-object allocation abstraction and owning smart-pointer types built on it.
- `Vec<T,Alloc>` / `VecObject<T,A>` (twizzler/collections/vec*) — persistent growable array, both as a bare base type and as a full object wrapper.
- `PersistentHashMap<K,V,S,A>` / `RawTable` (twizzler/collections/hachage/*) — SwissTable-style persistent hash map.
- `Context` (dynlink/context.rs) — the dynamic linker's top-level state: a graph of `Library` nodes grouped into `Compartment`s.
- `Library` / `UnloadedLibrary` / `LibraryId` (dynlink/library.rs) — a loaded/not-yet-loaded ELF DSO and its identity within the `Context` graph.
- `Compartment` / `CompartmentId` (dynlink/compartment.rs) — an isolation unit grouping libraries, with its own TLS generation state.
- `ContextEngine` trait / `Engine` (dynlink/engines/mod.rs, engines/twizzler.rs) — pluggable, system-specific segment-loading backend (the Twizzler implementation creates backing objects via `sys_object_create`).
- `RuntimeInitInfo` (dynlink/context/runtime.rs) — handoff struct given to a freshly-linked runtime (TLS region, ctors, used slots).
- `SecGateInfo<F>` / `RawSecGateInfo` (secgate/src/lib.rs) — the ELF-section-embedded descriptor of a single secure gate, discovered by `dynlink`/monitor during compartment loading.
- `Arguments<Args>` / `Return<T>` / `Crossing` (secgate/src/lib.rs) — stack-marshalled argument/return containers for a gate call, and the auto trait restricting which types are allowed to cross a compartment boundary.
- `GateCallInfo` (secgate/src/lib.rs) — caller thread ID + source security-context ID passed into every gate invocation.
- `DynamicSecGate<'comp,A,R>` (secgate/src/lib.rs) — a runtime-resolved, callable (`Fn`/`FnMut`/`FnOnce`) handle to a secure gate, used e.g. by `twizzler::pager::sync_object`.
- `HandleMgr<ServerData>` / `Handle` trait (secgate/src/util/handle.rs) — generic server-side descriptor table for compartment-exposed handles.
- `SimpleBuffer` (secgate/src/util/buffer.rs) — shared-memory-object byte buffer for variable-length cross-compartment data.
- `Cap` (twizzler-security/src/capability.rs) — a signed capability granting a security context access to an object.
- `SecCtxBase` / `SecCtx` (twizzler-security/src/sec_ctx/base.rs, user.rs) — raw and userspace-object-backed security context representations holding capabilities/delegations/masks.
- `SigningKey` / `VerifyingKey` / `Signature` (twizzler-security/src/keys/*) — ECDSA (p256) key and signature types used to sign/verify capabilities; a `VerifyingKey` doubles as an object's "kuid".
- `PermsInfo` (twizzler-security/src/sec_ctx/mod.rs) — effective provide/restrict `Protections` computed for an object under a given security context.
- `QueueEntry<T>` / `RawQueue<T>` / `RawQueueHdr` (twizzler-queue-raw/src/lib.rs) — the lock-free MPSC ring-buffer primitive (submission/completion slot format, head/tail/bell/turn state).
- `Queue<S,C>` (twizzler-queue/src/queue.rs) — an object-backed duplex submission+completion queue pair with thread-sync-based blocking.
- `QueueSender<S,C>` / `CallbackQueueReceiver<S,C>` (twizzler-queue/src/sender_queue.rs, callback_queue.rs) — async sender/receiver wrappers around `Queue`, built on `async-io`.
- `TwizzlerWaitable` trait (twizzler-futures/src/lib.rs) — bridges any `AsRawFd`-like Twizzler object to a `ThreadSyncSleep` + readiness bool for use by async executors.

### API surface (syscalls / secgate gates / public entry points)

- **twizzler-abi syscalls**: the `Syscall` enum (`syscall/mod.rs`) enumerates the full table — `Null`, `KernelConsoleRead`/`Write`, `ThreadSync`, `ThreadCtrl`, `ObjectCreate`/`Map`/`Unmap`/`Ctrl`/`Stat`/`ReadMap`, `SysInfo`, `Spawn`, `ReadClockInfo`/`ReadClockList`, `Kaction`, `NewHandle`/`UnbindHandle`, `SctxAttach`, `GetRandom`, `MapCtrl`, `Ktrace`, `Enumerate`. Each is wrapped by a typed `sys_*` function in the corresponding `syscall/*.rs` submodule (e.g. `sys_object_create`, `sys_thread_sync`, `sys_kaction`, `sys_read_clock_info`), which packs arguments into a `[u64; N]`, calls arch-specific `raw_syscall`, and decodes the two-register `(code, val)` return into a typed `Result` via `convert_codes_to_result`. Categories: object lifecycle (create/map/unmap/ctrl/stat/notes), thread control (spawn/exit/yield/tls/registers/upcall/sync/state), security context attach, device kaction/interrupts, console I/O, clocks, random, tracing, and generic handle management.
- **secgate gate-call mechanism**: a cross-compartment call is declared by writing the caller-facing stub with `#[secgate::gatecall]` (in the library/API crate) and the real implementation with `#[secgate::entry]` (in the server compartment). The `gatecall` macro rewrites the function body to allocate `GateCallInfo`/`Arguments`/`Return` on the stack (via `alloca`), inline-`asm!("call ...")` into a generated `extern "C"` trampoline, and unwrap the result. The `entry` macro generates the `extern "C"` trampoline that reads arguments, invokes the real (renamed) implementation, writes the return value, and records a `SecGateInfo`/`RawSecGateInfo` entry in the `.twz_secgate_info` ELF section so `dynlink`/the monitor can enumerate and manage gate call points per-library (`Library::iter_secgates`, `AllowedGates`). At runtime, `secgate::frame()`/`restore_frame()` snapshot/restore the calling thread's TLS pointer and active security context around the call; `DynamicSecGate<A,R>` provides a `Fn`-like handle to a gate resolved dynamically at runtime (address-based) rather than statically linked, used by e.g. `twizzler::pager::sync_object` to call into the pager compartment via `monitor_api::CompartmentHandle`.
- **twizzler-queue submission/completion API**: a `Queue<S,C>` sits on top of two `RawQueue`s (submission-of-`S`, completion-of-`C`) inside one mapped object, initialized via `Queue::init`. A producer calls `submit(id, item, flags)` to enqueue a request and later `get_completion(flags)` to retrieve a `(id, C)` completion; a consumer calls `receive(flags)`/`complete(id, item, flags)`. Blocking uses `sys_thread_sync` via `setup_read_sub_sleep`/`setup_write_com_sleep`-style helpers that produce a `ThreadSyncSleep`. `QueueSender<S,C>` (client side) and `CallbackQueueReceiver<S,C>` (server side) layer async/await ergonomics on top via `twizzler_futures::TwizzlerWaitable` + `async-io`.

### Critical relationships

- `twizzler-abi` is the foundation: `dynlink`, `secgate`, `twizzler`, `twizzler-security`, `twizzler-queue-raw`, `twizzler-queue`, and `twizzler-futures` all depend on it directly (per their `Cargo.toml`s) for `ObjID`, syscalls, and thread-sync primitives; it has no dependency on any other crate in this file set.
- `twizzler` (high-level object API) depends on `twizzler-derive` (proc macros for `Invariant`/`BaseType`) and, on Twizzler targets, `twizzler-abi`; `twizzler-security`'s `user` feature depends on `twizzler` for object creation/mapping, and `secgate`'s `SimpleBuffer`/gate marshalling is used by `twizzler::pager` to call into the pager compartment.
- `dynlink` depends on `secgate` (reads `RawSecGateInfo` out of each `Library`'s ELF `.twz_secgate_info` section to discover/enumerate gate call points during loading) and on `twizzler-abi` (object creation/mapping to back loaded segments, via its `engines::twizzler::Engine`). Per CLAUDE.md, `dynlink` is used by `src/rt/monitor` (outside this file set) to load and manage compartments.
- `secgate` depends only on `twizzler-abi`/`twizzler-rt-abi` (plus `alloca` for stack marshalling) — it has no dependency on `twizzler` or `dynlink`, keeping it usable from minimal/static runtimes; `dynlink` and (per CLAUDE.md) the monitor are its consumers for compartment-boundary enforcement.
- `twizzler-queue` depends on `twizzler-queue-raw` (the allocation/object-agnostic lock-free algorithm) plus `twizzler-abi`/`twizzler-rt-abi` (to back the queue with a real mapped object and to block via `sys_thread_sync`) and `twizzler-futures` (for async wrapper support, `CallbackQueueReceiver`/`QueueSender`). Per CLAUDE.md, this queue mechanism is what userspace servers (pager, naming, devmgr, etc., in `src/srv`) use to communicate with clients.
- `twizzler-futures` depends only on `twizzler-abi`/`twizzler-rt-abi` and is consumed by `twizzler-queue` (`CallbackQueueReceiver` implements its `TwizzlerWaitable` trait) to bridge Twizzler's thread-sync blocking model into `async-io`/`futures`-based executors.
- `twizzler-security` depends on `twizzler-abi` always, and additionally `twizzler` under its `user` feature (for object-backed `SecCtx`) or is built `no_std`/kernel-only under its `kernel` feature (mutually exclusive with `user`) — allowing the same capability/signature logic to run in both the kernel and userspace.
- `twizzler-abi/src/security.rs`'s minimal `SecurityContextBase`/`Permissions` is a bare ABI-level shim; the full capability/signing/security-context model lives in the separate `twizzler-security` crate, which builds on top of `twizzler-abi::object::ObjID`/`Protections`.
- Many "primary" types (`ObjID`, `Protections`, `ObjectHandle`, `MapFlags`, `ObjectCreate`, `BackingType`, `LifetimeType`, marker traits `Invariant`/`BaseType`/`StoreCopy`) are actually defined in `twizzler-rt-abi` (`src/abi/rt-abi`, outside this survey's scope) and simply re-exported through `twizzler-abi`/`twizzler`'s modules (e.g. `twizzler-abi/src/object.rs`, `twizzler-abi/src/meta.rs`, `twizzler/src/marker.rs`).

---

## Driver/server-support libraries (src/lib — driver, nvme-rs, io, net, display, virtio-*, naming, pager, devmgr, logboi)

### Key structs/enums/traits

- `RequestDriver` (twizzler-driver/request/mod.rs) — trait a hardware driver implements (`submit`/`flush`, associated `Request`/`Response`/`SubmitError` types) to plug into the generic async `Requester` request-tracking machinery; used by NVMe/e1000/virtio drivers alike.
- `Requester<T: RequestDriver>` (twizzler-driver/request/requester.rs) — generic async submit/collect engine sitting on top of a `RequestDriver`; assigns request IDs, matches responses, produces `SubmitSummary(WithResponses)`.
- `Device` (twizzler-driver/device/mod.rs) — handle to a device KSO; root type for bus/device enumeration, MMIO mapping, interrupt allocation, PCIe capability walking.
- `DeviceController` (twizzler-driver/controller.rs) — pairs a `Device` with its `DeviceEventStream` (interrupts + mailbox).
- `DmaPool` / `DmaRegion<T>` / `DmaSliceRegion<T>` / `DmaPin` (twizzler-driver/dma/*.rs) — the DMA memory abstraction: pool-allocate typed regions, pin them to get stable physical addresses, sync for coherence. Consumed by nvme-rs-based disk drivers, virtio-gpu, virtio-net.
- `DeviceSync` (twizzler-driver/dma/mod.rs) — auto-trait marking types safe to share with a device over DMA (explicitly *not* implemented for raw pointers/references/`UnsafeCell`).
- `CommonCommand` / `CommonCompletion` (nvme-rs/ds/queue/{subentry,comentry}.rs) — the generic 64B NVMe submission entry / 16B completion entry that all NVMe command builders (`Identify`, `ReadCommand`, `WriteCommand`, `CreateIOSubmissionQueue`, etc.) convert into.
- `SubmissionQueue` / `CompletionQueue` (nvme-rs/queue/mod.rs) — raw ring-buffer views for NVMe hardware queues.
- `IdentifyControllerDataStructure` / `IdentifyNamespaceDataStructure` (nvme-rs/ds/identify/*.rs) — parsed NVMe Identify response layouts.
- `VolatileBuffer<N>` (twizzler-io/buffer.rs) — generic ring buffer over object memory with `ThreadSyncSleep`-based blocking; the low-level primitive underneath `Pipe`, PTYs and net packet signaling.
- `PacketObject` (twizzler-io/packet.rs) — fixed-slot packet pool object; reused both directly (twizzler-net) and DMA-wrapped as `DmaPacketObject` (twizzler-net/drivers.rs).
- `PtyBase` / `PtyClientHandle` / `PtyServerHandle` (twizzler-io/pty.rs) — full PTY master/slave object model with termios-driven line discipline converters.
- `NetDriver` (twizzler-net/drivers.rs) — trait implemented by hardware NIC drivers (`E1000Device` in-tree, `virtio-net`'s `DeviceWrapper`) to plug into the twizzler-net stack.
- `NetClient` / `NetServer` (twizzler-net/client.rs, server.rs) — client/server endpoints, both implement `smoltcp::phy::Device`, connected via `Pair`/`PairInner` (twizzler-net/endpoint.rs) packet-object+queue pairs.
- `ClientMsg`/`ServerMsg`/`ClientMsgKind`/`ServerMsgKind` (twizzler-net/lib.rs) — the client<->net-server control message envelope.
- `BufferObject` / `DisplayBuffer` / `Rect` / `WindowConfig` / `WindowHandle` (twizzler-display/lib.rs) — shared-framebuffer object, damage-rectangle tracking, and window handle protocol types for the compositor.
- `NamerAPI` (naming-core/api.rs) — the naming protocol trait, implemented once statically (`naming::StaticNamingAPI`, direct `#[secgate::gatecall]` calls) and once dynamically (`naming-core::dynamic::DynamicNamerAPI`, `DynamicSecGate` handles resolved at runtime).
- `NameStore` / `NameSession` / `NsNode` / `NsNodeKind` (naming-core/store.rs) — the actual namespace tree data structure and its session-scoped operations (put/get/remove/rename/link/enumerate), with symlink resolution.
- `Namespace` trait, impl'd by `NamespaceObject` (naming-core/store/nsobj.rs, object-backed) and `ExtNamespace` (naming-core/store/ext.rs, cached external/pager-backed namespace).
- `PagerHandle` / `ExternalFile` / `ExternalKind` (pager-dynamic/lib.rs) — client handle and file-metadata record type for the pager's "external file" (POSIX-like) namespace support.
- `DriverSpec` / `Supported` / `OwnedDevice` (devmgr/lib.rs) — device-manager query spec and result record type.
- `LogHandle` (logboi/lib.rs) — client handle to the log service, wraps a `SimpleBuffer`.
- `TwzHal` (virtio-gpu/hal.rs, virtio-net/hal.rs — two independent impls of the same pattern) — implements `virtio_drivers::Hal` on top of `twizzler_driver::dma::DmaPool`.
- `TwizzlerTransport` (virtio-gpu/transport.rs, virtio-net/transport.rs) — implements `virtio_drivers::transport::Transport` over a Twizzler PCIe `Device`.

### API surface (protocol messages / client APIs)

- **twizzler-driver**: not a client/server protocol crate itself — it's the substrate (`Device`, DMA, `Requester`) that driver processes use to talk directly to hardware KSOs/MMIO/interrupts; no IPC message types of its own.
- **nvme-rs**: pure wire-format library — `CommonCommand`/`CommonCompletion` plus opcode-specific builders (`Identify`, `ReadCommand`, `WriteCommand`, `CreateIOSubmissionQueue`, `CreateIOCompletionQueue`, `SetFeatures`, `DatasetMgmtCommand`) are the "protocol messages" exchanged with real NVMe hardware queues (via `twizzler-driver`'s DMA/queue primitives in `pager-srv`).
- **twizzler-io**: no network protocol per se; `PacketObject`/`VolatileBuffer`/`Pipe`/PTY types are shared-memory object layouts + sync primitives consumed directly by twizzler-net and terminal programs.
- **twizzler-net**: client<->net-server messages are `ClientMsg`/`ServerMsg` (kind enums `ClientMsgKind`/`ServerMsgKind`) carried over a `twizzler-queue`-based `Pair<S,C>`; bulk packet data flows through shared `PacketObject`/`PacketSet` rather than in the message itself. Hardware-facing side is the `NetDriver` trait (`send_packets`/`recv_packets`-style).
- **twizzler-display**: secgate-call protocol — `create_window`/`drop_window`/`reconfigure_window`/`get_window_config`/`get_display_info`, each taking/returning `WindowConfig`/`ObjID`/handle keys; bulk pixel data flows through a shared `BufferObject` framebuffer with `Rect` damage tracking, not through the gate calls.
- **naming**: `NamerAPI` trait surface — `put`/`get`/`mkns`/`link`/`remove`/`rename`/`change_namespace`/`enumerate_names(_nsid)`/`open_handle`/`close_handle`; paths are passed as `(Descriptor, name_len)` where the actual path bytes live in a shared `SimpleBuffer` referenced by the descriptor, not inline in the call.
- **pager**: `pager_start`, `adv_lethe`, `disk_len`, `pager_open_handle`/`close_handle`, and the "external file" namespace-shadowing calls `pager_enumerate_external`/`pager_lookup_external`/`pager_create_external`/`pager_unlink_external`/`pager_readlink_external` (same `Descriptor`+shared-buffer pattern as naming); `pager-dynamic::ExternalFile`/`ExternalKind` describe the external file metadata records returned/enumerated.
- **devmgr**: single query call `get_devices(DriverSpec) -> ObjID` (a `VecObject<OwnedDevice>`), wrapped client-side by `enumerate_devices()` into an iterable vector object — plus a `devmgr_start()` bootstrap call.
- **logboi**: `logboi_open_handle`/`logboi_close_handle`/`logboi_post(desc, buf_len)` — log message bytes are written into a `SimpleBuffer` first, then `logboi_post` tells the server how many bytes to consume.
- **sgtest**: single-call demo — `foo(Foo) -> Result<u32, TwzError>`.

### Critical relationships

- `twizzler-driver` is the foundational dependency for all hardware-facing crates here: `nvme-rs`-based disk code, `twizzler-net`'s e1000 driver, and both `virtio-gpu`/`virtio-net` (via their `DmaPool`-backed `TwzHal`) depend on it for device/bus access, MMIO, interrupts, and DMA.
- `virtio-gpu` and `virtio-net` both depend on `devmgr` (to find their PCI device via `devmgr-srv`) and duplicate near-identical `hal.rs`/`transport/virtio_pcie.rs` code (the virtio-over-PCI plumbing is not factored into a shared crate).
- `virtio-net` additionally depends on `twizzler-net` (implements its `NetDriver` trait in `tcp.rs`) and `twizzler` — it is the concrete NIC driver plugged into the twizzler-net stack; `twizzler-net` itself ships a second, in-tree `NetDriver` impl (`E1000Device` in `drivers/e1000.rs`) so it doesn't strictly require virtio-net.
- `twizzler-net` depends on `twizzler-io` (for `PacketObject`), `twizzler-queue` (for the client/server message queue), and `twizzler-driver` (for the e1000 driver's DMA/device access); pairs with `src/srv/net-srv`.
- `twizzler-display` pairs with `src/srv/display-srv` (confirmed via display-srv's Cargo.toml dependency) — a self-contained secgate-call + shared-framebuffer-object protocol with no dependency on twizzler-driver (compositor runs above the device layer).
- `logboi` pairs with `src/srv/logboi-srv` (confirmed dependency) — simplest example of the `#[secgate::gatecall]` stub + `SimpleBuffer` client pattern used throughout.
- `naming` (static gatecall stubs) and `naming-core` (protocol trait + `NameStore` tree + dynamic dispatch) together pair with `src/srv/naming-srv`, which depends on both plus `pager` (confirmed) — naming-srv uses the pager to back "external" namespaces (POSIX-like files) via `naming-core::store::ext` and `pager-dynamic`.
- `naming-core` depends on `pager-dynamic` directly (not `pager`), i.e. naming-core talks to the pager service using the dynamic-dispatch client pattern regardless of whether `naming` itself is statically or dynamically linked.
- `pager` (static gatecall stubs) and `pager-dynamic` (dynamic-dispatch client + `ExternalFile` types) both pair with `src/srv/pager-srv`, which depends on `pager`, `twizzler-driver`, `nvme` (nvme-rs) and `devmgr` (confirmed) — i.e. pager-srv is the component that actually drives an NVMe disk (via twizzler-driver + nvme-rs) and exposes it both as raw storage and as the "external file" namespace source consumed by naming-srv.
- `devmgr` pairs with `src/srv/devmgr-srv` (confirmed dependency: devmgr-srv depends on `twizzler-driver` + `devmgr`) — devmgr-srv is the actual bus/device enumerator; `devmgr` is just the thin query client used by every other driver crate/server (virtio-gpu, virtio-net, pager-srv) to find their device.
- `sgtest` pairs with `src/srv/sgtest-srv` (confirmed dependency) — a minimal reference pair for how a `#[secgate::gatecall]`-based client/server crate pairing is supposed to look, independent of any real device/service logic.
- `naming-core`, `pager-dynamic`, and `twizzler-net`'s client-open path (`NetClient`, via `monitor-api`) all use the same `monitor_api::CompartmentHandle::lookup(...)` + `secgate::DynamicSecGate` pattern for cross-compartment calls resolved at runtime, as opposed to the directly-linked `#[secgate::gatecall]` stub pattern used by `naming`, `pager`, `devmgr`, `logboi`, `twizzler-display`, and `sgtest`.

---

## Runtimes & ABI (src/rt, src/abi)

### Key structs/enums/traits
- `ReferenceRuntime` (`reference/src/runtime.rs`) — the reference runtime's state struct (`state: AtomicU32` READY/IS_MONITOR flags, `object_manager`, `nameroots`); nearly all of its behavior is spread across `impl ReferenceRuntime` blocks in `runtime/*.rs` rather than one central impl.
- `MinimalRuntime` (`minimal/src/runtime.rs`) — analogous unit struct for the static/minimal runtime; same "impl blocks spread across submodules" pattern.
- `RuntimeState` (`reference/src/runtime.rs`) — bitflags (`READY`, `IS_MONITOR`) tracking the reference runtime's lifecycle stage.
- `Monitor` (`monitor/src/mon/mod.rs`) — the monitor process's top-level state: compartment manager, thread manager, address space, protected via `happylock`.
- `RunComp` (`monitor/src/mon/compartment/runcomp.rs`) — a live loaded compartment: per-thread simple buffers (`PerThread`), state flags/signals (atomics), comp-config pointer, monitor-owned allocation helpers.
- `CompartmentMgr` (`monitor/src/mon/compartment.rs`) — owns/looks-up all `RunComp`s by ObjID/name/dynlink-id; tracks controller relationships and cleanup.
- `RunCompLoader`/`LoadInfo` (`monitor/src/mon/compartment/loader.rs`) — the compartment-loading pipeline that turns a set of library names into a running `RunComp`, via `dynlink::context::Context`.
- `ThreadMgr`/`ManagedThreadInner`/`ManagedThreadRepr` (`monitor/src/mon/thread.rs`) — monitor's global thread registry, each entry owning the kernel thread-repr object.
- `ThreadCleaner` (`monitor/src/mon/thread/cleaner.rs`) — background reaper thread for exited threads.
- `Space`/`MapInfo`/`MapHandleInner` (`monitor/src/mon/space.rs`, `space/handle.rs`) — the monitor's address-space/object-mapping manager and owning handle type (unmap-on-drop, deferred to `Unmapper`).
- `Engine` (`monitor/src/dlengine.rs`) — monitor's `dynlink::engines::ContextEngine` implementation (object backing + libname tracking).
- `SharedCompConfig` / `TlsTemplateInfo` (`monitor-api/src/lib.rs`) — per-compartment configuration and TLS template info shared between monitor and each compartment's runtime.
- `CompartmentHandle` / `CompartmentLoader` / `LibraryHandle` / `LibraryLoader` (`monitor-api/src/lib.rs`) — client-side RAII handles/builders for talking to the monitor about compartments and libraries; implement `secgate::util::Handle`.
- `RuntimeThreadControl` (`monitor-api/src/lib.rs`) — per-thread control data (incl. `THREAD_STARTED` thread-local flag) shared between monitor and runtime thread management.
- `MonitorStats`/`SpaceStats`/`ThreadMgrStats`/`CompartmentMgrStats`/`HandleStats`/`DynlinkStats` (`monitor-api/src/lib.rs`) — stats snapshot types returned by `monitor_rt_stats`.
- `ObjectHandleManager` (`reference/src/runtime/object.rs`) — the reference runtime's cache of currently-mapped `ObjectHandle`s (keyed by `ObjectMapKey`).
- `HandleCache`/`FotCache` (`reference/src/runtime/object/handlecache.rs`, `fotcache.rs`) — supporting caches for raw object handles and foreign-object-table entries.
- `LocalAllocator` (`reference/src/runtime/alloc/talc.rs`) — the reference runtime's `talc`-based per-compartment allocator, with an early-boot spinlock phase before `Mutex` is usable.
- `FileDesc`/`Fd` trait (`reference/src/runtime/file.rs`, `file/file_desc.rs`) — the reference runtime's fd abstraction; every fd "kind" (raw file, dir, symlink, pty, socket, compartment, kconsole) implements `Fd`.
- `SocketKind`/`Engine` (socket) (`reference/src/runtime/file/kinds/socket.rs`, `socket/engine.rs`) — smoltcp-backed TCP/UDP socket implementation and its background network engine.
- `ElfObject`/`ElfHeader`/`ElfPhdr` (`minimal/src/runtime/load_elf.rs`) — minimal runtime's own ELF loader for spawning executables (reference runtime instead delegates to `monitor-api::CompartmentLoader`/`dynlink`).
- `Invariant` trait (`abi/rt-abi/src/marker.rs`) — unsafe marker trait for types safe to store in a persistent Twizzler object (FFI-safe, fixed layout, arch-independent); implemented for all primitive numeric types.
- `ObjectHandle` (`abi/rt-abi/src/object.rs`) — the core cross-runtime RAII handle to a mapped object (`Clone`/`Drop` call back into `twz_rt_update_handle`/`twz_rt_release_handle`).
- `ObjectCreate`/`ObjectSource`/`MetaInfo`/`MetaExt` (`abi/rt-abi/src/object.rs`) — object-creation parameters and metadata-extension records shared by every layer that creates objects (minimal runtime, reference runtime, monitor's `Space`).
- `TwzError`/`RawTwzError`/`ErrorCategory` (`abi/rt-abi/src/error.rs`) — the ABI-wide error type: a packed `u64` (category+code) with typed category enums (`GenericError`, etc.) layered on top.
- `RuntimeInfo`/`BasicAux`/`BasicReturn`/`CtorSet` (`abi/rt-abi/src/core.rs`, `bindings.rs`) — the structs passed from `_start`/loader into `twz_rt_runtime_entry`, describing how to initialize a freshly-loaded program/compartment.

### API surface
The "Runtime trait" in Twizzler is not a literal Rust `trait` — it's the `twz_rt_*` C ABI defined by the bindgen bindings in `twizzler-rt-abi::bindings` (generated from `src/abi/include/twizzler/rt/*.h`). Each runtime (`minimal`, `reference`) provides a concrete inherent-method surface on its own runtime struct (`MinimalRuntime` / `ReferenceRuntime`) and then exports it as `extern "C-unwind" twz_rt_*` symbols in a `syms.rs` file, type-checked against the bindgen signatures via a `check_ffi_type!` macro. The functions a runtime must implement span:
- Core lifecycle: `runtime_entry`, `pre_main_hook`/`post_main_hook`, `exit`, `abort`, `gc`.
- Allocation: `malloc`/`dealloc`/`realloc` (`GlobalAlloc` impl).
- Objects: `map_object`, `map_two_objects`, `release_handle`, `update_handle`, `create_object`/`create_rtobj`, FOT insert/resolve.
- Threads: `spawn`, `join`, `futex_wait`/`futex_wake`, `yield_now`, `sleep`, `set_name`/`get_name`, `tls_get_addr`, `available_parallelism`, `thread_get_info`.
- Files/IO (reference runtime only — minimal has a much smaller fs.rs): `open`/`close`, `read`/`write`, `pread`/`pwrite[v]`, `seek`, `poll`/`select`, `fd_get_info`/`fd_get_config`/`fd_set_config`/`fd_cmd`, naming ops (`symlink`/`readlink`/`rename`/`remove`/`mkns`).
- Exec: `exec_spawn` (spawn a new program/compartment).
- Debug: `get_image_info`, `iterate_phdr`.
- Time: `get_monotonic`, `get_system_time`, `actual_monotonicity`.
- Misc: `sysinfo`, `get_random`, upcall handling (`upcall_rust_entry`/arch-specific trampolines).

Monitor-api's public surface (what a client compartment can ask the monitor to do, via `#[secgate::gatecall]` functions in `monitor-api/src/lib.rs`, implemented in `monitor/src/gates.rs`):
- Compartment lifecycle: load a compartment (`monitor_rt_load_compartment`), get a handle/info/deps/threads for one (`monitor_rt_get_compartment_handle/info/deps/thread`), look one up by name or ID, wait for state changes (`monitor_rt_compartment_wait`), control it (`monitor_rt_comp_ctrl`), set a controller relationship, drop a handle.
- Library lifecycle: load a library (`monitor_rt_load_library`), get info/handle, drop a handle, resolve a dynamic gate (`monitor_rt_compartment_dynamic_gate`), look up a symbol.
- Threads: spawn a thread inside a compartment (`monitor_rt_spawn_thread`), get per-thread simple-buffer object for IPC-ish communication.
- Objects: map/pair-map/unmap objects on the caller's behalf (`monitor_rt_object_map`/`_pair_map`/`_unmap`).
- Config/naming: get the caller's `SharedCompConfig`, set the naming root (`monitor_rt_set_nameroot`), map/unmap library names to ObjIDs.
- Misc: post a signal to a compartment, fetch aggregate monitor stats (`monitor_rt_stats`).

### Critical relationships
- Userspace programs in the default (dynamic-linking) build link against the thin `rt` wrapper crate (`twizzler-runtime`, `src/rt/src/lib.rs`), whose sole content is `#[link(name = "twz_rt")] extern "C-unwind" {}` — this forces the linker to pull in the `reference` crate's `cdylib` output (crate name `twz-rt`, `[lib] crate-type = ["cdylib"]`) rather than selecting it via a Cargo feature/cfg; `reference` is thus the "default dynamic-linking runtime" by virtue of being the thing `rt` hard-links to.
- `minimal` (crate `twizzler-minruntime`) is an entirely separate implementation of the same ABI, selected instead of `reference`/`rt` by building a program in the Userspace-static collection (`package.metadata.twizzler-build = "static"` in its `Cargo.toml`) rather than the default dynamic-userspace collection; it does not depend on `dynlink`, `monitor-api`, or `secgate` at all, and implements its own ELF loader (`load_elf.rs`) and slot/TLS/allocator logic from scratch.
- `reference` depends directly on `dynlink` (for compartment/TLS/library loading), `monitor-api` (for all monitor RPCs — `CompartmentLoader`, `CompartmentHandle`, `LibraryHandle`, `RuntimeThreadControl`, `THREAD_STARTED`, `get_comp_config`), and `secgate` (for `TwzError`, `util::Descriptor`, thread/context IDs used by the trace subsystem) — i.e. the reference runtime is the compartment-aware, monitor-mediated runtime, while `minimal` is monitor-agnostic.
- `monitor-api` exists specifically to avoid `reference`/`monitor` having a direct Cargo dependency cycle: `reference` calls into the monitor only through `monitor-api`'s `#[secgate::gatecall]`-declared functions (compiled as cross-compartment gate stubs), and `monitor` implements the actual bodies of those same gate calls in `monitor/src/gates.rs` (built with monitor's `secgate-impl` feature, default-on). Both `reference` and `monitor` depend on `monitor-api` as a workspace dependency.
- `monitor` depends directly on `dynlink` (owns/drives the `Context` for all compartments, via `dlengine::Engine` implementing `ContextEngine`), `secgate` (implements the gate call machinery), and `twizzler-security` (feature `user`) for security-context handling — none of which are used directly by `minimal`.
- `twizzler-rt-abi` (`src/abi/rt-abi`) and `twizzler-types` (`src/abi/types`) are the shared foundation under all of the above: both `reference` and `minimal` depend on `twizzler-rt-abi` for the `bindings`/`error`/`object`/`fd`/etc. types and the `twz_rt_*` function signatures they must implement; `monitor-api` and `monitor` also depend on `twizzler-rt-abi` for shared types (`bindings::{binding_info, ctor_set}`, `debug::{DlPhdrInfo, LinkMap, LoadedImageId}`, `error::{ArgumentError, TwzError}`, `thread::ThreadSpawnArgs`). The `kernel` feature on `twizzler-rt-abi` lets the kernel crate use its type definitions while the `nk!` macro prevents it from accidentally calling the (userspace-only) ABI functions.

---

## Servers & programs (src/srv, src/bin)

### Key structs/enums/traits

- `PagerContext` (pager-srv/lib.rs) — pager-srv's global state: object store handle, physical-memory tracker, kernel-notification queue; threaded through nearly every pager-srv function.
- `PagerData`/`PagerDataInner` (pager-srv/data.rs) — global physical-page allocator state for the pager (free-region tracking, per-object page maps).
- `PerObject`/`PerObjectInner` (pager-srv/data.rs) — per-object page-range tracking (which pages are resident/wired) inside the pager.
- `Memory`/`Region`/`MemoryWaiter` (pager-srv/data.rs) — free physical memory region allocator with an async waiter for page-starvation backpressure.
- `PagedObjectStore` / `PagedDevice` / `ExternalFileStore` traits (object-store/paged_object_store.rs) — the abstract interfaces pager-srv drives against any storage backend (disk, ext4, Lethe, virtio-mem).
- `Ext4Store<D>` (object-store/ext4.rs) — object store backend storing each object as a file inside an ext4 image.
- `LetheObjectStore<D>` (object-store/lethe_object_store.rs) — encrypted, provable-deletion object store (KHF key hierarchy + epoch advance = secure erase).
- `NvmeController`/`NvmeRequester`/`NvmeRequest` (pager-srv/nvme/*, duplicated in genrandom & mnemosyne) — the NVMe block-device driver: identify, DMA/PRP management, submit/poll commands.
- `DisplayInfo`/`DisplayClient` (display-srv/lib.rs) — display server's global GPU/framebuffer state and per-client window handle.
- `LogClient`/`Logger` (logboi-srv/lib.rs) — per-client log buffer and the server's client-handle table.
- `Namer`/`SbObjects` (naming-srv/lib.rs) — naming server's wrapper around `naming_core::NameStore` plus shared simple-buffer objects for client communication.
- `NameStore`/`NameSession`/`NsNode`/`NsNodeKind` (naming-core, used throughout) — the actual in-memory/persistent namespace tree naming-srv serves and naming-test/ls/ptest/sqlite_test exercise directly.
- `CacheState`/`HeldObject`/`CachedStats` (cache-srv/lib.rs) — cache server's held-object map and per-hold stats returned to clients (also redeclared client-side in `bin/cache/main.rs`).
- `Client`/`PortAssigner`/`NetworkInfo` (net-srv) — per-connection client state, ephemeral-port allocator, and global smoltcp interface/network config.
- `DriverSpec`/`OwnedDevice`/`Supported` (devmgr, used by devmgr-srv/pager-srv/net-srv) — device query spec and the returned device handle other servers request through `get_devices`.
- `CompartmentLoader`/`CompartmentHandle`/`NewCompartmentFlags` (monitor-api, used pervasively by init/bootstrap/debug/trace/shell) — the API for loading and controlling compartments (servers, shell, user programs) under the monitor.
- `TwizzlerTarget`/`TwizzlerGdb`/`TwizzlerConn` (bin/debug/gdb.rs) — gdbstub `Target`/event-loop/`Connection` implementations bridging a debugged compartment to the GDB remote protocol.
- `Job`/`Jobs`/`ShellInvoke`/`ShellCommand`/`InvokeCtx` (bin/shell/main.rs) — the shell's job-control and command-invocation model.
- `Tracer`/`TracingState`/`TraceSource` (bin/trace/tracer.rs) — kernel-tracing session state and captured-event iteration used by the `trace` CLI.
- `Report`/`ReportStatus`/`ReportInfo`/`TestResult` (unittest-report/lib.rs) — the JSON test-result schema shared between `unittest` and individual test binaries.
- `FileSystem<S>`/`Superblock`/`FATEntry`/`ONode` (mnemosyne/fat) — mnemosyne's custom on-disk FAT-like filesystem schema and driver.
- `PersistentHashMap<K,V>` (twizzler::collections::hachage, exercised by persistent-hashmap-test) — an invariant-pointer persistent hash map over a Twizzler object.

### API surface (server protocols / IPC endpoints)

All `src/srv/*` servers expose their API as `#[secgate::entry(lib = "...")]` functions — cross-compartment call gates invoked by clients (typically through a matching `src/lib/<name>` client crate) rather than a message-passing protocol, except net-srv and pager-srv which additionally layer a `twizzler-queue`-based request/completion protocol underneath for high-volume/async traffic.

- **cache-srv** (`lib = ""`): `hold(id, flags)`, `drop(id, flags)`, `preload(id)`, `stat(id)`, `list_nth(nth)` — pin/unpin objects in memory and enumerate cache state.
- **devmgr-srv** (`lib = "devmgr"`): `devmgr_start()`, `get_devices(spec: DriverSpec) -> ObjID` — trigger PCIe bus scan and query for matching devices.
- **display-srv** (`lib = "twizzler-display"`): `start_display()`, `create_window(WindowConfig)`, `drop_window(handle)`, `reconfigure_window(handle, WindowConfig)`, `get_window_config(handle)`, `get_display_info()` — window lifecycle and framebuffer configuration.
- **logboi-srv** (`lib = "logboi"`): `logboi_open_handle()`, `logboi_close_handle(desc)`, `logboi_post(desc, buf_len)` — per-client log-buffer handle open/close/flush-to-console.
- **naming-srv** (`lib = "naming"`): `namer_start(bootstrap)`, `open_handle()`/`close_handle(desc)`, `put(desc, name, id)`, `mkns(desc, name, persist)`, `link(desc, name, link)`, `get(desc, name, flags)`, `rename(desc, old, new)`, `remove(desc, name)`, `enumerate_names(...)`/`enumerate_names_nsid(...)`, `change_namespace(desc, name)` — the full namespace CRUD/traversal API.
- **net-srv** (`lib = "twizzler-net"`): `start_network()`, `twz_net_alloc_port(desc, port)`, `twz_net_release_port(desc, port)`, `twz_net_drop_client(desc)`, `twz_net_open_client(config) -> NetClientOpenInfo` — plus an underlying `ClientMsg`/`ServerMsg`/`ClientRet`/`ServerRet` queue protocol (twizzler-net) for actual socket send/recv traffic once a client is open.
- **pager-srv** (`lib = "pager"`): `pager_start(q1, q2) -> ObjID` (one-time bootstrap wiring the kernel<->pager queues), `adv_lethe()`, `disk_len(id)`, plus (`handle.rs`) `pager_open_handle`/`pager_close_handle`, `pager_enumerate_external`, `pager_lookup_external`, `pager_create_external`, `pager_unlink_external`, `pager_readlink_external` (name<->object resolution against the on-disk store, used by naming-srv); the high-volume page-in/page-out path itself runs over a `twizzler-queue` (`RequestFromKernel`/`CompletionToKernel`, `RequestFromPager`/`CompletionToPager`), not secgate calls.
- **sgtest-srv** (`lib = "sgtest"`): `foo(x: Foo) -> u32` — trivial round-trip test gate, no real functionality.

### Critical relationships

- Boot order (from `src/bin/bootstrap/src/main.rs` and `src/bin/init/src/main.rs`): kernel starts **bootstrap** -> bootstrap loads and jumps into the **monitor** (`libtwz_rt.so`/`monitor`, via `dynlink`) -> monitor starts **init** -> init loads, in order: **logboi-srv** -> **devmgr-srv** -> **pager-srv** (`pager_start`) -> **naming-srv** (`namer_start`, needs pager's bootstrap object ID) -> [library-directory caching] -> **cache-srv** -> **net-srv** -> **display-srv** -> **sshd** -> optionally **unittest** (if booted with `--tests`) -> loops forever launching **shell** (restarting on exit), or instead runs a single named `autostart` program if one was passed on the kernel command line.
- All server compartments are loaded via `monitor_api::CompartmentLoader` with `NewCompartmentFlags::EXPORT_GATES`, and init blocks on each compartment's `CompartmentFlags::READY` flag before proceeding — this is the mechanism enforcing the dependency order above (e.g. naming-srv isn't started until pager-srv signals ready).
- **naming-srv** depends on **pager-srv** having started (`initialize_namer(bootstrap_id)` takes pager's bootstrap object ID) — pager-srv's `pager_lookup_external`/`pager_create_external`/`pager_enumerate_external`/`pager_unlink_external`/`pager_readlink_external` gates are the mechanism by which naming-srv resolves/creates objects backed by files on the real on-disk filesystem.
- **pager-srv** and **net-srv** both depend on **devmgr-srv** (`devmgr::enumerate_devices`/`get_devices`) to find their NVMe/virtio-net PCIe devices; **display-srv** talks directly to a virtio-gpu device without going through devmgr in the code inspected.
- Client programs use per-server client library crates rather than calling secgate gates directly: `bin/cache` (cache-srv, but note it also redeclares the gate signatures locally), `bin/ls`/`bin/shell`/`bin/naming-test`/`bin/sqlite_test`/`bin/persistent-hashmap-test`/`bin/gadget` (naming, via the `naming`/`naming-core` crates), `bin/gadget` (pager, via the `pager` crate's `adv_lethe`), `bin/gfxtest` (display-srv, via `twizzler-display`), `bin/virtio`/`bin/stdnet_test` (bypass net-srv, talking to `virtio-net`/`std::net` directly — `stdnet_test` and `sshd` go through the OS socket layer which net-srv backs), `bin/logboi-test`/`bin/gadget` (logboi, via the `logboi` crate).
- **debug** and **trace** both load and control arbitrary target compartments via `monitor-api::CompartmentLoader`, similarly to how init loads servers — `debug` additionally speaks GDB-remote over the loaded compartment, `trace` attaches kernel tracing to it.
- **unittest** is itself launched by init and in turn spawns every other test/benchmark binary found in `/initrd` as child processes (matched by name), collecting results/timings via the shared `unittest-report` schema — most of the small one-off test binaries in `src/bin` (`randtest`, `random_validation`, `schedtest`, `object-store-test`, `naming-test`, `stdnet_test`, `ptest`, `persistent-hashmap-test`, `sqlite_test`, etc.) are meant to be run this way rather than launched by end users.
- **genrandom**, **mnemosyne**, and **pager-srv** each carry their own independent, largely-duplicated copy of the NVMe driver (`nvme/controller.rs`, `dma.rs`, `requester.rs`) — genrandom and mnemosyne talk to the disk directly instead of through pager-srv, useful for raw disk benchmarking/formatting outside the normal paging path.
- **gadget** is currently disabled: not a workspace member, not in the initrd, and no longer buildable — its `setup_http` demo depended on a vendored `tiny_http` port that was removed along with the `test-tiny-http` harness carrying it.
- **object-store-test** exercises the `object-store` library crate (which lives under `src/srv/pager-srv/object-store`) directly and independently of `pager-srv`, as a lower-level test of the Lethe/ext4 storage backends.
