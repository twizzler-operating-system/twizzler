extern crate twizzler_runtime;

pub use logboi_impl::{foo, Bar};

#[link(name = "logboi_impl")]
extern "C" {}
