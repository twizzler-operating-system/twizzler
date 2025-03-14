use std::{path::Path, sync::OnceLock};


use monitor_api::CompartmentHandle;
use secgate::{
    util::{Descriptor, Handle, SimpleBuffer},
    DynamicSecGate,
};
use twizzler_abi::{object::ObjID, pager};
use twizzler_rt_abi::object::MapFlags;

struct PagerAPI {
    _handle: &'static CompartmentHandle,
    full_sync_call: DynamicSecGate<'static, (ObjID,), ()>,
    open_handle: DynamicSecGate<'static, (), Option<(Descriptor, ObjID)>>,
    close_handle: DynamicSecGate<'static, (Descriptor,), ()>,
    enumerate_external:
        DynamicSecGate<'static, (Descriptor, usize), Result<usize, std::io::ErrorKind>>,
    stat_external: DynamicSecGate<'static, (Descriptor, usize), Result<(ObjID, bool), std::io::ErrorKind>>,
}

static PAGER_API: OnceLock<PagerAPI> = OnceLock::new();

fn pager_api() -> &'static PagerAPI {
    PAGER_API.get_or_init(|| {
        let handle = Box::leak(Box::new(
            CompartmentHandle::lookup("pager-srv").expect("failed to open pager compartment"),
        ));
        let full_sync_call = unsafe {
            handle
                .dynamic_gate::<(ObjID,), ()>("full_object_sync")
                .expect("failed to find full object sync gate call")
        };
        let open_handle = unsafe {
            handle
                .dynamic_gate("pager_open_handle")
                .expect("failed to find open handle gate call")
        };
        let close_handle = unsafe {
            handle
                .dynamic_gate("pager_close_handle")
                .expect("failed to find close handle gate call")
        };
        let enumerate_external = unsafe {
            handle
                .dynamic_gate("pager_enumerate_external")
                .expect("failed to find enumerate external gate call")
        };
        let stat_external = unsafe {
            handle
                .dynamic_gate("pager_stat_external")
                .expect("failed to find stat external gate call")
        };
        PagerAPI {
            _handle: handle,
            full_sync_call,
            open_handle,
            close_handle,
            stat_external,
            enumerate_external,
        }
    })
}

pub fn sync_object(id: ObjID) {
    (pager_api().full_sync_call)(id).unwrap()
}

pub struct PagerHandle {
    desc: Descriptor,
    buffer: SimpleBuffer,
}

impl Handle for PagerHandle {
    type OpenError = ();

    type OpenInfo = ();

    fn open(_info: Self::OpenInfo) -> Result<Self, Self::OpenError>
    where
        Self: Sized,
    {
        let (desc, id) = (pager_api().open_handle)().ok().flatten().ok_or(())?;
        let handle =
            twizzler_rt_abi::object::twz_rt_map_object(id, MapFlags::READ | MapFlags::WRITE)
                .map_err(|_| ())?;
        let sb = SimpleBuffer::new(handle);
        Ok(Self { desc, buffer: sb })
    }

    fn release(&mut self) {
        (pager_api().close_handle)(self.desc);
    }
}

// On drop, release the handle.
impl Drop for PagerHandle {
    fn drop(&mut self) {
        self.release()
    }
}

impl PagerHandle {
    /// Open a new logging handle.
    pub fn new() -> Option<Self> {
        Self::open(()).ok()
    }

    pub fn enumerate_external<P: AsRef<Path>>(&mut self, path: P) -> std::io::Result<Vec<String>> {
        let len = self
            .buffer
            .write(path.as_ref().as_os_str().as_encoded_bytes());
        let len = (pager_api().enumerate_external)(self.desc, len).unwrap()?;

        let mut off = 0;
        let mut v = Vec::new();
        while off < len {
            let mut buf = [0; MAX_EXTERNAL_PATH];
            let thislen = self.buffer.read_offset(&mut buf, off);
            let end = buf.iter().position(|x| *x == 0).unwrap_or(thislen - 1);
            let buf = &buf[0..(end + 1)];
            if let Ok(name) = String::from_utf8(buf.to_owned()) {
                v.push(name);
            }

            off += MAX_EXTERNAL_PATH;
        }
        Ok(v)
    }

    pub fn stat_external<P: AsRef<Path>>(&mut self, path: P) -> std::io::Result<(ObjID, bool)> {
        let len = self
            .buffer
            .write(path.as_ref().as_os_str().as_encoded_bytes());
        Ok((pager_api().stat_external)(self.desc, len).unwrap()?)
    }
}

pub const MAX_EXTERNAL_PATH: usize = 4096;
