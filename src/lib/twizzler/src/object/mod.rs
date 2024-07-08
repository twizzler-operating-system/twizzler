//! Core Twizzler Object APIs.
//!
//! # Object Construction
//!
//! The primary mechanism for creating objects in Twizzler uses the Builder pattern. An
//! [ObjectBuilder] allows a programmer a number of convenience features for creating objects,
//! statically allocating regions within them at construction time, and safely initializing the Base
//! of the object.
//!
//! ## Simple Base types (can be constructed out-of-place)
//! For simple base types that can be constructed anywhere and copied into the object, the object
//! builder provides a [ObjectBuilder::build] method that initialized the object base and constructs
//! the object, returning an open handle to the new object.
//!
//! ```{rust}
//! struct Foo { x: u32 }
//! impl BaseType for Foo {}
//! const DEF_SPEC: ObjectCreate = ObjectCreate::new(BackingType::Normal, LifetimeType::Volatile, None, ObjectCreateFlags::empty());
//! let foo_object = ObjectBuilder::new(DEF_SPEC).build(Foo {x: 42}).unwrap();
//! ```
//!
//! ## Complex Base types
//! Some base types require arbitrary work to build, and for these, we provide a
//! [ObjectBuilder::construct] method that takes a closure that constructs the object base type.
//! When this closure runs, it has access to a [ConstructorInfo] struct that contains a reference to
//! the object being created as an _uninitialized object_ (since by definition we have not yet
//! constructed the base).
//!
//! ```{rust}
//! struct Foo { x: u32 }
//! impl BaseType for Foo {}
//! const DEF_SPEC: ObjectCreate = ObjectCreate::new(BackingType::Normal, LifetimeType::Volatile, None, ObjectCreateFlags::empty());
//! let foo_object = ObjectBuilder::new(DEF_SPEC).construct(|info| Foo {x: 42}).unwrap();
//! ```
//!
//! ## Static allocations
//! A common pattern is to allocate, statically, a region of the object to be used during runtime,
//! and then point to it from the base of the object. The [ObjectBuilder] provides the
//! `allocate_static` function for this, which takes a value of type T, and then allocates space for
//! the T in the object, after the base data. For example, the Queue object contains, in its base, a
//! pointer to the buffer, which is contained in the same object.
//!
//! Here is an example that constructs such an object:
//!
//! ```{rust}
//! let builder = ObjectBuilder::new(DEF_SPEC);
//! let foo_obj = builder.construct(|_obj| Foo { x: 42 }).unwrap();
//!
//! let builder = ObjectBuilder::new(DEF_SPEC);
//! let bar_obj = builder.construct(|mut ctorinfo| {
//!     // Build an invariant pointer to foo_obj's base data. This is a little funky here because it's bootstrapping -- the new object hasn't been constructed yet, so we can't safely use it directly yet.
//!     let foo_ptr = ctorinfo.new_invptr(foo_obj.base().into());
//!     Bar { x: foo_ptr }
//! }).unwrap();

mod builder;
pub use builder::ObjectBuilder;

pub mod fot;
pub mod meta;

mod base;
pub use base::BaseType;

mod ctrl;
mod objtypes;
mod stat;

pub use objtypes::*;
