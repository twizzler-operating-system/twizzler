use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use twizzler::object::ObjID;

use crate::helpers::PAGE;

#[derive(Clone, Debug, Default, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PerObjectStats {
    pub pages_read: usize,
    pub pages_written: usize,
    pub pages_allocated: usize,
    pub read_errors: usize,
    pub write_errors: usize,
    pub bytes_read: usize,
    pub bytes_written: usize,
}


#[derive(Clone, Debug)]
pub struct RecentStats {
    map: HashMap<ObjID, PerObjectStats>,
    point: Instant,
}

#[allow(unused)]
impl RecentStats {
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
            point: Instant::now(),
        }
    }

    pub fn reset(&mut self) {
        self.map.clear();
        self.point = Instant::now();
    }

    pub fn write_pages(&mut self, id: ObjID, count: usize) {
        let entry = self.map.entry(id).or_default();
        entry.pages_written += count;
    }

    pub fn read_pages(&mut self, id: ObjID, count: usize) {
        let entry = self.map.entry(id).or_default();
        entry.pages_read += count;
    }

    pub fn pages_read(&self, id: ObjID) -> Option<usize> {
        self.map.get(&id).map(|stats| stats.pages_read)
    }

    pub fn pages_written(&self, id: ObjID) -> Option<usize> {
        self.map.get(&id).map(|stats| stats.pages_written)
    }

    pub fn dt(&self) -> Duration {
        self.point.elapsed()
    }

    pub fn recorded_ids(&self) -> impl Iterator<Item = ObjID> + use<'_> {
        self.map.keys().cloned()
    }

    pub fn recorded_stats(&self) -> impl Iterator<Item = (&ObjID, &PerObjectStats)> {
        self.map.iter()
    }

    pub fn had_activity(&self) -> bool {
        !self.map.is_empty()
    }

    pub fn alloc_pages(&mut self, id: ObjID, count: usize) {
        let entry = self.map.entry(id).or_default();
        entry.pages_allocated += count;
    }

    pub fn record_error(&mut self, id: ObjID, is_read: bool) {
        let entry = self.map.entry(id).or_default();
        if is_read {
            entry.read_errors += 1;
        } else {
            entry.write_errors += 1;
        }
    }

    pub fn record_bytes(&mut self, id: ObjID, read_bytes: usize, write_bytes: usize) {
        let entry = self.map.entry(id).or_default();
        entry.bytes_read += read_bytes;
        entry.bytes_written += write_bytes;
    }

}

pub fn pages_to_kbytes_per_sec(count: usize, dt: Duration) -> f32 {
    let bytes = count * PAGE as usize / 1024;
    let dt = dt.div_duration_f32(Duration::from_secs(1));
    bytes as f32 * dt
}
