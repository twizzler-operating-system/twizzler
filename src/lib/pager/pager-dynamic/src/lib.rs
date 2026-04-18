use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};

use libc::mode_t;
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
    lookup_external: DynamicSecGate<'static, (Descriptor, ObjID), usize>,
    create_external: DynamicSecGate<'static, (Descriptor, ObjID, mode_t, usize), usize>,
    unlink_external: DynamicSecGate<'static, (Descriptor, ObjID, usize), ()>,
    readlink_external: DynamicSecGate<'static, (Descriptor, ObjID), usize>,
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
        let lookup_external = unsafe {
            handle
                .dynamic_gate("pager_lookup_external")
                .expect("failed to find lookup external gate call")
        };
        let create_external = unsafe {
            handle
                .dynamic_gate("pager_create_external")
                .expect("failed to find create external gate call")
        };
        let unlink_external = unsafe {
            handle
                .dynamic_gate("pager_unlink_external")
                .expect("failed to find unlink external gate call")
        };
        let readlink_external = unsafe {
            handle
                .dynamic_gate("pager_readlink_external")
                .expect("failed to find unlink external gate call")
        };
        PagerAPI {
            _handle: handle,
            open_handle,
            close_handle,
            enumerate_external,
            lookup_external,
            create_external,
            unlink_external,
            readlink_external,
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

fn get_external_file_from_sb(sb: &SimpleBuffer, offset: usize) -> Option<(ExternalFile, usize)> {
    let mut file = std::mem::MaybeUninit::<ExternalFileSbHdr>::uninit();
    let ptr = file.as_mut_ptr().cast::<u8>();
    let slice =
        unsafe { core::slice::from_raw_parts_mut(ptr, std::mem::size_of::<ExternalFileSbHdr>()) };
    let thislen = sb.read_offset(slice, offset);

    if thislen < std::mem::size_of::<ExternalFileSbHdr>() {
        return None;
    }

    let file = unsafe { file.assume_init() };

    let mut pathbuf = [0u8; MAX_EXTERNAL_PATH];
    let pathlen = sb.read_offset(&mut pathbuf[0..(file.pathlen as usize)], offset + thislen);

    if pathlen < file.pathlen as usize {
        return None;
    }

    Some((
        ExternalFile::new(
            unsafe { str::from_utf8_unchecked(&pathbuf[0..pathlen]) },
            file.kind,
            file.id,
        ),
        thislen + pathlen,
    ))
}

impl PagerHandle {
    /// Open a new logging handle.
    pub fn new() -> Option<Self> {
        Self::open(()).ok()
    }

    pub fn readlink_external(&mut self, id: ObjID) -> Result<String> {
        let len = (pager_api().readlink_external)(self.desc, id)?;
        let mut v = vec![0; len];
        self.buffer.read(&mut v);
        String::from_utf8(v).map_err(|_| TwzError::INVALID_ARGUMENT)
    }

    pub fn unlink_external(&mut self, id: ObjID, name: impl AsRef<Path>) -> Result<()> {
        let name = name.as_ref().as_os_str().as_encoded_bytes();
        if name.len() > NAME_MAX {
            return Err(TwzError::INVALID_ARGUMENT);
        }
        let namelen = self.buffer.write(name);

        (pager_api().unlink_external)(self.desc, id, namelen)
    }

    pub fn create_external_file(
        &mut self,
        dir: ObjID,
        name: impl AsRef<Path>,
        mode: mode_t,
    ) -> Result<ExternalFile> {
        let name = name.as_ref().as_os_str().as_encoded_bytes();
        if name.len() > NAME_MAX {
            return Err(TwzError::INVALID_ARGUMENT);
        }
        let namelen = self.buffer.write(name);

        let _filelen = (pager_api().create_external)(self.desc, dir, mode, namelen)?;

        get_external_file_from_sb(&self.buffer, 0)
            .ok_or(TwzError::INVALID_ARGUMENT)
            .map(|x| x.0)
    }

    pub fn enumerate_external(&mut self, id: ObjID, entries: &mut Vec<ExternalFile>) -> Result<()> {
        let len = (pager_api().enumerate_external)(self.desc, id)?;

        let mut off = 0;
        entries.clear();
        while off < len {
            let Some(file) = get_external_file_from_sb(&self.buffer, off) else {
                break;
            };
            entries.push(file.0);

            off += file.1;
        }
        Ok(())
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

#[derive(Clone, Debug, PartialEq, PartialOrd, Ord, Eq, Hash)]
pub struct ExternalFile {
    pub id: u128,
    pub path: PathBuf,
    pub kind: ExternalKind,
}

impl ExternalFile {
    pub fn new(path: impl AsRef<std::path::Path>, kind: ExternalKind, id: u128) -> Self {
        Self {
            id,
            path: path.as_ref().to_path_buf(),
            kind,
        }
    }

    pub fn name(&self) -> Option<&str> {
        self.path.file_name().and_then(|s| s.to_str())
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

pub struct ExternalFileSbHdr {
    pub id: u128,
    pub kind: ExternalKind,
    pub pathlen: u32,
}
