use crate::object::ObjID;

pub struct AsyncEvent {
    pub sender: ObjID,
    pub flags: AsyncEventFlags,
    pub message: u32,
    pub aux: [u64; MAX_AUX_DATA],
}

impl AsyncEvent {
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

const MAX_AUX_DATA: usize = 7;

bitflags::bitflags! {
    pub struct AsyncEventFlags : u32 {
        const NON_BLOCKING = 1;
    }
}

pub struct AsyncEventCompletion {
    pub flags: AsyncEventCompletionFlags,
    pub status: u32,
    pub aux: [u64; MAX_AUX_DATA],
}

impl AsyncEventCompletion {
    pub fn new(flags: AsyncEventCompletionFlags, status: u32, aux: [u64; MAX_AUX_DATA]) -> Self {
        Self { flags, status, aux }
    }
}

bitflags::bitflags! {
    pub struct AsyncEventCompletionFlags : u32 {
    }
}
