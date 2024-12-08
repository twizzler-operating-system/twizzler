pub use naming_srv::{put, get, reload, NamespaceHandle};

#[link(name = "naming_srv")]
extern "C" {}
