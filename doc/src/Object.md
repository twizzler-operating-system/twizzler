# Objects

## Definition

Objects are an abstraction of a set of related data with the same lifetime and permissions. This vague definition allows for applications to define what data is contained in a single object in a way that is most reasonable for the particular use case. For example, a B-Tree could contain all nodes in the same object given that the nodes likely have the same permissions and lifetime. However, another tree with different permissions for children could separate these nodes into different objects. For this second example, managing their lifetime can be done with [ties](./Lifetime.md#Ties).

Kernel interposition is only done when creating and deleting objects, leaving access and modification to userspace facilities and hardware. Access control is limited by specifying policies and letting hardware enforce those policies. This allows the kernel to avoid involvement in access, improving performance without sacrificing security. Objects maintain a reference count to prevent deletion of object data when multiple pointers reference it.

## Object Creation

When creating objects, the medium storing data can be chosen, such as choosing between volatile DRAM and non-volatile memory. While these options are supported by default, other types can be configured based on the hardware support of the particular machine. Different storage mediums provide different benefits and costs and a more in depth discussion can be found at [lifetime](./Lifetime.md).

When creating objects, a source object can be denoted, where the new object will be a copy of the original. This allows for easy versioning, as objects can be copied and kept as different versions. Copying an object uses copy-on-write, meaning another copy of the data is only created when a change is made, rather than immediately on creation.

## IDs

While 2<sup>64</sup> object IDs provides a large enough space for a single computer's address space without worries of running out object IDs, adding the ability to generate IDs without having to interact with a central authority and the Twizzler's future of a transparent single id space on a distributed set of computers creates the possibility of collisions. Thus 128 bits are used to shrink the possibility of collisions and the ability to guess an object ID, while also creating a large enough ID space to allow for distribution.

### ID derivation

IDs are derived by inputting a nonce, 128 bits of random data, to a hash function. The nonce is provided for objects created using copy-on-write, the objects where a `src` points to a valid object when calling `twz_obj_new()` so as to create unique IDs despite having the same object content.

There is also the ability to create object IDs by hashing the contents of the object. This is most useful for conflict-free replicated data types (CRDTs), where multiple computers are running distributed Twizzler and can aggresively replicate objects without worrying about consistency issues. Hashing to obtain an object ID is designed for immutable objects.

<!-- CRDT paper, read and explain
https://hal.inria.fr/hal-00932836/file/CRDTs\_SSS-2011.pdf
-->
