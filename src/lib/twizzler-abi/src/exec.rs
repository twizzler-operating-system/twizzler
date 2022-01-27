use crate::object::ObjID;

// TODO: this is all a hack
pub fn get_current_exe_id() -> Option<ObjID> {
    unsafe { crate::rt1::get_exec_id() }
}

pub struct Segment {
    pub vaddr: usize,
    pub len: usize,
}

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
