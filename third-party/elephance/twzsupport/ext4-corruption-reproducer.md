# A one-boot reproducer for store-write filesystem corruption

2026-09-13. Follows `ext4-block-double-allocation.md`, which found llama-twz-72's boot-2
`libnet_srv.so(unloaded)` panic was caused by guest writes double-allocating ext4 blocks.

## The reproducer

    # image verified e2fsck-clean beforehand: 0 multiply-claimed, 0 count errors
    cargo start-qemu --profile release --autostart "elephance build --entries=2000000 --name=bigprobe"
    e2fsck -fn target/disk-x86_64-unknown-twizzler.img

One boot, ~2 minutes. Creates a 2M-entry `PersistentHashMap` in a persistent object (scattered
writes by hash, through the **mapping**), then syncs. `t_insert_us=1989536`, `t_sync_us=384`,
build reports success.

**Result: the filesystem is corrupt afterwards.** `e2fsck` exits 12 and **aborts** partway:

    Inode 546037 seems to have inline data but extent flag is set.
    Inode 546037 has INLINE_DATA_FL flag on filesystem without inline data support.
    Inode 546038 has corrupt extent header.  Clear inode? no
    Inode 546038 has the casefold flag set but is not a directory.
    Inode 546040 has a extra size (36267) which is invalid
    Inode 546036/37/38/40/41 is in use, but has dtime set.
    ********** WARNING: Filesystem still has errors **********

Five inodes in the contiguous range **546036..546041**, carrying flags that cannot coexist
(inline-data on a filesystem without inline-data support, casefold on a non-directory, imagic,
INDEX_FL on a non-directory) and junk `i_block` / extra-size fields. That is not a
mis-set flag; **it is file data written over the inode table.**

## Two distinct signatures, same family

| | llama's image | this reproducer |
|---|---|---|
| trigger | twzm KV buffer, truncate-to-size then sparsely dirtied, durable sync | 2M-entry PHM, scattered mapped writes, sync |
| damage | **54 multiply-claimed blocks** — one store object sharing blocks with 3 server libraries | **5 inodes of garbage** at 546036..546041; **0** multiply-claimed |
| victim | `libnet_srv.so`, `libnaming_srv.so`, `libpager_srv.so` contents | the inode table itself |
| free counts | wrong (blocks and inodes) | fsck aborts before reaching pass 5 |

Different victims, same class: **the store writes outside the extents it owns.** One landed on other
files' data, one on metadata.

## What this rules out

An earlier hypothesis — large *sparse* file with scattered extents — is **refuted**. Writing 512
scattered 4 KiB pages through a ~1 GB sparse file via `std::fs` (`mk-file --span`) completed and left
the image **clean**. The difference that matters is the **writer**: `std::fs` goes through
`RawFile`; this goes through the pager's page-out path for dirty mapped pages. The corrupting path
is page-out, not file write.

## RESOLVED 2026-09-16: mballoc counted scattered clear bits as a contiguous run

Root cause (found statically, fixed, and verified with this reproducer): fork commit `2e3dd89
Implement mballoc` in `src/ports/lwext4-rs/lwext4` added `ext4_bmap_count_empty_bits`, which
counts clear bits **without stopping at a set bit**, while all three call sites in
`__ext4_balloc_alloc_multiple_block` treat the return as a contiguous free run at the first clear
bit and claim `[first_clear, first_clear+count)` wholesale — any in-use block inside the window
is allocated a second time. `a66f17a`'s early-break fix never touched the scattered count.

It explains both signatures in the table above:

- **54 multiply-claimed + group #45 free count off by exactly 54**: re-setting already-set bits
  is idempotent, so the bitmap never changed for the stolen blocks; only the counters moved.
  Counter-skew == steal-count is this bug's fingerprint.
- **Inode-table garbage**: flex_bg clusters inode tables; a claimed range crossing set bits
  covers them, and page-out writes file data direct-to-device at those physical blocks.
- **Scale threshold**: fresh regions have genuinely contiguous free runs (5,000 entries clean);
  2M hash-scattered allocations fragment the bitmap and the bug fires.

Fix (submodule, 2026-09-16): `ext4_bitmap.c` — the counter stops at the first set bit, making
the first-fit scan's `tmp = rel + thiscount + 1` advance exactly right; `ext4_balloc.c` — the
retry loop in `ext4_balloc_alloc_multiple_blocks` no longer spins forever holding
`block_alloc_lock` on ENOSPC-at-count-1 or on non-ENOSPC errors (a latent pager wedge on EIO).

Verified: fix confirmed compiled (object mtime + disassembly shows the early-exit, on the
x86_64-unknown-twizzler triple), fresh private image fsck-clean baseline, then this exact
reproducer (`entries=2000000`, `t_insert_us=1869943`) → `e2fsck -fn` **exit 0, passes 1-5
clean**, where the unfixed tree gave exit 12 and five garbage inodes. n=1, but the failure was
2/2 deterministic before the fix.

## Still open (pre-fix notes, superseded by the above)

- **Which layer**: lwext4's block allocator / bitmap accounting, or the store's use of it
  (`ext4.rs`'s i_size extend and page-out sizing). The tight inode range suggests a single bad
  extent rather than scattered stray writes.
- **Whether e2fsprogs normalisation matters.** llama's image was `e2fsck -fy` repaired immediately
  before its corrupting boot; mine was not repaired, only verified clean. Their `twztwz.md` §6
  records the mirror hazard (lwext4 leaving summaries e2fsprogs disagrees with). One run cannot
  separate "lwext4 accounting is wrong" from "normalisation confuses lwext4".
- **Scale threshold**: 5,000 entries (earlier `persistprobe`) left the image clean; 2,000,000
  corrupts it. Somewhere between is the smallest failing case, which would make bisection faster.
