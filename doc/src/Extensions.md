# Extensions

This is the interface abstraction in many programming languages, or the functions that must be implemented for drivers, such as `read()` and `write()`. In practice, Twizzler's implementation of this applies to objects, where a set of methods are defined and noted such that external threads accessing the object can call the interface methods without a need to understand the specifics of under the hood operations for the object.

## Examples

Two examples of extensions are IO and Event. IO is useful for reading and writing to an object, and for an object to support the extension, the object must implement the functions `read()`, `write()`, `ioctl()`, and `poll()`. When registering the extension, the object will provide pointers to all of the functions, so calls to `read()` for example on the object will know how to implement the function in an object specific way.

Event is a way of waiting for something to happen to an object, similar to `poll()` on a file descriptor in Unix. Specific events can be waited for by using `event_wait()` with the object and event passed in as arguments. Because this is just an interface, an object can implement it in a way that makes sense to it, such as waiting for data from a network or a write to an object to complete.

## Tags

Tags are a way of uniquely identifying an extension, such as IO, and checking if the object supports the extension. These are stored in the metadata for the object, and when the tag is added to the metadata, a pointer to the functions that implement the interface are also added.
