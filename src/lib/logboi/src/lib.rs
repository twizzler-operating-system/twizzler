use secgate::util::{Descriptor, Handle, SimpleBuffer};
use twizzler_runtime_api::{get_runtime, MapFlags};

/// An open handle to the logging service.
pub struct LogHandle {
    desc: Descriptor,
    buffer: SimpleBuffer,
}

// A service typically implements these handles via this interface.
// You can see that internally, this is where most of the secure gate APIs
// are actually used, so that interface is abstracted from the programmer.
impl Handle for LogHandle {
    type OpenError = ();

    type OpenInfo = ();

    fn open(_info: Self::OpenInfo) -> Result<Self, Self::OpenError>
    where
        Self: Sized,
    {
        let (desc, id) = logboi_srv::logboi_open_handle().ok().flatten().ok_or(())?;
        let handle = get_runtime()
            .map_object(id, MapFlags::READ | MapFlags::WRITE)
            .map_err(|_| ())?;
        let sb = SimpleBuffer::new(handle);
        Ok(Self { desc, buffer: sb })
    }

    fn release(&mut self) {
        logboi_srv::logboi_close_handle(self.desc);
    }
}

// On drop, release the handle.
impl Drop for LogHandle {
    fn drop(&mut self) {
        self.release()
    }
}

impl LogHandle {
    /// Open a new logging handle.
    pub fn new() -> Option<Self> {
        Self::open(()).ok()
    }

    /// Send a log message via this logging handle.
    pub fn log(&mut self, buf: &[u8]) -> Option<usize> {
        let len = self.buffer.write(buf);
        if len == 0 {
            return Some(0);
        }

        if logboi_srv::logboi_post(self.desc, len).ok().is_some() {
            Some(len)
        } else {
            None
        }
    }
}
