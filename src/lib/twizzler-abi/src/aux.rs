//! When running a new program (and thus, initializing a new runtime), the new program expects to
//! receive some information about how it was started, including arguments, env vars, etc. These are
//! passed to the new program through the _start function as an array of AuxEntries as its only argument.
//!
//! This array of entries is an unspecified length and is terminated by the Null entry at the end of
//! the array.

use crate::object::ObjID;

/// Information about initrd object names.
#[derive(Copy, Clone)]
#[repr(C)]
pub struct KernelInitName {
    name: [u8; 256],
    id: ObjID,
    len: usize,
    res: u64,
}

impl KernelInitName {
    /// Constructor for a null name.
    pub const fn null() -> Self {
        Self {
            name: [0; 256],
            id: ObjID::new(0),
            len: 0,
            res: 0,
        }
    }

    /// New mapping from name to ID.
    pub fn new(name: &str, id: ObjID) -> Self {
        let mut new = Self {
            name: [0; 256],
            id,
            len: name.bytes().len(),
            res: 0,
        };
        for b in name.bytes().enumerate() {
            new.name[b.0] = b.1;
        }
        new
    }

    /// Get a name.
    pub fn name(&self) -> &str {
        unsafe { core::str::from_utf8_unchecked(&self.name[0..self.len]) }
    }

    /// Get an ID.
    pub fn id(&self) -> ObjID {
        self.id
    }
}

/// Kernel init info, including initrd names.
#[repr(C)]
pub struct KernelInitInfo {
    version: u32,
    flags: u32,
    boot_names_len: usize,
    boot_names: [KernelInitName; 256],
}

impl KernelInitInfo {
    /// Constructor.
    pub const fn new() -> Self {
        Self {
            version: 0,
            flags: 0,
            boot_names_len: 0,
            boot_names: [KernelInitName::null(); 256],
        }
    }

    /// Add a name to the name list.
    pub fn add_name(&mut self, name: KernelInitName) {
        self.boot_names[self.boot_names_len] = name;
        self.boot_names_len += 1;
    }

    /// Get the name list.
    pub fn names(&self) -> &[KernelInitName] {
        &self.boot_names[0..self.boot_names_len]
    }
}
