//! Some runtime-available executable information. Mostly used for backtracing.

use crate::object::ObjID;

// TODO: this is all a hack
/// Returns the object ID of the running executable.
pub fn get_current_exe_id() -> Option<ObjID> {
    unsafe { crate::rt1::get_exec_id() }
}

/// A particular segment of the loaded executable, corresponding to where the program headers were
/// loaded into memory.
pub struct Segment {
    pub vaddr: usize,
    pub len: usize,
}

/// Return a given segment for a given loaded executable or library by index, or None of segnr is too large (or if this information is not available).
pub fn get_segment(id: ObjID, segnr: usize) -> Option<Segment> {
    if let Some(eid) = get_current_exe_id() {
        if eid != id {
            return None;
        }
        crate::rt1::get_load_seg(segnr).map(|(v, l)| Segment { vaddr: v, len: l })
    } else {
        None
    }
}
