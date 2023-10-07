//! Welcome to the dynamic linker.
//!
//! The job of the dynamic linker (dynlink) is:
//!   1. Load dynamic shared objects (DSOs) and their dependencies.
//!   2. Fixup all the relocations inside those DSOs.
//!   3. Manage TLS regions
//!
//! On the surface, this isn't too bad. But it's mired in a long history, compatibility, deep
//! magic for performance, and a lack of good, easy to understand "official" documentation. However, we
//! will, in this crate, try to be as clear and forthcoming with what we are doing and why.
//!
//! # Basic Dynamic Linking Concepts
//! *What is a dynamic shared object (DSO)?*
//! Practically speaking (and for our purposes), it's an ELF file that has been prepared in such a way that we can load it
//! into memory, fix it up a bit based on where we loaded it (the file is relocatable), and then call code within it. The
//! overall process looks like this:
//!
//! Loading:
//! 1. Map the library into memory
//! 2. Register TLS template, if the library has one
//! 3. Register constructors, if any.
//! 4. Insert the library into the global dependency graph (a directed graph, possibly with cycles)
//! 5. For each dependency, recurse
//! 6. Add edges from the library to each dependency
//!
//! Relocating (from a starting point DSO):
//! 1. If marked as in-progress or done, return.
//! 2. Mark as in-progress.
//! 3. Recurse on all dependencies
//! 4. For each relocation entry,
//!    4a. Fixup the relocation entry according to its contents, possibly looking up a symbol if necessary.
//! 5. Mark as done
//!
//! Let's talk about loading first. In step 1, for example, we need to iterate the program headers of the ELF file,
//! looking for PT_LOAD statements. These statements tell us how to setup the virtual memory for this program. Since these
//! DSOs are relocatable, we can load them _at a specific base address_. Each DSO gets loaded to its own base address and
//! is mapped into memory according to the base address and the PT_LOAD entries. In Twizzler, we can leverage the powerful
//! copy-from primitive to make this easier.
//!
//! In steps 2 and 3 we are noting down information ahead of time. We want to record the loaded libraries for TLS purposes
//! in this order, since we must reserve one exalted DSO to live right next to the thread pointer. On most systems, this is
//! reserved for the executable. For us, it's just the first DSO to be loaded. We also note down if this library has any
//! constructors, that is, code that needs to be run before we can call any other code in the DSO.
//!
//! In step 4, we just add the library into global context. At this point, we have recorded enough info that we can make
//! this library namable and searchable for symbols. Finally, in the last two steps, we recurse on each dependency, and
//! add edges to the graph to note dependencies.
//!
//! When relocating a DSO, we need to ensure that it is fixed up to run at the base address we loaded it to. As a simple
//! mental model, we can imagine that if we had some static variable, foo, that lives in a DSO. When linking, the linker
//! has no idea where the dynamic linker will end up putting the DSO in memory. So when accessing foo, the compiler emits
//! some _relative_ address for reaching foo, say "0x300 + BASE", where BASE is a 64-bit value in the code. But again,
//! we don't know the base address, so we need to emit an entry in a relocation table that tells the dynamic linker, "hey,
//! when you load this DSO, go to _this spot_ (where BASE is) and change it to the actual base address of the DSO".
//!
//! In practice, of course, its more complex, there are optimizations, there are indirections, etc, but this is basically
//! the idea. In the steps listed above, we perform a post-order depth-first walk over the graph, performing all relocations
//! that the DSO specifies.
//!
//! One key idea that happens in relocations is _symbol lookup_. A relocation can say, "hey, write into me the address of
//! the symbol foo", and the dynamic linker will go look for that symbol's address by name. This is possible because each
//! DSO has a symbol table for symbols that it is advertising as useable for dynamic linking. The dynamic linker thus, when
//! looking up symbols, transitively looks though a DSO's dependencies until it finds the symbol. If it doesn't, it
//! falls back to a global lookup, where it traverses the entire graph looking for the symbol.
//!
//!
//!
//! # Basic Concepts for this crate
//!
//! ## Context
//! All of the work of dynlink happens inside a Context, which contains, essentially, a single "invocation" of the dynamic
//! linker. It defines the symbol namespace, the compartments that exist, and manages the library dependency graph.
//!
//! ## Library
//! This crate calls DSOs Libraries, because in Twizzler, there is usually little difference.
//!
//! ## Error Handling
//! This crate reports error with the [error::DynlinkError] type, which implements std::error::Error. One specific quirk
//! is that this type can report a collection of errors, designed to allow reporting more than one error in situations where
//! it would be annoying to get errors one at a time. For example, during relocation, symbols are looked up. If many symbol
//! lookup failures occur, it would be nice to see reporting for all of them instead of just the first one.
//!
//! ## Compartments
//! We add one major concept to the dynamic linking scene: compartments. A compartment is a collection of DSOs that operate
//! within a single, shared isolation group. Calls inside a compartment operate like normal calls, but cross-compartment
//! calls or accesses may be subject to additional processing and checks. This doesn't change a lot of the core
//! functionality of the dynamic linker, except for a few things (primarily TLS handling and allocation).
//!
//!
//!

#![feature(strict_provenance)]
#![feature(never_type)]
#![feature(iterator_try_collect)]
#![feature(allocator_api)]
#![feature(result_flattening)]
#![feature(alloc_layout_extra)]

pub mod addr;
pub(crate) mod arch;
pub mod compartment;
pub mod context;
pub mod error;
pub mod library;
pub mod symbol;
pub mod tls;
pub use error::*;
