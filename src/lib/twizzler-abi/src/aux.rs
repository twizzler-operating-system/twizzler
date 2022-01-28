//! When running a new program (and thus, initializing a new runtime), the new program expects to
//! receive some information about how it was started, including arguments, env vars, etc. These are
//! passed to the new program through the _start function as an array of AuxEntries as its only argument.
//!
//! This array of entries is an unspecified length and is terminated by the Null entry at the end of
//! the array.

use crate::object::ObjID;

#[repr(C)]
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
/// Auxillary information provided to a new program on runtime entry.
pub enum AuxEntry {
    /// Ends the aux array.
    Null,
    /// A pointer to this program's program headers, and the number of them. See the ELF
    /// specification for more info.
    ProgramHeaders(u64, usize),
    /// A pointer to the env var array.
    Environment(u64),
    /// A pointer to the arguments array.
    Arguments(u64),
    /// The object ID of the executable.
    ExecId(ObjID),
}
