# `OpenKind::Object` in the reference runtime

Unblocks llama.cpp (`/scratch/dbittman/llm/llama-twz`) loading a model on Twizzler:
`fd_for_objid()` in `twzm/twizzler_twz.cpp` opens an fd on an object id purely to read
one `u64` (the length), and every model segment's map goes through it. Written here
rather than in the llama tree because the fix is runtime source.

## The gap

`src/rt/reference/src/runtime/file/kinds.rs::open()` handles Path, Pipe, both Pty
kinds, Compartment, the three socket kinds, KernelConsole and Kqueue, then
`_ => Err(ErrorKind::Unsupported)?` (kinds.rs:349). `OpenKind::Object` lands there.

**Not a deliberate rejection.** `OpenKind_Object` is the second enumerator in the C ABI
(`src/abi/include/twizzler/rt/fd.h:61`), `object_bind_info` is declared alongside it
(fd.h:101), and the minimal runtime rejects *everything* but Path
(`src/rt/minimal/src/runtime/syms.rs:423`). No runtime has ever implemented it; nothing
in the tree states a policy against it.

## Shape

Two helpers factored out of `open_path`, verbatim, so the two paths cannot drift when a
flag is added (twizzler-0a's ask: error branches stay byte-identical -- `check_truncate`
returns the same `TwzError::INVALID_ARGUMENT` the inline test returned, and the flags
match keeps all four arms rather than collapsing to `_ => READ`):

    fn map_flags_for(open_opt: OperationOptions) -> MapFlags
    fn check_truncate(open_opt: OperationOptions) -> Result<()>

then the arm itself: `RawFile::open(ObjID::new(info.id), map_flags_for(opts))`, the same
three lines `open_path` ends with for `NsNodeKind::Object`, plus the truncate handling.

## Inherited semantics the caller needs to know

1. **A read-only open of an object with no `MEXT_SIZED` reports length 0, it does not
   fail** (`raw_file.rs:305-319`). Only a `WRITE` open creates the ext (set to 0) and
   stamps a creation mtime. So `create_fresh -> size to 40997` works iff the open asks
   for write.
2. **Ask for `OPEN_FLAG_READ|OPEN_FLAG_WRITE`, not `OPEN_FLAG_WRITE` alone.**
   `RawFile::open` reads the meta page to find `MEXT_SIZED`; the flags match maps
   write-only to `MapFlags::WRITE` with no `READ`. On amd64 that happens to work (x86-64
   has no write-only page), but it is accidental and aarch64 need not be so kind.
   Parity with `open_path` is deliberate here -- diverging would make `OPEN_FLAG_*` mean
   different things depending on which `OpenKind` reached it.
3. `MAX_SIZE - NULLPAGE_SIZE` is the truncate ceiling (`raw_file.rs:346`), i.e. a single
   object is the unit of length; segments larger than that need more objects regardless.

## Ordering inside the arm

`check_truncate()` runs **before** `binding_ref()`, not after. `open_path` validates the flag
combination before it does any work (kinds.rs:116, ahead of the name lookup), and the arm should
agree: when a caller gets both wrong -- an undersized `object_bind_info` *and*
`OPEN_FLAG_TRUNCATE` without write -- the error it sees should not depend on which `OpenKind`
it used. Costs nothing, and it is the kind of divergence that is invisible until someone is
debugging errno translation.
