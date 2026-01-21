use std::{
    io::{ErrorKind, Read, Write},
    os::raw::c_void,
    sync::{Arc, Mutex},
};

use monitor_api::{CompartmentFlags, CompartmentHandle};
use twizzler_rt_abi::{
    bindings::{IO_REGISTER_SIGNAL, IO_REGISTER_STATUS, STATUS_FLAG_READY, STATUS_FLAG_TERMINATED},
    error::TwzError,
};

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

    pub fn get_config(
        &mut self,
        reg: u32,
        val: *mut c_void,
        val_len: usize,
    ) -> Result<(), TwzError> {
        match reg {
            IO_REGISTER_STATUS => {
                if val_len < std::mem::size_of::<u64>() {
                    Err(TwzError::INVALID_ARGUMENT)
                } else {
                    let flags = self.inner.comp.info().flags;
                    let mut status = if flags.contains(CompartmentFlags::EXITED) {
                        STATUS_FLAG_TERMINATED
                    } else {
                        0 // TODO
                    };
                    if flags.contains(CompartmentFlags::READY) {
                        status |= STATUS_FLAG_READY;
                    };
                    unsafe {
                        *(val as *mut u64) = status;
                    }
                    Ok(())
                }
            }
            _ => Err(TwzError::INVALID_ARGUMENT),
        }
    }

    pub fn set_config(
        &mut self,
        reg: u32,
        val: *const c_void,
        val_len: usize,
    ) -> Result<(), TwzError> {
        match reg {
            IO_REGISTER_SIGNAL => {
                if val_len < std::mem::size_of::<u64>() {
                    Err(TwzError::INVALID_ARGUMENT)
                } else {
                    self.inner.comp.signal(unsafe { *val.cast() })?;
                    Ok(())
                }
            }
            _ => Err(TwzError::INVALID_ARGUMENT),
        }
    }
}

impl Read for CompartmentFile {
    fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
        let mut current_state = self.inner.comp.info().flags;
        if current_state.contains(CompartmentFlags::EXITED) {
            return Ok(0);
        }
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
