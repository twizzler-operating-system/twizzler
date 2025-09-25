# Runtime

## Programs in Twizzler

A standard Rust program running on Twizzler links to two libraries (at a minimum):

  - libstd.so, the Rust standard library
  - A Twizzler Runtime, exporting the Twizzler Runtime ABI

The Rust standard library, when targeting Twizzler, expects the Twizzler Runtime
to exist (in particular, a number of functions defined prefixed by twz_rt_). Thus
programs targeting Twizzler will need to pick a runtime to use, and a compilation
method. Right now there are really only two options: minimal runtime (with static
compilation) or the reference runtime (with dynamic compilation). 

The reference runtime is more featureful and is the default. The default Twizzler
target will compile programs as dynamic executables that can link (dynamically) to
the standard library, the reference runtime (and the monitor), and any other
dynamic libraries. No additional setup is needed for this.

Another option is the minimal runtime. This runtime doesn't depend on libstd, nor
on the security monitor or other OS features other than the kernel. Currently, this
runtime option only supports static linking, and as a result, some additional work
is needed to make it happen. This is not officially supported, however, and I suggest
you stick to the default, above. If you insist that you want this, however, take a
look at src/bin/bootstrap's Cargo.toml.

## The Runtime ABI

The ABI for the runtimes is defined in the src/abi submodule. This submodule contains
header files that are fed into bindgen to generate the actual ABI and API definitions.
The reason is that this way we can support additional languages in the future, as long
at they can talk C ABI. In any case, the src/abi/rt-abi crate contains the bindings for
the generated runtime ABI as well as a set of convenience functions for calling those
with a more Rust-like interface.

## ABI Version Compatibility

When compiling, the build system will check the version of the installed toolchain
(the one built and installed by cargo bootstrap) and compare it to the version in
the source tree. If either the Rust commit ID or the crate version of twizzlert-rt-abi
differ between the source tree and the installed version, the build system will error
out and refuse to compile.

In the future, we plan to use semver to make this check less strict.

# The Twizzler Reference Runtime

The primary runtime environment supported by Twizzler is the _Reference
Runtime_. It provides userspace functionality for programs, the ability to load
programs and libraries, and the ability to isolate those programs and libraries
from each other.

TODO

# The Twizzler Minimal Runtime

TODO
