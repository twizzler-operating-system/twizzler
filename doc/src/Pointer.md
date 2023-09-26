# Pointers

<!-- Page 10-12 of Twizzler: a Data-Centric OS for Non-Volaile Memory -->
## Definition

There are two types of pointers in Twizzler: persistent and dereferenceable. Persistent pointers are used in order to refer to external data with no extra context needed. They can be thought of file names on a traditional operating system, where the data exists longer than any process or power cycle. Dereferenceable pointers are references to data in ways that act like traditional memory accesses when programing on other operating systems (such as stack or heap data). Unlike persistent pointers, the data can be acted upon, such as by reading or writing, but additional context is necessary.

## Rationale

Persistent pointers are much more efficient than file I/O, as there is a link to data from the pointer without the need for deserialization of a file. This just allows links of data in data structures, which is what objects are.

## Foreign Object Table

Persistent pointers work by indexing into a Foreign Object Table (FOT), which holds a longer reference to the data, allowing for late binding of names. Late binding in FOTs are explained in more detail in a [later section](#late-binding). Persistent pointers are thus just an index of external object wanted (16 bits, allowing for 65,536 object references), and an offset within the foreign object (40 bits, a maximum offset of 1 terabyte). Because access control is at an object granularity, multiple pointers to the same object can use the same FOT entry.

### Late-Binding

Late binding of names is used often with libraries to allow for updates of the library without requiring every program using the library to be recompiled. In Twizzler, late-binding is done by putting a name as the entry in the FOT. When creating the FOT entry, a name resolver can also be specified to allow for different objects to have different name resolvers. The actual name resolution happens when converting the persistent pointer to a dereferenceable one, allowing for different objects to be resolved at different times, just based on the name at dereference time.
