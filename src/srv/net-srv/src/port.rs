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

    /// Allocate an ephemeral port, cycling upward through the range before reusing anything.
    ///
    /// The order matters, and getting it wrong is not a fairness nicety. Handing back a
    /// just-released port means the next connection reuses the previous one's four-tuple while
    /// that connection's socket may still be lingering (CloseWait/LastAck/TimeWait); smoltcp
    /// matches the incoming SYN against that socket rather than a listener, and the handshake
    /// never completes. This previously preferred the recycle pool, so every connection after the
    /// first release collided -- and the pool never removed what it handed out, so it kept
    /// returning the *same* port forever.
    pub fn get_ephemeral_port(&mut self) -> Option<u16> {
        while self.unused_start < EPHEMERAL_END {
            let port = self.unused_start;
            self.unused_start += 1;
            if self.used.contains(&port) {
                continue;
            }
            self.unused.remove(&port);
            self.used.insert(port);
            return Some(port);
        }
        // Range exhausted; now recycling is the only option.
        let port = *self.unused.iter().next()?;
        self.unused.remove(&port);
        self.used.insert(port);
        Some(port)
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
