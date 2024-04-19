#[allow(unused_imports)]
use crate::upcall::{UpcallData, UpcallInfo};

/// Arch-specific frame info for upcall.
#[derive(Clone, Debug)]
#[repr(C)]
pub struct UpcallFrame {}

impl UpcallFrame {
    /// Get the instruction pointer of the frame.
    pub fn ip(&self) -> usize {
        todo!()
    }

    /// Get the stack pointer of the frame.
    pub fn sp(&self) -> usize {
        todo!()
    }

    /// Get the base pointer of the frame.
    pub fn bp(&self) -> usize {
        todo!()
    }

    /// Build a new frame set up to enter a context at a start point.
    pub fn new_entry_frame(
        sp: usize,
        tp: usize,
        ctx: crate::object::ObjID,
        entry: usize,
        arg: usize,
    ) -> Self {
        todo!()
    }
}

#[no_mangle]
#[cfg(feature = "runtime")]
pub(crate) unsafe extern "C" fn upcall_entry2(
    _frame: *mut UpcallFrame,
    _info: *const UpcallInfo,
) -> ! {
    todo!()
}

#[cfg(feature = "runtime")]
#[no_mangle]
pub(crate) unsafe extern "C-unwind" fn upcall_entry(
    _frame: *mut UpcallFrame,
    _info: *const UpcallData,
) -> ! {
    todo!()
}
