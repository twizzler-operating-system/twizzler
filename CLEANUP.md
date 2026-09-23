# Cleanup review: d8c944d3..HEAD (0755d59c) — status as of 2026-09-17 21:22 UTC

88 commits, Jul 17 – Sep 16 2026. ~97k insertions / ~13k deletions across 534 files, produced by
multiple concurrent sessions. The original pass (against b87f10c2) was verified against HEAD only.
This revision re-checks every item against the working tree, which carried a cleanup sweep that
answers most of them. While this file was being written, a concurrent session committed that sweep
(a6ef3377, 177 files, +1460 / −19558) and then kept committing every minute or so (`src/abi`,
`ferroc`, `mlibc` in-submodule commits with gitlinks bumped; `ctl`/`brush`/`ssh` added; strays
deleted). Every "DONE" below is now in HEAD unless it says otherwise; every "OPEN" item is unchanged
from the first pass unless noted. Re-verify against `git status` before acting — this tree moves.

- Build evidence: `cargo check-all` on the tree content that became a6ef3377 — see §0. No x86 boot
  was attempted then. 2026-09-18: aarch64 builds and links (`cargo build-all --arch aarch64
  --machine virt`, all five collections); see 1c.

## Major threads of change

| Thread | When | Main artifacts |
|---|---|---|
| Memory-leak hunt + lock tracking | 07-18..07-24 | `src/test/leakcheck`, `tools/leakplot.py`, kernel `kalloc_census`/`kalloc_track`, monitor `heapdiag`/`lockdiag`, TLS-dtor fixes |
| Test infrastructure | 07-25..08-01 | `tools/xtask/src/test.rs` (`cargo test-all`, lowmem scenario), `unittest` rework, `many.py`/`check-many.py`, `memhog-test` |
| SMP/stability/mutex fixes | 08-05..08-15 | kernel fixes, `thread/reaper.rs`, hang reporting |
| Priority work | 08-12 | `sys_thread_set_priority`, kernel `pager/boost.rs`, pager fast/bulk lanes |
| TLB improvements + KVM PV | 08-14..09-10 | amd64 `memory/pagetables/consistency.rs` (+744), `arch/amd64/kvm.rs`, tlbfix precise targeting |
| Perf hunt (dominant, 21 commits) | 08-19..09-16 | `sysbench`, `pagepar`, `framecache`, `SlotMgr`/`regionmgr`, sharded omap, ferroc slab work, `perfmark`, pager `profile` |
| Pager async→blocking rewrite | 08-28 | ae4410b8 |
| Toolchain: `target_family="unix"` flip | 08-28..09-04 | `unix.md`, rustix/nix/getrandom ports, uuhelper restore, hosted rustc, libgit2/libssh2/nghttp2/neatvi ports |
| Net-stack + syncwedge hunts | 08-27..09-16 | net-srv switch rewrite + diagnostics, `net_test`/`net_test_peer`, ARM-REPAIR fix in `obj/thread_sync.rs` |
| Cleanups | scattered | deleted `mnemosyne`, `genrandom`, `test-tiny-http`, `cat`, `ls`; test bins moved `src/bin` → `src/test` |
| **Uncommitted cleanup sweep** | working tree | closes §2–§4 almost entirely; see per-item status |

Resolved-in-range items worth noting so nobody re-litigates them: the reaper starts before
`test_main()` (`src/kernel/src/main.rs:457`), the NVMe-driver triplication was eliminated by
deleting `mnemosyne`/`genrandom`, and the redundant park-path validation was removed with a
tombstone once ARM-REPAIR proved sufficient (`src/kernel/src/obj/thread_sync.rs:638`).

---

## 0. Does the working tree type-check?

`cargo check-all` on the working tree (2026-09-17): **exit 0, 0 errors, 0 `could not compile`**,
53 warnings (unused imports and never-used consts, mostly in ported crates such as `kibi`). So the
sweep is compile-complete for every collection `check-all` covers — note it does not cover kernel
tests (`#[kernel_test]` bodies are only built by `--tests`) and does not prove anything about a
fresh clone, where 1a/1b still break `cargo metadata`.

---

## 1. Missing — fell through the cracks

### 1a. OPEN, and grew: HEAD does not build from a fresh clone

`src/bin/ctl`, `src/bin/brush`, `src/bin/ssh` were added at 21:21; the other 10 directories from
the first pass are still untracked (`git ls-files` returns nothing for each), and the sweep added
more tracked references to untracked content:

| Untracked path | Referenced by (tracked) |
|---|---|
| `src/test/pipestat` | workspace member |
| `src/test/spawn-test`, `src/test/signal-test` | members + initrd + `test-programs` |
| `src/test/nullexit`, `src/test/rmdirtest`, `src/test/hmrepro`, `src/test/memlat` | members + initrd |
| `third-party/elephance` (296 files) | initrd + `[workspace.metadata.third-party]` |
| `src/ports/parking_lot_core` (26), `src/ports/backtrace` (62) | `[patch.crates-io]` — `cargo metadata` fails without them |
| **new** `src/ports/gdbstub` (197 files) | `[patch.crates-io]` (`Cargo.toml:435`), needed by the restored `src/bin/debug` (1d) |
| **new** `src/ports/brush` (2973 files) | path dep of member `src/bin/brush` (`src/bin/brush/Cargo.toml:18`); listed in `exclude` as a nested workspace |
| **new** `src/lib/twizzler-io/src/intr.rs` (40 lines) | `pub(crate) mod intr` in `lib.rs:2`, used by `pipe.rs:17` |
| **new** `src/rt/reference/src/runtime/alloc/anon.rs` (360) | `pub(crate) mod anon` in `alloc.rs:18`, used by `syms.rs:219` |
| **new** `src/rt/monitor/src/mon/handlesweep.rs` (226), `mon/state.rs` (47) | `pub mod` in `mon/mod.rs:39,43` |

Untracked copies that nothing references and should be deleted or ignored rather than committed:
`src/ports/getrandom02`, `src/ports/getrandom03`, `src/ports/socket2`, `src/ports/memmap2` (the
patch points at the tracked `memmap2-rs` submodule). `src/ports/openssl` (12k files) and
`net-srv/src/Untitled-1` were deleted at 21:21.

### 1b. MOSTLY DONE: committed code depends on uncommitted submodule content

Resolved at 21:14–21:21: `src/abi` (the `twz_rt_fd_link` / `twz_rt_thread_signal` /
`twz_rt_get_stack_bounds` / `MEXT_NLINK` additions), `toolchain/src/mlibc`, and `ferroc` are
committed in-submodule and their gitlinks are bumped in HEAD.

`src/ports/nix`: its `twizzler` branch was a stale upstream 0.29.0 snapshot (2024-05) with no port
commits; the port lived on `master`. Fixed 21:23: `twizzler` fast-forwarded to `master` and the
uncommitted ioctl/termios selection for `target_os = "twizzler"` committed there as ffaa5bf1 (the
checkout is now on `twizzler`). The gitlink is staged but not yet in a superproject commit, and the
branch is not pushed (171 ahead of `origin/twizzler`). `.gitmodules` has no `branch =` for nix;
adding `branch = twizzler` would let `submodule update --remote` follow it.

Still dirty, in-submodule, uncommitted: `termion` (+24 in `src/sys/unix/tty.rs`: a twizzler
`get_tty()` that dups a terminal stdio fd since there is no `/dev/tty`; detached at the recorded
commit, which is on the configured `femto-twiz` branch — right branch, just uncommitted; remote is
`cosdet/termion-twiz`, not the org), `femto` (+35/−15 `src/main.rs`), `kibi` (+2/−1
`src/unix.rs`), `toolchain/src/rust` (3 zero-line entries: nested gitlinks/modes), and
`object-store` below.

`src/srv/pager-srv/object-store` is a gitlink (mode 160000) with **no `.gitmodules` entry** — an
embedded repo, invisible to `git submodule status`. The 124-line `PAGE_OUT_EXTEND_FROM_CACHE`
writeup is gone from it; what remains is a 26/11 diff in `src/ext4.rs` (adds an `extend_locked`
counter, removes `PAGE_OUT_ALWAYS_FLUSH`). The page-out site now stacks two contradictory comment
blocks — one saying the length cache answers "already long enough" without the lock, the next
saying `i_size` is always read under the lock because `set_size` does not persist reliably. The
code does the latter. Tidy before committing.

### 1c. DONE (build-verified 2026-09-18): aarch64 kernel APIs restored

The static claim was wrong by 83 errors: `EntryFlags::{WRITE,DIRTY,WIRED,OBJECT_TABLE}`,
`apply_perms`, `is_object_table`, seven `ArchTlbMgr` methods, `ArchCacheLineMgr::add_cache_line`,
and the `FrameAllocator`-taking `ArchContext` surface (`object_map`, `is_object_mapped`,
`ensure_object_mapped`, `unmap_object`) had never reached aarch64. Ported (uncommitted):
`arch/aarch64/{context.rs, memory/pagetables/{entry,table,consistency}.rs, processor.rs,
syscall.rs, thread.rs}`, `machine/arm/virt/{interrupt,serial}.rs`, the two x86 cfg gates in generic
`memory/pagetables/consistency.rs`, and the fault dump in `thread.rs`. `WRITE` is the DBM bit
(Linux's scheme); `DIRTY` is software plus a hardware-cleared AP[2]. Also needed to build:
xtask's two userspace collections that passed the CLI machine into the rust triple
(`aarch64-virt-twizzler` does not exist), the clang builtins archive on aarch64 links (mlibc's
`libc.a` uses outline atomics), `lwext4-rs` bindings moved to `OUT_DIR` (the tracked copy was
rewritten by whichever arch built last), the initrd packer skipping `debug` like the build does,
and `-vga virtio` gated to x86. Boot: Limine loads the kernel and the initrd; what happens after
is the open question (see the 2026-09-18 report).

### 1d. DONE in tree: `src/bin/debug` is back

Member again (`Cargo.toml:78`); gdbstub patched to the ported copy (`Cargo.toml:435`). That copy is
untracked (1a).

### 1e. DONE: `src/test/simdtest` is a member (`Cargo.toml:34`).

### 1f. OPEN: the `libtwz_rt.so` second-copy follow-up (unix.md)

Unchanged: delete-then-restage at `tools/xtask/src/disk.rs:161`, release-only sysroot copy via
`only_runtime` in `build.rs:613`, and the deciding question (does `ports rust`'s hosted std link
against the sysroot copy?) still unanswered.

### 1g. MOSTLY DONE: scratch-doc citations 229 → 29

All citations of the six docs that no longer exist anywhere (`pagerperf.md` x50, `sysperf.md` x21,
`sysbench.md` x18, `faplan.md` x9, `TLB.md` x8, `regionplan.md`) are stripped. The remaining 29
cite seven docs that exist on disk but are untracked:

| Doc | Citations | Where |
|---|---|---|
| `pagerwedge.md` | 8 | virtmem.rs, table.rs, pager.rs, security.rs, thread.rs, unmapper.rs, pager-srv data.rs/dma.rs |
| `spawnbench.md` | 7 | region.rs, mapper.rs, table.rs, pager.rs, inflight.rs, talc.rs, file.rs |
| `syncwedge-0910.md` | 4 | thread_sync.rs, pager/queues.rs (x2), twizzler-queue-raw |
| `prereg-mss-0827.md` | 4 | twizzler-net client.rs, socket engine.rs (x2), net-srv gates.rs |
| `reclaim-design.md` | 2 | tracker.rs, obj/mod.rs |
| `perf-inprogress.md` | 2 | region.rs, sysbench main.rs |
| `CARGO.md` | 1 | obj/data.rs:685 |

Decision still needed: commit these seven (e.g. under `doc/notes/`) or strip the last 29.

### 1h. OPEN: CI never picked up the new test lanes

Unchanged. `.github/workflows/build-and-test.yml:45,47` runs only `start-qemu --tests`; `cargo
test-all`, the lowmem scenario, and the fsck lane still run only by hand / `many.py`.

### 1i. DONE: `NONBLOCK_POLL_QUEUE` decided

The knob is gone; the non-blocking arm ships unconditionally. The `POLLQ_TX_DROPPED` /
`POLLQ_COMP_DEFERRED` counters remain (9 refs) with a doc comment making the drop-instead-of-wedge
behaviour explicit (`src/lib/twizzler-net/src/lib.rs:21-25`) — that is a recorded decision, not
sediment.

### 1j. DONE: committed doc drift

`.claude/repo_index.md` (−58 lines) no longer documents `src/bin/ls` or the deleted modules;
`.claude/architecture.md` no longer describes the NVMe triplication; `CLAUDE.md` documents
`test-all`, `many.py`, `check-many.py` (`CLAUDE.md:81-88`). `doc/src` is still +18 lines for the
whole range — unchanged, low priority.

### 1k. DONE: `logboi-test` re-enabled

Back in `test-programs` (`Cargo.toml:130`) and initrd (`:164`). The #GP it was disabled for is not
mentioned anywhere now; if it recurs, the suite will say so.

---

## 2. Duplicate efforts

### 2a. DONE: heap censuses 3 → 1

`src/rt/monitor/src/heapdiag.rs` and `src/srv/pager-srv/src/heapdiag.rs` are deleted. The reference
runtime `census` (+ `census::track`) in `runtime/alloc.rs` is the sole remaining copy.

### 2b. PARTIAL: wedge/hang monitors 6 → 3

Gone: net-srv per-octet stage/park tracking (3e); schedmon is now gated on `is_diag_mode()`
(`src/kernel/src/main.rs:500`); POLLQ counters retained deliberately (1i). Remaining, each still
always-on:

- Kernel hang report at `HANG_REPORT_SECS = 6` (`src/kernel/src/thread.rs:1282`); the comment now
  argues it is affordable because service threads parked on condvars cost one line each.
- Monitor `lockdiag.rs` (257 lines), watchdog started at `mon/mod.rs:135`.
- pager-srv `watchdog.rs` (395 lines), `lib.rs:36`.

This is now a decision item, not obvious garbage.

### 2c. DONE: pager-srv stats modules

`stats.rs`, `dispatch_stats.rs`, `heapdiag.rs` deleted; the inline `bodystats`/`lookupstats`/
`xferstats` modules, the `if false` stats loop, and the commented-out `bench_disk` are gone.

### 2d. Harness proliferation — partly addressed

`CLAUDE.md` now documents the `test-all` / `many.py` / `check-many.py` / `unittest` entry points.
Contracts (what each grades, exit codes, where results land) are still only in `xtask test --help`
and the scripts themselves.

---

## 3. Garbage code

### 3a. DONE: zero `if false` blocks remain in `src/`/`tools/` (ports excluded).

### 3b. DONE: experiment toggles 105 → 0

The only `const …: bool = false` left in the kernel is `IS_ZEROED` in
`memory/allocator.rs:101`, an associated const, not an A/B gate.

### 3c. DONE: the ungated QBELL prints are gone from `obj/thread_sync.rs`.

### 3d. PARTIAL: debug print volume

Kernel `emerglogln!` 209 → 139, `logln!` 243 → 163 (range start: 56 / 92). Still ~2.5x / ~1.8x the
starting surface; no further obvious dead prints, so this is a taste call.

### 3e. DONE: net-srv syncwedge forensics kit removed (0 references to the eight arrays).

### 3f. DONE for pager-srv (`rand = "0.10"`, `libc = { workspace = true }`)

Wildcard deps remain at ~55 other sites (`sshd`, `ssh`, `cache`, `net-srv`, `xtask`, …), nearly all
pre-dating the range. Out of scope for this review but worth one pass if someone is in there.

---

## 4. Obsolete code

- **4a. DONE** — no `SYNC_SLOT_MEMO` / `FAULT_SLOT_MEMO` / SlotMemo references remain.
- **4b. DONE** — the `TLS_FRAME_ALLOCATOR` pool is gone from `tracker.rs`; `framecache` has no
  `ENABLED` switch.
- **4c. DONE** — `memory/pagetables/zeroprobe.rs` deleted, no references.
- **4d. DONE** — `thread/locktrack.rs` 900 → 240 lines; `DISABLE_LOCK_TRACKING` gone; what remains
  is the `diag` anomaly counters that `repo_index.md:191` describes.
- **4e. DONE** — `kalloc_census.rs` / `kalloc_track.rs` deleted, no references.
- **4f. DONE** — `gadget`, `etl_twizzler`, `sgtest`, `sgtest-srv`, `src/test/virtio`,
  `naming-test`, `object-store-test`, `tools/serialtest` all deleted (staged).
- **4g. PARTIAL** — schedmon is diag-only now; `HANG_REPORT_SECS = 6` still ships (see 2b).

---

## Remaining work, in order

1. **Sweep is committed** (a6ef3377); §0 was green on that content. Nothing to do here.
2. **Make HEAD reproducible** (1a, 1b): `git add` the 10 remaining directories plus
   `src/ports/gdbstub`, `src/ports/brush`, and the four `.rs` files declared by tracked modules;
   commit the staged nix gitlink and push nix `twizzler`; commit inside `termion`, `femto`, `kibi`,
   `toolchain/src/rust`, and `object-store` (after tidying its page-out comment) and bump the
   gitlinks; give `object-store` a `.gitmodules` entry or absorb it.
3. **Delete or ignore the stray copies**: `getrandom02`, `getrandom03`, `socket2`, `memmap2`.
4. **Decide the seven cited scratch docs** (1g): commit or strip the last 29 citations.
5. **Close the `libtwz_rt.so` follow-up** (1f) and **wire the test lanes into CI** (1h).
6. **Decide the three remaining always-on monitors** (2b/4g) and whether the log surface (3d) is
   where it should stay.
7. **Boot aarch64**: the virt kernel boots to userspace (2026-09-18): every boot line prints,
   threads and the initrd come up, `bootstrap` relocates the monitor. Fixed on the way: a
   kernel-owned boot stack, buffered-and-replayed early console (Limine's HHDM has no MMIO),
   born-dirty kernel mappings plus a software dirty fault for object pages (cortex-a72 has no
   FEAT_HAFDBS), the thread-pointer read hoisted above its write in `init_cpu`, aarch64
   page-table levels renumbered to the generic convention, the inverted descriptor-type bit,
   and EL0 counter access. 2026-09-19: the `process_relr` fault was the text segment: dynlink
   direct-maps text from the ELF object (p_offset must be p_vaddr - 0x1000) but bare ld.lld
   links aarch64 with a 64K max page size, so every text address was off by 0xf000 (the
   x86-only check in `engines/twizzler.rs` hid it; now unconditional). Fixed in the toolchain
   (`-zmax-page-size=0x1000` in the target spec, linker script text_begin = NULLPAGE_SIZE),
   which needs `cargo toolchain bootstrap --step libc --step rust --step rt` (libstd.so and
   libtwz_rt.so in the sysroot are 64K-laid-out). Also fixed while there: aarch64 dynlink's
   `Tcb` now has the x86 layout with TP fixed 128 bytes above it (mlibc finds its Tcb as
   `tp + 16 - sizeof(Tcb)`; mlibc's aarch64 Tcb gets x86's padding under `__Twizzler__`,
   `RuntimeThreadControl` is now exactly mlibc-sized, 144), Variant I blocks use lld's
   placement (`16 + ((p_vaddr - 16) & (align - 1))`, TP aligned) in dynlink, the kernel and
   the minimal runtime, aarch64 `do_reloc` gained the weak-symbol and per-compartment cache
   handling x86 had (missing weak symbols were a hard error) and a correct RELATIVE arm, and
   rt-abi's aarch64 `__twz_rt_upcall_entry` is implemented. Toolchain rebuilt and verified: relocation completes and bootstrap enters the
   monitor. Next fault was libc.so's PLT: the clang-linked libc uses REL-form `.rel.plt`, and
   the aarch64 `JUMP_SLOT` arm added the slot's implicit "addend" (the lazy-binding stub
   address 0x105a0) to the symbol, so libc's `__rust_entry_from_c` slot pointed 0x105a0 past
   the monitor's trampoline, into `TwzError`'s Debug impl; now `S` only, as on x86. Also: the
   Userspace-tests collection did not build for aarch64 because `src/bin/debug/src/gdb.rs` was
   x86-only; its two `TwzRegs` conversions and the target's `Arch` are now per-arch
   (gdbstub's `AArch64CoreRegs`; `UpcallFrame::fp` holds x30, and SIMD state is zeroed because
   the upcall frame does not save it). The kernel must still deliver EL0 `brk` as an upcall
   before the debugger is actually usable there; `many.py` takes `--arch/--machine` for aarch64 lanes (tcg
   only). Odd and unexplained: under `-S -gdb`, `user_init`'s first write into the bootstrap
   image refaults the same page hundreds of thousands of times before it maps, while the same
   image passes in 46 s without the stub. Then, in order: `UpcallFrame::new_entry_frame` was a
   `todo!()` (implemented: pc/sp/x0/tpidr, spsr 0x340, prior ctx); secgate's gate call was
   `todo!()` on non-x86 (now `blr` with x0-x2 and C clobbers); the kernel's aarch64
   `setup_upcall` had `todo!("supervisor stack requested")` (ported from x86 incl. the sctx +
   thread-pointer switch in `arch_queue_upcall`); the reference runtime installed and passed the
   TCB address as the thread pointer (`sys_thread_settls`, `monitor_rt_spawn_thread`) which is
   only right on x86 -- now `dynlink::tls::thread_pointer_from_tcb`; and the kernel's
   `create_user_slice` tripped a debug precondition on a null user pointer (now `None`). Result
   (2026-09-19 03:40, many.py lane `a64-as11`): bootstrap -> monitor -> init -> logboi/devmgr,
   then pager-srv panicked for want of an NVMe controller. PCIe on virt (same day): the generic
   parts of `machine/pc/pcie.rs` moved to `machine/pcie.rs` (bus walk, BARs, KSOs, kaction,
   interrupt bookkeeping) with the segment's ECAM phys/virt base and MSI doorbell as inputs;
   pc feeds it ACPI+`phys_to_virt`, `machine/arm/virt/pcie.rs` feeds it the
   `pci-host-ecam-generic` node mapped as device memory and the `arm,gic-v2m-frame` doorbell.
   aarch64 `allocate_interrupt_vector` hands out SPIs from MSI_TYPER's range; the IRQ handler
   forwards unknown INTIDs to `external_interrupt_entry`; GICv2 `MAX_VECTOR` now spans SPIs
   (was the SGI end, 15, which sized the generic tables); GICD_ICFGR is programmed so
   `set_interrupt`'s trigger is honored (v2m pulses SPIs; the UART is now registered Level).
   `PcieDeviceInfo` carries `msi_addr`; twizzler-driver builds the MSI message per arch and its
   aarch64 DMA `sync` is a `dsb sy`. Config space and non-prefetchable BARs are mapped
   `MemoryMappedIO` (UC on x86, Device on aarch64). The last blocker was firmware leaving the
   NVMe function's PCI command register at 0 (no memory decode, no bus master; BARs read as
   all-ones and the driver spun on RDY): the kernel now sets MSE|BME on endpoints at
   registration. Result (lane `a64-pcie4`): pager, naming, net-srv, sshd all up, init finishes
   in ~9 s, the shell prints its banner. display-srv panics in the virtio-gpu transport (no GPU
   on virt) without stopping init. Matrix (2026-09-19, aarch64 many.py lanes are TCG-only): debug/smp1 boots to a
   shell reproducibly (init ~9 s). debug/smp4 hangs in `boot_all_secondaries` -- secondary
   bring-up has never worked here; `init_secondary` still has the old SPSel block the boot path
   dropped. Release does not build for aarch64 at all, and predates this work: the kernel's
   `arm-gic`/`smccc` pull `thiserror`, whose proc-macro half needs `syn`, and a cold release
   build compiles `syn` for the *target* and fails (x86 release builds clean as a control;
   `src/kernel/Cargo.toml` is untouched). Kernel tests do run on aarch64: 27 of 28 attempted
   pass, then the suite wedges in `test_mutex`; x86 `--tests` passes 85/85 in the same tree.
   Two things hid all of this: backtracer_core's aarch64 `trace`/`trace_from` are `todo!()`, so
   every kernel panic double-panicked and lost its message (`panic.rs` now skips the backtrace
   on aarch64 and says so), and many.py's 300 s silence watchdog fires during `test_condvar`'s
   slow dot output under TCG, so raise `TWZ_SILENCE_TIMEOUT` on aarch64 test lanes. Secondary CPUs now start (four PSCI-path defects: the entry stub was a
   normal Rust fn whose prologue writes an undefined SP, so it is naked now; SP_EL0 was the
   stack's base rather than its top; the 4 GB identity map did not reach the kernel image the
   trampoline executes from, so it is sized by `frame::max_phys_addr()` on aarch64; and
   `args.spsr` inherited the bsp's SPSR_EL1, which my naked boot stack had flipped to EL1h, so
   the core landed on an uninitialized SP_EL1. `BootArgs` also carries `self_va` now, because
   PSCI passes the args physically and that is unmapped once the MMU is on). smp4 now boots to the
   shell (init ~8.3 s). The `late_init` stall was `PL011::enable_rx_interrupt` disabling the
   UART and spinning until the rx side was empty: xtask writes `status\n` to qemu's stdin every
   heartbeat as a liveness probe, that lands in the guest UART, and nothing drains it until the
   interrupt this function enables -- smp1 simply got there before the first heartbeat. It just
   sets UARTIMSC.RXIM now, and `PL011::init` no longer waits for rx either (a loaded smp1 boot that
   reached its first UART init after the 15 s heartbeat hung there with no kernel output at
   all). Four more SMP defects fixed on the way, each invisible at smp1: the
   timer handler cleared CNTP_CTL.ENABLE after the hardtick had rearmed it (every cpu's clock
   died after one tick); `GICD::set_interrupt_target` indexed the SPI target array with `-1`
   where the split-off banked registers need `-8` (QEMU's uniprocessor GIC targets everything at
   cpu0, which hid it); aarch64 `send_ipi` waited on GICD_CPENDSGIR, a receiver-banked register,
   with interrupts masked, so any inbound SGI wedged the sender; and `read_current_thread_ptr`'s
   aarch64 arm derefed the thread-local under interrupts-off, which does not stop the
   `tpidr_el1` read hoisting above the disable (`maybe_suspend_self ... while Some(M) is
   current` on secondaries) -- it and `framecache::cache()` now read `tpidr_el1` in the same
   asm block as the load, like the x86 `fs:` arms. Console output from several cpus interleaves
   per character because emergency prints take no lock; that is by design. Right after the
   shell, the smp1 lane hit a debug-build precondition panic in `create_user_slice`
   (misaligned pointer); it now returns None for a misaligned or oversized slice, with a
   temporary log naming the element type until the calling syscall is identified. One smp1 run
   had naming-srv take an ObjectMemoryFault, and delivering the upcall panicked the kernel:
   `arch_queue_upcall` held the `entry_registers` RefCell borrow across the frame write to the
   user stack, the write faulted, and `sync_handler` could not `borrow_mut`; the handler also
   set and cleared entry registers on kernel-mode entries. Both fixed (pointer copied out;
   set/clear only for EL0 entries, as x86 does). Final state: debug smp1 and smp4 both boot to
   the shell (init ~8.3-9 s). smp1 `--tests`: all kernel tests pass (the `test_mutex` wedge is
   gone); the userspace suite then wedged after the kernel rejected a `ThreadSync` at 8 mod 16 --
   aarch64 `setup_upcall` entered handlers with the x86 8-byte stack skew, which aarch64
   prologues do not absorb, so every upcall handler ran with a misaligned SP; removed. smp4
   `--tests` then panicked in `CondVar::signal` with no current thread during `test_mutex`: the
   aarch64 context switch never used `switch_lock`, so a thread could be picked up by another cpu
   before its registers were saved and the reaper could free a thread still on its stack
   (`has_left_kernel_stack` was hard-wired false there). The amd64 `__do_switch` handshake is now
   in a naked `__do_switch` (release after the sp save, acquire before the restore) -- naked
   because the resumed thread `ret`s through the saved x30 past any epilogue, so the switch must
   own no frame. With it, all kernel tests pass on smp1 and smp4. The userspace suite still
   dies intermittently on aarch64 with `Object(NotMapped)` faults reported as "supervisor
   exception in monitor" (naming-srv start, init starting net-srv, net_test, naming_core tests),
   with the same ip repeating in a run. 2026-09-21: that was the kernel after all. The switch
   saved no FP/SIMD state (the kernel is soft-float, so v0-v31/FPCR/FPSR only change hands in
   `__do_switch`; they are in `RegisterContext` now, under `.arch_extension fp/simd` -- `cargo
   check` never assembles, only a build catches a rejected `stp q`); the aarch64 `UpcallFrame`
   had none either (`fpcr/fpsr/v` added, saved in `setup_upcall`, restored in `handle_upcall`,
   plus the literal in `src/bin/debug/src/gdb.rs`); and `return_to_user` programmed
   ELR/SP_EL0/SPSR with IRQs unmasked, so a tick in that window made the `eret` land on a kernel
   pc at EL0 -- visible as an EC=0 exception only because kernel mappings lacked UXN (set for
   every non-USER mapping now; `EntryFlags::perms` reads EXEC from the XN bit of the mapping's
   own privilege, the other way round broke `test_mapper_levels`). `restore_upcall_frame`
   refuses frames with a kernel pc/sp. Diagnostics left in: an interrupts-off assert in
   `arch_switch_to`, a kernel-stack overflow check at EL1 entry (skips threads whose saved sp is
   not on their own stack: the boot cpu's leaked per-cpu stack sits just below thread 1's), and
   TEMP prints for switch_lock==0, ignored `sys_object_map` errors, and entry registers on
   upcalls. With these the userspace suite runs to the `twizzler` crate on smp1 and smp4
   (lanes a64-fp8/fp9/fp10). Residue: 0-2 processes per smp1 run die at start (exit 137) in
   libtest `getopts::Options::parse` reading a Vec element with ptr 0 -- same sp offset every
   time, never a slot collision (the map log never fired); `sys_object_map` still ignores
   `map_object_into_context` errors; `test_mutex` at smp4 trips `switch_lock == 0` in
   `arch_switch_to` about one run in three (likely x86's rare test_mutex wedge with a visible
   message); `collections::hachage::benches::random_insert_*` take minutes under TCG so every
   aarch64 `--tests` lane ends as "guest went silent" there; the aarch64 IRQ path still sets no
   entry registers and runs no enter/exit_kernel. Later on 2026-09-21: the getopts startup death
   was the FP save itself. `stp`/`ldp` of a q-register pair covers 32 bytes and the four
   save/restore blocks (switch and upcall frame) stepped by 16, so pair i+1 overwrote the odd
   register of pair i and every restore came back with q1=q2, q3=q4, ... (the fp11a register
   dump shows getopts' vectorised `Vec::new()` fill writing q2's literal where q1's belonged
   after a tick inside the loop). Stride is 32 now; with it both smp1 and smp4 run the whole
   userspace suite with no startup deaths, and the hachage benches finish inside an 8-minute
   run instead of outrunning the watchdog (ahash hashes through NEON). Reaching the end of the
   suite exposed three more: `debug_shutdown` was a `todo!()` (now PSCI SYSTEM_OFF via the
   device-tree conduit; QEMU exits 0 and xtask judges by the report); `handle_syscall` copied
   the results into x6/x7 of the exception context *after* `exit_kernel`, where a mailbox
   upcall snapshots that context as the resume frame, so a syscall interrupted by a signal
   resumed with its syscall number as the error code (signal-test's "post failed:
   uncategorized 36239") -- copied before `exit_kernel` now; the allocator crash that followed
   the first resume was `setup_upcall` never filling `UpcallFrame::prior_ctx` (amd64 sets it to
   the source context), so `restore_upcall_frame` switched every resumed thread to security
   context 0 and its next heap access read garbage -- set now; and lltest fails because dynlink refuses TLSDESC relocations
   against runtime-loaded modules on aarch64 (a deliberate limit, unchanged). 2026-09-22: the
   smp4 `test_mutex` `switch_lock == 0` assert was `Processor::cleanup_exited` popping the
   newest exited-list entry with no "has left its kernel stack" guard. Its callers
   (`KthreadClosure::wait[_timeout]`) run with interrupts on, so the `current_processor()`
   they read can be a cpu the waiter has since migrated off, and the entry may still be
   inside `__do_switch` there; freeing it lets that switch's late lock release land in a
   reused `Thread`. Unreachable on aarch64 until `has_left_kernel_stack` stopped answering
   false there. Now gated like `drain_exited` (the gate fires 1-5 times per smp4 run inside
   test_mutex, counted in LIVE_STACK_SKIPS); post-switch publication goes through
   `publish_current_thread`, which does not dereference the outgoing thread's box (the reaper
   may have freed it); `Thread::drop` logs a drop with the lock still held. Verified: aarch64
   smp4 debug 0 asserts in 16+ rounds, x86 KVM smp4 8/8. The aarch64 release build works after
   clearing stale host artifacts in `target/kernel/release` (the toolchain's rustc reports no
   commit hash, so cargo reuses proc-macro rlibs from an older compiler). The getty shell
   loop on aarch64 was brush, not the kernel: rustix's `Termios` lacked `line_discipline` on
   twizzler (56 bytes) while mlibc's is the Linux layout (60), so `tcgetattr` wrote `c_obaud`
   (B9600 = 13) over the low half of a saved x30 -- `target_os = "twizzler"` added to that cfg in
   the rustix submodule. With brush running, `ls` panicked in uu_ls's "six months ago"
   subtraction because the aarch64 kernel had no wall clock (the only one is kvmclock, so every
   non-KVM boot on either arch reports the monotonic counter as real time). Two fixes: uu_ls
   uses `checked_sub` (coreutils submodule), and the virt machine registers its PL031 RTC as
   the best-realtime clock (`machine/arm/virt/rtc.rs`, read once at boot and carried in
   generic-timer ticks so the userspace fast clock's `cntvct` extrapolation is right).
   Timestamps now read 2026-09-22 and `brush -c ls` exits 0. Noticed, not changed: amd64's
   `KvmClock::read` reports a 1 ns tick rate while `FastClock` extrapolates from TSC ticks, so
   `SystemTime` on KVM should run at the TSC rate between calibration and each reading -- worth
   a check. Later on 2026-09-22, the hosted Rust toolchain for aarch64: `cargo toolchain ports
   --arch aarch64 @all` needed four toolchain fixes before LLVM would even configure --
   `libc++abi.so` with undefined outline-atomics helpers (bootstrap `build_libcxxabi` now
   links the clang builtins archive via `LIBCXXABI_ADDITIONAL_LIBRARIES`; the installed .so was
   relinked in place from its build tree), mlibc's PIE `Scrt1.S` reaching `main` with `adr`
   (+-1 MiB; now adrp/add, hand-reassembled into both the sysroot and the rustlib copies),
   ncurses stripping aarch64 binaries with the host `strip` (`--disable-stripping`), and the
   binutils patch naming a bfd vector that does not exist (`aarch64_elf64_le_vec`). The rust
   port then needed the same builtins archive in its target rustflags, or
   `librustc_driver.so` (LLVM's C++) fails to load. On target, `cargo new` + `cargo add
   tracing` (registry over TLS, no CA staging needed) + `cargo build` reached the proc-macro
   crate and exposed the TLSDESC gap: dynlink now emits `_tlsdesc_dynamic` with a leaked
   `tls_index` for runtime-loaded modules and the reference runtime implements the resolver
   (fast path off the DTV at tp-128, slow path through `__tls_get_addr`); lltest's TLSDESC
   failure is the same gap, unverified there. Two more: monitor-api's runtime-load ctor runner
   built a slice from a null `init_array` (debug-only precondition; x86 ran release), and a
   user `brk` (rustc's abort) panicked the kernel -- `sync_handler` sends
   `UpcallInfo::Exception` for user traps now, like amd64 (compiles, not exercised). Result:
   `cargo build` of a `cargo new` hello with `tracing` finishes on aarch64 Twizzler, 14/14
   units, proc macro executed. Note that late compartments bind `/sysroot/lib/libtwz_rt.so`,
   so the sysroot's aarch64 copy was refreshed by hand from the xtask build; the aarch64 rust
   port build tree was deleted afterwards to make room for a sweep. That sweep (2026-09-22,
   tags sw0922x/sw0922x2/sw0922a2): x86 KVM 16 rounds each of debug/release x smp1/smp4 --
   all 72/72 except one net_test flake (release smp1) and two lane errors from a full disk
   (re-run clean); aarch64 TCG 10 rounds each -- all 72/72 (lltest now passes) except one
   garbled REPORT line (a net-srv log interleaved into the JSON; every binary passed) and one
   kernel panic in test_mutex at debug smp4: `entry.rs` `assert!(!is_critical())` after a
   worker's closure. That assert now prints the count and the `critical_origin()` site; the
   "thread made current on a second cpu" counter beside it is a false positive of the diag in
   `switch_thread`'s tail (idle's first switch-in), fixed with a pointer compare. Both are now
   explained. The assert: aarch64's `read_current_thread_ptr` was `mrs tpidr_el1` + `ldr`, and an
   interrupt between them that migrates the thread completes the load against the old cpu's TLS
   block -- the post-closure check was reading another thread's critical count; IRQs are now
   masked inside that asm block (x86's `mov fs:[off]` never had the window). Verified 16/16
   aarch64 debug smp4 rounds (tag mtx0922a; one garbled-REPORT harness artifact again). The
   net_test flake: not a lost FIN -- on x86 KVM smp1 the LAPIC timer handler ran `stat::tick()`
   before `oneshot_clock_hardtick()`, and the stattick's `cleanup_exited` (thread teardown,
   sleeping locks) can block; a switch there leaves the cpu with no timer armed until whatever
   it switched to blocks, which a user spinner (simdtest) never does. Reproduced with a scratch
   autostart script: 10 spawns beside two spinners took 27 s, the stattick's own context
   resuming after 26.9 s; with the hardtick (and its re-arm) first, 128 ms and 1 kHz ticks
   throughout (`arch/amd64/apic/local.rs`). A ULE-style sleep/run interactivity filing was tried
   first and measured no effect, so it was dropped. Observations left behind: stat ticks take
   0.5-1.5 ms each (of a 1 ms tick) on that path; aarch64 has no statclock at all
   (`start_clock` is a stub), so its timeshare calendar never advances its insert marker and the
   stattick reap never runs there; the reaper thread is woken only from the idle loop and the
   low-memory path. A scheduler pass followed (2026-09-22): `balance()` marked every lone busy
   thread for migration; a yield on an empty queue paid insert+take; the deadline boost was
   dead (reset on insert, before its own check) and now stamps at switch-in with a one-slice
   floor, and a boosted or realtime arrival preempts a timeshare thread at once (the local wake
   path marks it; the IPI handler and the tick share `needs_reschedule`); the stattick no
   longer reaps (the exit path notifies the reaper and donates REALTIME past `BACKLOG_HIGH`);
   the statclock module moved to `clock::stat` and aarch64 now runs it (so it rebalances,
   advances the calendar and samples thread stats) and sends a reschedule SGI on remote wakes;
   `TimeshareQueue::insert`'s unused `current` argument is gone. One change was tried and
   backed out: rotating equal-priority timeshare threads on slice expiry instead of every tick.
   A `--diag=wake` A/B (wakeA/wakeB) showed unboosted equal wakes at smp1 going from 176 us
   mean to 708 us, and `twizzler_queue_raw` at smp1 from 4 s to 154 s -- hand-off pairs on one
   cpu depend on the 1 ms rotation. The bsp's 1 ms round-robin between equal busy threads
   therefore stays; a real fix needs shorter slices or spin-aware yields. The first full-matrix
   sweep of the rest (sched3x) failed signal-test 8/80 at smp2-5 ("reader sees itself as objid
   0x0", 0/300 x86 runs before): a never-run thread has deadline 0, so every spawn was filed in
   the realtime slot and the child ran before its spawner's thread-manager entry existed. New
   threads are now stamped at creation and file into the calendar as before; the runtime race
   itself (a thread's own `twz_rt_get_thread_info` is filled in by the spawner) is left as found.
   The same sweep printed a boot-time `mutex stall` on the entropy accumulator (owner: cpu 0's
   idle thread, waiter on another cpu, hardticks 0 there) in 4 of 16 smp3 runs, a report only
   -- not seen in the 4 pre-batch smp3 runs; unresolved. Verification: sched3a (aarch64),
   sched4x (x86, after the stamp; 82/82) and the wakeC histogram.
   2026-09-23: the 1 ms rotation is replaced by wake preemption: a timeshare wake
   (`schedule_thread` only) goes to the front of the calendar (`TimeshareQueue::insert_front`)
   and is counted in the run queue's `pending_wakes`; equal priority rotates for such a wake once
   the running thread has had one tick (`WAKE_GRAN_TICKS`), and non-bsp cpus arm that tick;
   busy threads rotate on slice expiry only. `twizzler_queue_raw`'s test `wait` yields instead
   of spinning (4.4 s -> 0.08 s at smp1; net_test 39 s -> 33 s). `select_cpu` tested
   `rq.current_load() == 0` as "the last cpu is idle", so wakes went back to a cpu already
   running one thread: now `Processor::is_idle()`. Per-thread `switches`/`migrations`/`cpu`
   were added to `ThreadSchedStats` (`top` shows them), and `src/test/schedtest` (`st`) is a
   scheduler harness (spin, yield, pingpong, sleeper, memory, spawn; 1..3N threads). Numbers in
   `target/results/many-st2` (before) and `many-st3` (after): sleeper beside spinners 30 ms ->
   9 us at smp1; 4-of-4 memory threads jain 0.53 -> 1.0. FIXED the same night: with more threads than cpus a
   queued equal thread waited hundreds of ms behind a running one; a TEMP `--diag=switch`
   trace (removed) showed the expiring thread taking itself back: its deadline was stamped at
   switch-in as now + slice, so at the reinsertion it was "past its deadline" and filed in the
   realtime queue. The stamp now happens at switch-out (`switch_to` stamps the outgoing thread,
   a reinsertion stamps before its insert). Still open: the balancer only moves load every
   0.5-1.5 s, so an initial 3/1/1/1 placement lasts a whole 1 s test. `top` and `st` report
   the peer's `CpuInfo` (MHz, topology, caches). The
   wedge hunt (`target/many-work/hunt`) never ran: its build script copied the kernel from the
   wrong path (fixed), and the aarch64 release-smp1 silent stop is still unexplained. Also open:
   `machine/arm/morello` still
   has the old three-argument `mapper.map` calls, the TEMP entry-register/stack dump on upcalls,
   and x86 signal-test's "reader sees itself as objid 0x0" (2/8 in slx1, seen once before in
   ports0918).
