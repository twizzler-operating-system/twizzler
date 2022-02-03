% Object Lifetime

## Volatile Memory

Placing objects in volatile memory (such as DRAM) limits the object lifetime to at most the time of the next power cycle. This can provide easy cleanup for temporary data, such as the result of computation or cached data kept in memory for locality (faster access).

Objects in volatile memory can be accessed and used in the same ways they are in non-volatile memory.


## Ties

<!-- page 7 of Twizzler: a Data-Centric OS for Non-Volatile Memory -->

Ties handle object lifetime by allowing for automatically deleting objects once other constructs are deleted. For example, if an application crashes, the temporary computation might be useless, yet keeping the temporary computation in volatile memory until a power cycle occurs is a waste of that space. Instead, we can tie the lifetime of the object to other objects, such that the object is automatically deleted with the other, freeing the memory before a power cycle. This mechanism is convenient because the kernel does not have to maintain an understanding of the implied lifetime of an object, rather it can be specified relative to other objects.

Ties also provide the benefit of allowing temporary context (such as stacks and heaps) to be stored in persistent memory, allowing for recovery after a power cycle.

For example if we have two objects: koala and coldbrew, tying koala to coldbrew means that koala will not be deleted until after coldbrew is. While koala can be deleted immediately after coldbrew is, if koala is tied to multiple objects, it will only be fully deleted when the final object it is tied to is deleted. Additionally, if koala is tied to a handful of objects, once all of those objects are deleted, koala will be automatically deleted too.

Since most construts in Twizzler are just various types of objects, we can use ties to establish a lifetime of objects based on the existence of other objects, such as threads or views. For a more detailed explanation of views, see the [page on views](./Views.md). For temporary computation done in a thread, an instance of computation, objects can be tied to the thread. This provides similar semantics to creating and immediately unlinking files on Unix, as with both, once the application exits, the data is deleted. With views, this provides an address space for an application to run in, possibly over multiple time periods. Tying an object to a view allows the object to exist for as long as the execution state of the application, which could be as long as the application is installed, or only removed when all application data is deleted.

### Ties to Volatile Memory

Tying volatile objects to each other implies that both objects will be deleted at a power cycle, which is to be expected. However, things get a little more complicated with ties between volatile and persistent objects. Tying a volatile object to a persistent one breaks the semantics of ties in the event of a power cycle, as the volatile object is deleted and the persistent one is not. However, this outcome is to be expected, and we assume programmers will use this when doing temporary computation. Tying a persistent object to a volatile object is dangerous as both objects will be deleted in the event of a power cycle, including an unexpected one.


