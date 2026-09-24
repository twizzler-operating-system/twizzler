# Upstream patches for the Twizzler forks

Generated 2026-08-29 from the vendored ports under `src/ports/`, as `diff -ruN` against the
pristine crates.io sources. Each is the twizzler delta and nothing else.

Purpose: three of the four ports the cargo work added exist only because a fork is stale. Landing
these upstream collapses them back to zero, leaving `socket2` as the only genuinely new port —
and that one is shaped to be upstreamable to socket2 itself rather than carried.

Apply from the crate root with **`patch -p3`** (paths inside are `src/ports/<vendor>/...`, so
three components come off), or read them as the spec for a rebase.

Verified, not asserted: each patch was applied to a fresh pristine copy of its crates.io source
and the result compared against the vendored port. All four apply cleanly and reproduce the port
byte-for-byte, ignoring the packaging files crates.io adds (`.cargo-ok`, `Cargo.toml.orig`,
`Cargo.lock`, `.cargo_vcs_info.json`).

## What each one is for

### `getrandom-0.2.16-twizzler.patch` -> `getrandom-twizzler`, branch `twizzler-0.2`

The branch is already at 0.2.16, so **only the fix matters**, not a version bump. Two changes:

- `src/twizzler.rs`: the current branch checks only `res == 0`, so a **short read is treated as
  success** and uninitialised bytes are returned as entropy. Both are corrected: `len ==
  dest.len()` is the condition, and a failure returns `Error::UNEXPECTED` instead of `panic!`.
- Declares `twz_rt_get_random` as a bare `extern "C"` rather than depending on `twizzler-rt-abi`.

If you want the smallest reviewable change, take the `fill_inner`/`getrandom_inner` body only and
keep the `twizzler-rt-abi` dependency — the bug fix is independent of the extern change and is the
part that matters. The extern version additionally lets the crate build inside tools, where
bootstrap restricts `-Zallow-features`.

### `getrandom-0.3.4-twizzler.patch` -> `getrandom-twizzler`, branch `twizzler`

**Needs a rebase first: the branch is 0.3.1 and this is against 0.3.4.** That bump is the whole
reason `src/ports/getrandom03` exists — `jobserver` requires `getrandom >= 0.3.2`, so cargo cannot
unify the `^0.3` range on 0.3.1 and silently falls back to the unported registry crate.

Same two changes as the 0.2 patch. The 0.3.1 branch carries the identical short-read bug.

### `memmap2-0.9.11-twizzler.patch` -> `memmap2-rs` submodule, branch `twizzler`

**Needs a rebase first: the submodule is 0.9.5 and this is against 0.9.11.** At 0.9.5 the fork
cannot satisfy `gix-index`'s `memmap2 = "0.9.7"`, so it is unusable for cargo — and it is stale
for the OS tree too, where it is patched into both the root workspace and
`third-party/elephance/twz`.

Contents: the twizzler backend, the four module-path arms, and the uniform cfg transformation the
0.9.5 fork already applies (29 `#[cfg(unix)]` narrowed to `all(unix, not(twizzler))`, 3
`not(any(unix, windows))` widened to include twizzler).

One thing the rebase must carry, because it is what broke first: 0.9.11 added a trailing
`no_reserve: bool` to all six `MmapInner` constructors. The 0.9.5 backend predates it. Twizzler
maps objects and has no swap to reserve, so it is accepted and ignored.

**Manifest caveat:** this is the only patch with a `Cargo.toml` hunk (it adds the
`twizzler-rt-abi` dependency under `[target.'cfg(target_os = "twizzler")'.dependencies]`). The
diff is against the *normalised* manifest crates.io publishes, not the repo's hand-written one, so
that hunk will not apply cleanly — read it and make the equivalent edit. The code hunks apply as
they are.

### `socket2-0.6.1-twizzler.patch` -> upstream `rust-lang/socket2`

Not a Twizzler fork — this is shaped to go to socket2 itself. Three of the four changes are
additions to lists the crate already maintains for platforms lacking a feature:

- `IovLen`: twizzler added to the `c_int` arm. mlibc's `msghdr.msg_iovlen` is `c_int`. Neither
  `IovLen` list has a fallback arm, so with no entry the *type* does not exist.
- source-specific multicast: twizzler added to the existing `not(any(...))` exclusion list for the
  `ip_mreq_source` / `IP_{ADD,DROP}_SOURCE_MEMBERSHIP` import, and to the matching gates on
  `join_ssm_v4` / `leave_ssm_v4`.
- `as_unix()`: gated off. This is the only genuinely new gate. Twizzler's std configures
  `std::os::unix::net` out, so the return type does not exist on this target.

No `Cargo.toml` change, no dependency change, 9 added lines total.

## After landing

- `src/ports/getrandom02` and `src/ports/getrandom03` can be deleted and the root/cargo `[patch]`
  entries pointed back at the git branches.
- `src/ports/memmap2` can be deleted and cargo's `[patch]` pointed at the `memmap2-rs` submodule,
  which the OS tree already uses.
- `src/ports/socket2` stays until/unless upstream takes it.

Net: **+1 tracked port instead of +4.**
