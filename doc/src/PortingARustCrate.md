# Porting a Rust Crate to Twizzler

NOTE: this guide is incomplete, and needs to be expanded and verified.

While many Rust crates work on Twizzler just fine, some rely on OS-specific functionality or APIs internally. These crates must be ported before they will work. Often times, these crates will fail to compile.

## Some basic Rust concepts

A common way to do OS-specific code in Rust is use a cfg directive:
```{rust}
#[cfg(target_os = "linux")]
mod linux;
```

Another common thing you may see is switching on the target_os value to select a particular implementation of an "OS functionality" module:

```{rust}
#[cfg(target_os = "linux")]
#[path = "linux.rs"]
mod imp;
```
and then later, do something like `imp::foo()`. The crate may have a number of different implementations that all provide the necessary interface.

## A very not guaranteed to work step-by-step process for libraries

1. Fork the crate you want to port, and get a local checkout.
2. Have some existing Twizzler binary add the crate to its dependencies: `foo = { path = "path-to-local-checkout"}`.
3. Build twizzler, collect errors. Make sure this built your crate (look for its name in the output of cargo).
4. Fix the errors! Surely this part wont be too bad...
5. Once it's built and working, you can change the dependency to reference your git fork.

## What if I need to port a dependency of a dependency?

After you fork it, add to Cargo.toml (in twizzler root dir) where it says "patch.crates-io", something like:
`foo = { git = "path to your fork" }`

## Okay but how do I actually do step 4?

Yeah, okay, that is the hard part. You'll need to look through the code and find:
1. Where are errors happening, either at runtime or compile time?
2. Where is there any OS-specific code? (hint: grep for "target_os")

Usually this means having a decent understanding of overall how the project is organized and what the code is doing, so I suggest that you start by exploring the codebase for the crate and learning about what its doing and why.

Another suggestion is to look at what less established OSes are doing. If this is a popular crate, you may get lucky and find support for hermit or fuchsia. If so, you can look to see how they are implementing support, and what they did to port this crate.

