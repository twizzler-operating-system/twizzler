use std::{collections::HashSet, sync::Mutex};
pub struct PortAssignerInner {
    unused_start: u16,
    unused: HashSet<u16>,
    used: HashSet<u16>,
}

pub struct PortAssigner {
    inner: Mutex<PortAssignerInner>,
}

const EPHEMERAL_START: u16 = 49152;
const EPHEMERAL_END: u16 = 65535;

impl PortAssignerInner {
    pub fn new() -> Self {
        Self {
            unused_start: EPHEMERAL_START,
            unused: HashSet::new(),
            used: HashSet::new(),
        }
    }

    pub fn return_port(&mut self, port: u16) {
        self.used.remove(&port);
        self.unused.insert(port);
    }

    pub fn allocate_port(&mut self, port: u16) -> bool {
        if self.used.contains(&port) {
            return false;
        }

        self.unused.remove(&port);
        self.used.insert(port);
        true
    }

    pub fn get_ephemeral_port(&mut self) -> Option<u16> {
        if self.unused.is_empty() {
            if self.unused_start == EPHEMERAL_END {
                return None;
            }
            let port = self.unused_start;
            self.unused_start += 1;
            if self.used.contains(&port) {
                return self.get_ephemeral_port();
            }
            self.used.insert(port);
            self.unused.remove(&port);
            Some(port)
        } else {
            let port = self.unused.iter().next().unwrap();
            self.used.insert(*port);
            Some(*port)
        }
    }
}

impl PortAssigner {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(PortAssignerInner::new()),
        }
    }

    pub fn return_port(&self, port: u16) {
        self.inner.lock().unwrap().return_port(port);
    }

    pub fn get_ephemeral_port(&self) -> Option<u16> {
        self.inner.lock().unwrap().get_ephemeral_port()
    }

    pub fn allocate_port(&self, port: u16) -> bool {
        self.inner.lock().unwrap().allocate_port(port)
    }
}
