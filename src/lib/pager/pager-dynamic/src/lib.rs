use std::sync::OnceLock;

use monitor_api::CompartmentHandle;
pub use object_store::paged_object_store::{
    ExternalFile, ExternalKind, MAX_EXTERNAL_PATH, NAME_MAX,
};
use secgate::{
    util::{Descriptor, Handle, SimpleBuffer},
    DynamicSecGate,
};
use twizzler_abi::object::ObjID;
use twizzler_rt_abi::object::MapFlags;

struct PagerAPI {
    _handle: &'static CompartmentHandle,
    full_sync_call: DynamicSecGate<'static, (ObjID,), ()>,
    open_handle: DynamicSecGate<'static, (), Option<(Descriptor, ObjID)>>,
    close_handle: DynamicSecGate<'static, (Descriptor,), ()>,
    enumerate_external: DynamicSecGate<'static, (Descriptor, ObjID), usize>,
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
        PagerAPI {
            _handle: handle,
            full_sync_call,
            open_handle,
            close_handle,
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
        let _ = (pager_api().close_handle)(self.desc);
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

    pub fn enumerate_external(&mut self, id: ObjID) -> std::io::Result<Vec<ExternalFile>> {
        let len = (pager_api().enumerate_external)(self.desc, id)
            .map_err(|e| std::io::Error::new(e.into(), "enumerate external"))?;

        let mut off = 0;
        let mut v = Vec::new();
        while off < len {
            let mut file = std::mem::MaybeUninit::<ExternalFile>::uninit();
            let ptr = file.as_mut_ptr().cast::<u8>();
            let slice = unsafe {
                core::slice::from_raw_parts_mut(ptr, std::mem::size_of::<ExternalFile>())
            };
            let thislen = self.buffer.read_offset(slice, off);

            if thislen < std::mem::size_of::<ExternalFile>() {
                break;
            }

            unsafe {
                v.push(file.assume_init());
            }

            off += thislen;
        }
        Ok(v)
    }
}
