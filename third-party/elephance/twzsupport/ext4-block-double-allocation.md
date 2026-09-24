# Guest writes to the object store double-allocate ext4 blocks and corrupt unrelated files

Found 2026-09-13 while debugging llama-twz-72's boot-2 `libnet_srv.so(unloaded)` panic. This is a
**data-corruption bug**, not a loading bug, and it is the real cause.

## Evidence

`e2fsck -fn` on the data image after one guest boot that created store objects:

    Multiply-claimed block(s) in inode 32411: 1488486--1488488
    Multiply-claimed block(s) in inode 32412: 1488489--1488491
    Multiply-claimed block(s) in inode 32413: 1488492--1488539
    Multiply-claimed block(s) in inode 32458: 1488486--1488539

    File /ids/47/4741473a3898f44b585a9e262b0d9087 (inode #32458)
      has 54 multiply-claimed block(s), shared with 3 file(s):
        /sysroot/pkg/twizzler/lib/libpager_srv.so   (inode #32413)
        /sysroot/pkg/twizzler/lib/libnet_srv.so     (inode #32412)
        /sysroot/pkg/twizzler/lib/libnaming_srv.so  (inode #32411)

    Free blocks count wrong for group #45 (5143, counted=5197)
    Free blocks count wrong (23595075, counted=23595126)
    Free inodes count wrong (6520195, counted=6520194)
    ********** WARNING: Filesystem still has errors **********

**A guest-created store object was allocated 54 blocks that three server libraries already owned**,
and writing it overwrote their contents. The libraries' directory entries, inodes and sizes are
intact — `libnet_srv.so` still dumps a valid ELF header at the right length — so nothing *looks*
wrong until something reads the overwritten region. `dynlink` then fails the load and reports
`libnet_srv.so(unloaded)`.

## Why it presented as a "second boot" bug

The corrupting boot succeeds: it writes its own object and never reads the libraries it clobbered
(they were already loaded). The *next* boot loads `libnet_srv.so` from the damaged blocks and dies
in `init::initialize_network` before anything else runs. So the symptom appears one boot after the
cause, in an unrelated subsystem.

That also refutes the natural reading, which two of us held: both boots resolved the same persisted
dataroot and were **identical** through `caching library directories`, `starting cache service` and
`cache manager ready`. The persisted root was never the trigger.

## Scope, and why it matters more than what we were chasing

- The free-block and free-inode counters are **also** wrong, so the allocator's accounting is
  inconsistent with reality, not merely unlucky in one placement.
- Any file in the image can be the victim; here it happened to be three of the servers that init
  loads at boot, one of which it cannot start without.
- **Guest-side persistence currently corrupts the store.** Every result that depends on writing
  objects from inside the guest -- the KV cache across reboot, any persistent dataset -- is
  building on a filesystem that fails `e2fsck` afterwards.

## Not yet established

- Which writer double-allocates: the block allocator in `lwext4` under `pager-srv`, or the store
  layer's use of it. The blocks are contiguous and adjacent to the victims' ranges, which reads more
  like a bitmap/accounting fault than a random stray write.
- Whether it reproduces on a fresh data image. It should, if the allocator is at fault; that is the
  first experiment, and it is cheap.
- Whether earlier "persistence works" results are affected. The object I validated the persist fix
  with (`/ids/b2/b21fea…`) was in a *different* image, which has not been fsck'd.
