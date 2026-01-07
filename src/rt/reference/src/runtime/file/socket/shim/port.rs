use std::sync::Mutex;
pub struct PortAssignerInner {
    unused_start: u16,
    stack: Vec<u16>,
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
            stack: Vec::new(),
        }
    }

    pub fn return_port(&mut self, port: u16) {
        if self.unused_start == port + 1 {
            self.unused_start -= 1;
        } else {
            self.stack.push(port);
        }
    }

    pub fn get_ephemeral_port(&mut self) -> Option<u16> {
        self.stack.pop().or_else(|| {
            let next = self.unused_start;
            if next == EPHEMERAL_END {
                None
            } else {
                self.unused_start += 1;
                Some(next)
            }
        })
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
}
