// assigns dynamic ports as needed
// dynamic ports range from 49152 to 65535 (size of stack = 16383)

/* what this file should do:
 * create a stack for the dynamic port numbers
 * support stack operations as follows:
 *    pop(): pop off the next useable port
 *    push(): push a port to be used next
 * create a wrapper for the stack in a mutex.
 * the function get_ephemeral_port() will pop off the stack
 */
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
        // log::debug!("return port {}", port);
        if self.unused_start == port + 1 {
            self.unused_start -= 1;
        } else {
            self.stack.push(port);
        }
    }

    pub fn get_ephemeral_port(&mut self) -> Option<u16> {
        self.stack
            .pop()
            .or_else(|| {
                let next = self.unused_start;
                if next == EPHEMERAL_END {
                    None
                } else {
                    self.unused_start += 1;
                    Some(next)
                }
            })
            // .inspect(|port| log::debug!("get port: {}", port))
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

// todo:
// implement backlog for accept(): have 8 waiting listening sockets before the indiana jones swap
// <-- this is for me look at daniel's changes in code <-- daniel moved swap into the blocking code
// test with 100 simultaneous connections!!

#[cfg(test)]
mod tests {
    use crate::port::PortAssigner;
    #[test]
    fn allocate_port() {
        let mut p = PortAssigner::new();
        if let Some(port) = p.get_ephemeral_port() {
            println!("{}", port);
            p.return_port(port);
        } else {
            println!("none");
        }
        assert_eq!(p.get_ephemeral_port(), Some(49152));
    }
    #[test]
    fn get_last_port() {
        let mut p = PortAssigner::new();
        let mut port = p.get_ephemeral_port();
        while port != None {
            port = p.get_ephemeral_port();
        }
        port = p.get_ephemeral_port(); // returns None
    }
}
