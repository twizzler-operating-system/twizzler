//! Per-object page census: which object grew, and by how much.
//!
//! Counters say how much leaked; this says what holds it. It is the difference between "21
//! page_data frames per thread spawn" and a named object with a provenance note, which is the
//! difference between a number and a fix.
//!
//! Rising `page_data` against a flat object count means pages accruing to objects that already
//! exist, so a census that only diffed the *set* of live ids would report nothing. This diffs each
//! object's page count as well.

use std::collections::HashMap;

use twizzler_abi::{
    object::ObjID,
    syscall::{EnumerateKind, sys_enumerate, sys_object_stat},
};

pub struct Census {
    pub pages: HashMap<ObjID, usize>,
    /// Objects the enumeration listed but `sys_object_stat` refused. Reported rather than dropped:
    /// an object that cannot be statted is not an object with zero pages.
    pub unstattable: usize,
}

pub fn take() -> Census {
    let mut pages = HashMap::new();
    let mut unstattable = 0;
    let mut buf = [ObjID::new(0); 256];
    let mut off = 0usize;

    loop {
        let n = match sys_enumerate(EnumerateKind::Objects, &mut buf, off) {
            Ok(n) => n,
            Err(_) => break,
        };
        for id in buf.iter().take(n) {
            match sys_object_stat(*id) {
                Ok(info) => {
                    pages.insert(*id, info.pages);
                }
                Err(_) => unstattable += 1,
            }
        }
        if n < buf.len() {
            break;
        }
        off += n;
    }

    Census { pages, unstattable }
}

pub struct Delta {
    pub id: ObjID,
    pub before: usize,
    pub after: usize,
    pub is_new: bool,
}

impl Delta {
    pub fn growth(&self) -> i64 {
        self.after as i64 - self.before as i64
    }
}

/// Objects that gained pages or appeared, biggest gain first -- the "biggest offenders" ordering.
pub fn diff(before: &Census, after: &Census) -> Vec<Delta> {
    let mut out: Vec<Delta> = after
        .pages
        .iter()
        .filter_map(|(id, &a)| {
            let b = before.pages.get(id).copied();
            let d = Delta {
                id: *id,
                before: b.unwrap_or(0),
                after: a,
                is_new: b.is_none(),
            };
            (d.growth() > 0).then_some(d)
        })
        .collect();
    out.sort_by_key(|d| -d.growth());
    out
}

/// A note's bytes, if the object carries any. Notes are how provenance is already tagged in this
/// tree (`b"heap"`, `b"monitor-heap"`, and whatever `ObjectBuilder` was given).
pub fn note(id: ObjID) -> Option<String> {
    let mut keys = [0u64; 8];
    let n = twizzler_abi::syscall::sys_object_enumerate_notes(id, 0, &mut keys).ok()?;
    let mut buf = [0u8; 64];
    // All notes, not the first. An object can carry several -- the monitor's creation-site note
    // and its own `stack:<comp>`/`comp-config:<comp>` tag -- and returning the first silently
    // shadowed the more informative one.
    let mut found: Vec<String> = Vec::new();
    for key in keys.iter().take(n) {
        if let Ok(len) = twizzler_abi::syscall::sys_object_get_note(id, *key, &mut buf) {
            let len = len.min(buf.len());
            if len > 0 {
                found.push(String::from_utf8_lossy(&buf[..len]).into_owned());
            }
        }
    }
    (!found.is_empty()).then(|| found.join("|"))
}
