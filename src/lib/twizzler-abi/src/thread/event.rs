use crate::object::ObjID;

/// Basic structure of an async event sent to a thread queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(C)]
pub struct AsyncEvent {
    /// The sender thread's control ID, or 0 for kernel.
    pub sender: ObjID,
    /// Flags for this event.
    pub flags: AsyncEventFlags,
    /// API-specific message.
    pub message: u32,
    /// API-specific data.
    pub aux: [u64; MAX_AUX_DATA],
}

impl AsyncEvent {
    /// Construct a new AsyncEvent.
    pub fn new(
        sender: ObjID,
        flags: AsyncEventFlags,
        message: u32,
        aux: [u64; MAX_AUX_DATA],
    ) -> Self {
        Self {
            sender,
            flags,
            message,
            aux,
        }
    }
}

/// Maximum number of aux data slots.
pub const MAX_AUX_DATA: usize = 7;

bitflags::bitflags! {
    /// Async event flags.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct AsyncEventFlags : u32 {
        /// The sender did not (or does not) want to wait for the completion.
        const NON_BLOCKING = 1;
    }
}

/// The basic structure of an async event completion message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(C)]
pub struct AsyncEventCompletion {
    /// Flags about this completion. Reserved for future use.
    pub flags: AsyncEventCompletionFlags,
    /// API-specific status code.
    pub status: u32,
    /// API-specific data.
    pub aux: [u64; MAX_AUX_DATA],
}

impl AsyncEventCompletion {
    /// Construct a new AsyncEventCompletion.
    pub fn new(flags: AsyncEventCompletionFlags, status: u32, aux: [u64; MAX_AUX_DATA]) -> Self {
        Self { flags, status, aux }
    }
}

bitflags::bitflags! {
    /// Async event completion flags. Reserved for future use.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct AsyncEventCompletionFlags : u32 {
    }
}
