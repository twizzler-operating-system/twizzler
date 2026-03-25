use std::sync::OnceLock;

use monitor_api::CompartmentHandle;
use secgate::{
    util::{Descriptor, Handle, SimpleBuffer},
    DynamicSecGate,
};
use twizzler_abi::object::ObjID;
use twizzler_rt_abi::{error::TwzError, object::MapFlags, Result};

struct PagerAPI {
    _handle: &'static CompartmentHandle,
    open_handle: DynamicSecGate<'static, (), (Descriptor, ObjID)>,
    close_handle: DynamicSecGate<'static, (Descriptor,), ()>,
    enumerate_external: DynamicSecGate<'static, (Descriptor, ObjID), usize>,
}

static PAGER_API: OnceLock<PagerAPI> = OnceLock::new();

fn pager_api() -> &'static PagerAPI {
    PAGER_API.get_or_init(|| {
        let handle = Box::leak(Box::new(
            CompartmentHandle::lookup("pager-srv").expect("failed to open pager compartment"),
        ));
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
            open_handle,
            close_handle,
            enumerate_external,
        }
    })
}

pub struct PagerHandle {
    desc: Descriptor,
    buffer: SimpleBuffer,
}

impl Handle for PagerHandle {
    type OpenError = TwzError;

    type OpenInfo = ();

    fn open(_info: Self::OpenInfo) -> Result<Self>
    where
        Self: Sized,
    {
        let (desc, id) = (pager_api().open_handle)()?;
        let handle =
            twizzler_rt_abi::object::twz_rt_map_object(id, MapFlags::READ | MapFlags::WRITE)?;
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

    pub fn enumerate_external(&mut self, id: ObjID) -> Result<Vec<ExternalFile>> {
        let len = (pager_api().enumerate_external)(self.desc, id)?;

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

pub fn objid_to_ino(id: u128) -> Option<u32> {
    if id == 1 {
        return Some(0);
    };
    let (hi, lo) = ((id >> 64) as u64, id as u64);
    if hi == (1u64 << 63) {
        let ino = lo & !(1u64 << 63);
        Some(ino as u32)
    } else {
        None
    }
}

pub fn ino_to_objid(ino: u32) -> u128 {
    if ino == 0 {
        return 1;
    }
    (1u128 << 127) | (ino as u128) | (1u128 << 63)
}

pub const MAX_EXTERNAL_PATH: usize = 4096;
pub const NAME_MAX: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ord, Eq, Hash)]
#[repr(C)]
pub struct ExternalFile {
    pub id: u128,
    pub name: [u8; NAME_MAX],
    pub name_len: u32,
    pub kind: ExternalKind,
}

impl ExternalFile {
    pub fn new(iname: &[u8], kind: ExternalKind, id: u128) -> Self {
        let name_len = iname.len().min(NAME_MAX);
        let sname = &iname[0..name_len];
        let mut name = [0; NAME_MAX];
        name[0..name_len].copy_from_slice(&sname);
        Self {
            id,
            name,
            kind,
            name_len: name_len as u32,
        }
    }

    pub fn name(&self) -> Option<&str> {
        str::from_utf8(&self.name[0..(self.name_len as usize)]).ok()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ord, Eq, Hash)]
#[repr(u32)]
pub enum ExternalKind {
    Regular,
    Directory,
    SymLink,
    Other,
}
