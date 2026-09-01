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

/// Gate addresses resolved on first use, same reasoning as `naming_core::dynamic`: building all
/// seven eagerly cost a gate call each per compartment, and most compartments never touch the
/// external-file API at all.
macro_rules! lazy_gates {
    ($($field:ident : $ty:ty = $name:literal),* $(,)?) => {
        struct PagerAPI {
            handle: &'static CompartmentHandle,
            $( $field: OnceLock<$ty>, )*
        }
        impl PagerAPI {
            $(
                fn $field(&self) -> &$ty {
                    self.$field.get_or_init(|| unsafe {
                        self.handle
                            .dynamic_gate($name)
                            .expect(concat!("failed to find ", $name, " gate call"))
                    })
                }
            )*
        }
        static PAGER_API: OnceLock<PagerAPI> = OnceLock::new();

        fn pager_api() -> &'static PagerAPI {
            PAGER_API.get_or_init(|| {
                let handle = Box::leak(Box::new(
                    CompartmentHandle::lookup("pager-srv")
                        .expect("failed to open pager compartment"),
                ));
                PagerAPI { handle, $( $field: OnceLock::new(), )* }
            })
        }
    };
}

lazy_gates! {
    open_handle: DynamicSecGate<'static, (), (Descriptor, ObjID)> = "pager_open_handle",
    close_handle: DynamicSecGate<'static, (Descriptor,), ()> = "pager_close_handle",
    enumerate_external: DynamicSecGate<'static, (Descriptor, ObjID, usize, usize), usize>
        = "pager_enumerate_external",
    lookup_external: DynamicSecGate<'static, (Descriptor, ObjID, usize), usize>
        = "pager_lookup_external",
    create_external:
        DynamicSecGate<'static, (Descriptor, ObjID, mode_t, usize, Option<ObjID>), usize>
        = "pager_create_external",
    unlink_external: DynamicSecGate<'static, (Descriptor, ObjID, usize), ()>
        = "pager_unlink_external",
    set_mtime_external: DynamicSecGate<'static, (ObjID, u64), ()> = "pager_set_mtime_external",
    nlink_external: DynamicSecGate<'static, (ObjID,), u32> = "pager_nlink_external",
    readlink_external: DynamicSecGate<'static, (Descriptor, ObjID), usize>
        = "pager_readlink_external",
}

pub struct PagerHandle {
    desc: Descriptor,
    buffer: SimpleBuffer,
}

// Temporary instrumentation for the File::open latency hunt (pagerperf.md).
mod handlestats {
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNT: AtomicU64 = AtomicU64::new(0);
    static GATE: AtomicU64 = AtomicU64::new(0);
    static MAP: AtomicU64 = AtomicU64::new(0);

    /// `TWZ_DIAG` contains `pager` (comma list, or `all`). Runs in whichever client compartment
    /// opened the handle; those all inherit init's environment. Same contract as
    /// `twizzler_net::diag_enabled`, which this crate does not depend on.
    fn diag_enabled() -> bool {
        static SET: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        let set = SET.get_or_init(|| std::env::var("TWZ_DIAG").unwrap_or_default());
        set.split(',').any(|c| c == "pager" || c == "all")
    }

    pub fn record(gate: u64, map: u64) {
        let n = COUNT.fetch_add(1, Ordering::Relaxed) + 1;
        let g = GATE.fetch_add(gate, Ordering::Relaxed) + gate;
        let m = MAP.fetch_add(map, Ordering::Relaxed) + map;
        if n.is_power_of_two() && diag_enabled() {
            twizzler_abi::klog_println!(
                "PHSTATS {} pager handles: open-gate {} us, map {} us (per handle: {} us)",
                n,
                g / 1000,
                m / 1000,
                (g + m) / (n * 1000),
            );
        }
    }
}

impl Handle for PagerHandle {
    type OpenError = TwzError;

    type OpenInfo = ();

    fn open(_info: Self::OpenInfo) -> Result<Self>
    where
        Self: Sized,
    {
        let t_gate = std::time::Instant::now();
        let (desc, id) = (pager_api().open_handle())()?;
        let gate_ns = t_gate.elapsed().as_nanos() as u64;
        let t_map = std::time::Instant::now();
        let handle =
            twizzler_rt_abi::object::twz_rt_map_object(id, MapFlags::READ | MapFlags::WRITE)?;
        handlestats::record(gate_ns, t_map.elapsed().as_nanos() as u64);
        let sb = SimpleBuffer::new(handle);
        Ok(Self { desc, buffer: sb })
    }

    fn release(&mut self) {
        let _ = (pager_api().close_handle())(self.desc);
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
        let len = (pager_api().readlink_external())(self.desc, id)?;
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

        (pager_api().unlink_external())(self.desc, id, namelen)
    }

    pub fn create_external_file(
        &mut self,
        dir: ObjID,
        name: impl AsRef<Path>,
        link_to: Option<ObjID>,
        mode: mode_t,
    ) -> Result<ExternalFile> {
        let name = name.as_ref().as_os_str().as_encoded_bytes();
        if name.len() > NAME_MAX {
            return Err(TwzError::INVALID_ARGUMENT);
        }
        let namelen = self.buffer.write(name);

        let _filelen = (pager_api().create_external())(self.desc, dir, mode, namelen, link_to)?;

        get_external_file_from_sb(&self.buffer, 0)
            .ok_or(TwzError::INVALID_ARGUMENT)
            .map(|x| x.0)
    }

    pub fn lookup_external(&mut self, dir: ObjID, name: impl AsRef<Path>) -> Result<ExternalFile> {
        let name = name.as_ref().as_os_str().as_encoded_bytes();
        if name.len() > NAME_MAX {
            return Err(TwzError::INVALID_ARGUMENT);
        }
        let namelen = self.buffer.write(name);

        let _filelen = (pager_api().lookup_external())(self.desc, dir, namelen)?;

        get_external_file_from_sb(&self.buffer, 0)
            .ok_or(TwzError::INVALID_ARGUMENT)
            .map(|x| x.0)
    }

    pub fn enumerate_external(
        &mut self,
        id: ObjID,
        entries: &mut Vec<ExternalFile>,
        skip: usize,
        count: usize,
    ) -> Result<()> {
        let len = (pager_api().enumerate_external())(self.desc, id, skip, count)?;

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

/// Record `mtime` (seconds) on the store inode backing external object `id`. Needs no handle:
/// the call is inode-addressed and moves no buffer data.
pub fn set_mtime_external(id: ObjID, mtime: u64) -> Result<()> {
    (pager_api().set_mtime_external())(id, mtime)
}

/// The store's link count for external object `id`. Handle-free for the same reason as
/// [set_mtime_external]: inode-addressed, and it moves no buffer data.
pub fn nlink_external(id: ObjID) -> Result<u32> {
    (pager_api().nlink_external())(id)
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
        self.path.to_str()
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
