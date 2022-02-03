% Pointers

<!-- Page 10-12 of Twizzler: a Data-Centric OS for Non-Volaile Memory -->
## Definition

There are two types of pointers in Twizzler: persistent and dereferenceable. Persistent pointers are used in order refer to external data with no extra context needed. They can be thought of file names on a traditional operating system, where the data exists longer than any process or power cycle. Dereferenceable pointers are references to data in ways that act like traditional memory accesses when programing on other operating systems (such as stack or heap data). Unlike persistent pointers, the data can be acted upon, such as by reading or writing, but additional context is necessary.

## Rationale

Persistent pointers are much more efficient than file I/O, as there is a link to data from the pointer without the need for deserialization of a file. This just allows links of data in data structures, which is what objects are.

## Foreign Object Table

Persistent pointers work by indexing into a Foreign Object Table (FOT), which holds a longer reference to the data, allowing for late binding of names. Late binding of names is used often with libraries to allow for updates of the library without requiring every program using the library to be recompiled.

### Late-Binding
