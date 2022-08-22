# Direct Memory Access (DMA)

A key aspect of a device driver involves programming the device to access host memory. When a device
accesses host memory, we usually call it Direct Memory Access (DMA). DMA is used, for example, by
NICs to access transmit rings, or to copy packet data into main memory (memory that the CPU, and
thus the OS and user programs can access). However, devices access main memory differently to how
threads running on a CPU access memory. Before we discuss the API that Twizzler provides for DMA, we
should discuss how devices access memory, and the implications this has for memory safety,
translation, and coherence.

## Considerations for DMA

When programs access memory in Twizzler they do so via accessing object memory, which involves an
MMU translating some kind of object address to a physical address. On x86, for example, this
involves a software translation to a virtual address followed by a translation via the Memory
Management Unit (MMU) to a physical address. Similarly, when a device accesses memory, it emits a
memory address (likely programmed by the driver) that may undergo no translation or some other
translation on the bus before attempting to access host memory. There are two important
considerations that are the result of this alternate (or no) translation:

 - **Contiguous addresses**. While object memory is contiguous (within an object), the physical memory that
backs that object memory may not be. Devices and drivers need to be capable of handling access
to memory in a scatter-gather manner.
 - **Access Control**. Access control can be applied differently between host-side driver software and
devices. Thus driver software must be aware that it may have access to memory via the device that it
should not directly. We can use devices like the IOMMU to limit this effect.

In addition to the above, we need to consider the issue of coherence. While CPU caches are coherent
across cores, devices accessing host memory do not necessarily invalidate caches. Thus we have to
handle both flushing data to main-memory after writing before the device reads it and invalidating
caches if a device writes to memory. Some systems automatically invalidate caches, but not all do.

### Memory Safety

Finally, we must consider memory safety. While we can control writes from host software to DMA
buffers, we cannot necessarily control how the device will access that memory. To ensure memory
safety of shared regions, need to ensure:

 1. The device and host software cannot both mutate shared state at the same time (thread safety),
    or if this can happen, then the shared memory region that can be updated by both entities is
    comprised of atomic variables.
 2. The device mutates data such that each mutation is valid for the ABI of the type of the memory
region.

Enforcing these at all times would add significant overhead. We take some inspiration from Rust's
stance on [external influences to
memory](https://doc.rust-lang.org/std/os/unix/io/index.html#procselfmem-and-similar-os-features),
tempering this somewhat with the addition of a `DeviceSync` marker trait.

## Overview of DMA System

The Twizzler DMA system is contained within the twizzler-driver crate in the `dma` module. The
module exposes several types for using Twizzler objects in DMA operations along with an abstraction
that enables easier allocation of DMA-able memory. The key idea behind Twizzler's DMA operation is
that one can create a `DmaObject`, from which one can create a `DmaRegion` or a `DmaSliceRegion`.
These regions can then be "pinned", which ensures that all memory that backs them is locked in place
(the physical addresses do not change), and the list of physical addresses that back the region are
made available for the driver so that it may program the device.

### Coherence and Accessing Memory

The primary way that the driver is expected to access DMA memory is through the `DmaRegion`'s `with`
or `with_mut` method. These functions take a closure that expects a reference to the memory as
argument. When called, the `with` function ensures coherence between the device and the CPU, and
then calls the closure. The `with_mut` function is similar, except it passes a mutable reference to
the closure and ensures coherence after the closure runs as well.

The `DmaSliceRegion` type provides similar `with` functions, except they take an additional `Range`
as argument that can be used to select only a subslice of the region that the closure gets access
to. Allowing for subslicing here is useful because it allows the driver to communicate to the
library which parts of the region need coherence before running the closure.

### Access Directions and Other Options

Regions can be configured when they are created for various different use cases.

The **Access Direction** refers to which entities (the device and the CPU) may read and write the
memory. Driver writers should pick the most restricted (but correct) mode they can, as is can have
implications for maintaining coherence. It can have one of three values:

 - HostToDevice: The memory is used for the host to communicate to the device. Only the host may
   write to the memory.
 - DeviceToHost: The memory is used for the device to communicate to the host. The host may not
   write to the memory.
 - BiDirectional: Either entity may write to the memory.

In addition to access direction, regions can be configured with additional options, a bitwise-or of
the following flags:

 - UNSAFE_MANUAL_COHERENCE: The `with` functions will not perform any coherence operations. The
   driver must manually ensure that memory is coherent.

### Pinning Memory

Before a device can be programmed with a memory address for DMA, the driver must learn the physical
address that backs the DMA region while ensuring that that address is stable for the lifetime of
whatever operation it needs the device to perform. Both of these are taken care of with the `pin`
function on a `DmaRegion` or `DmaSliceRegion`. The `pin` function returns a `DmaPin` object that
provides an iterator over a list of `PhysInfo` types, which can provide the physical address of a
page of memory.

A region of DMA memory that comprises some number of pages (contiguous in virtual memory) can
list the (likely non-contiguous) physical pages that it maps to. The order that the pages are
returned in is the order that they appear for backing the virtual region. In other words, the 4th
`PhysInfo` entry in the iterator of a `DmaPin` for a region contains the physical address of the 4th
virtual page in the DMA region.

Any future calls to `pin` return another `DmaPin` object, but the underlying pin information (that
is, the physical addresses) may be the same, even if the `DmaRegion` is dropped and recreated.
However, if the `DmaObject` is dropped and recreated, the driver cannot rely on the pin to be
consistent. More specifically, the pin's lifetime is tied to the `DmaObject`, not the `DmaRegion`.
The reason for this somewhat conservative approach to releasing pins is to reduce the likelihood of
memory corruption from accidental mis-programming. Another consideration for pinned memory lifetime
is that it can leak if the driver crashes. Allowing for leaks in this case is intentional, as it
makes it less likely that the device will stomp over memory in the case of a driver crash.