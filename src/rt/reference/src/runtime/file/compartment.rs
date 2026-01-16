use std::{
    io::{ErrorKind, Read, Write},
    sync::{Arc, Mutex},
};

use monitor_api::{CompartmentFlags, CompartmentHandle};

#[derive(Clone)]
pub struct CompartmentFile {
    inner: Arc<CompartmentFileInner>,
}

struct CompartmentFileInner {
    comp: CompartmentHandle,
    last_state: Mutex<CompartmentFlags>,
}

impl CompartmentFile {
    pub fn new(comp: CompartmentHandle) -> Self {
        let last_state = comp.info().flags;
        Self {
            inner: Arc::new(CompartmentFileInner {
                comp,
                last_state: Mutex::new(last_state),
            }),
        }
    }
}

impl Read for CompartmentFile {
    fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
        let mut current_state = self.inner.comp.info().flags;
        loop {
            let mut ls = self.inner.last_state.lock().unwrap();
            if current_state != *ls {
                *ls = current_state;
                return Ok(0);
            } else {
                drop(ls);
                current_state = self.inner.comp.wait(current_state);
            }
        }
    }
}

impl Write for CompartmentFile {
    fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
        return Err(ErrorKind::Unsupported.into());
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
