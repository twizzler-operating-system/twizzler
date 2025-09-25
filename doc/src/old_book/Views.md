# Views

Views are an address space abstraction that sets an environment for threads to run in. Persistent objects are mapped into the view and given dereferenceable virtual pointers. These virtual pointers allow access to the object inside the view.

Because views are normal objects, they can be written on persistent memory and allow recovery of application state in the event of a crash or power cycle. The abstraction ov views also allows easy sharing of thread state, such as references to data. This is convient as it allows for sharing of data without requiring serialization through a construct like a pipe or file, and without the need of a call to `mmap`. Sharing views with other threads does provide a security threat, as one thread could corrupt the view for both. To deal with this security vulnerability, there is the abstraction of [secure API calls/gates](./Gates.md) which allows communication between threads without allowing one or the other to corrupt another's data.

When a thread wants to map a new object into the view, they can call _____ and when they attempt to access the object, the kernel will automatically map it in. To change or remove an entry, the kernel must be involved with the function `invalidate_view()`, to update references to the underlying memory.

To switch between views, the system call `become()` is needed.
