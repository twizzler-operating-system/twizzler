use std::{
    io::{ErrorKind, Read, SeekFrom, Write},
    sync::{Arc, Mutex, OnceLock},
};

use bitflags::bitflags;
use file_desc::FileDesc;
use lazy_static::lazy_static;
use naming_core::dynamic::{dynamic_naming_factory, DynamicNamingHandle};
use raw_file::RawFile;
use stable_vec::{self, StableVec};
use twizzler_abi::{
    object::{ObjID, MAX_SIZE, NULLPAGE_SIZE},
    syscall::{sys_object_create, BackingType, LifetimeType, ObjectCreate, ObjectCreateFlags},
};
use twizzler_rt_abi::{
    bindings::{create_options, io_vec},
    fd::{FdInfo, RawFd},
    io::IoFlags,
    object::MapFlags,
};

use super::ReferenceRuntime;

mod file_desc;
mod raw_file;

#[derive(Clone)]
enum FdKind {
    File(Arc<Mutex<FileDesc>>),
    RawFile(Arc<Mutex<RawFile>>),
    Stdio,
}

impl FdKind {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<usize> {
        match self {
            FdKind::File(arc) => arc.lock().unwrap().seek(pos),
            FdKind::RawFile(arc) => arc.lock().unwrap().seek(pos),
            FdKind::Stdio => Err(std::io::ErrorKind::Other.into()),
        }
    }

    fn stat(&mut self) -> std::io::Result<FdInfo> {
        match self {
            FdKind::File(arc) => arc.lock().unwrap().stat(),
            FdKind::RawFile(arc) => arc.lock().unwrap().stat(),
            FdKind::Stdio => Ok(FdInfo {
                flags: twizzler_rt_abi::fd::FdFlags::from_bits_truncate(0),
                kind: twizzler_rt_abi::fd::FdKind::Other,
                size: 0,
                id: 0,
            }),
        }
    }

    pub fn fd_cmd(&self, cmd: u32, arg: *const u8, ret: *mut u8) -> u32 {
        match self {
            FdKind::File(arc) => arc.lock().unwrap().fd_cmd(cmd, arg, ret),
            _ => 1,
        }
    }
}

impl Read for FdKind {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            FdKind::File(arc) => arc.lock().unwrap().read(buf),
            FdKind::RawFile(arc) => arc.lock().unwrap().read(buf),
            FdKind::Stdio => {
                let len = twizzler_abi::syscall::sys_kernel_console_read(
                    buf,
                    twizzler_abi::syscall::KernelConsoleReadFlags::empty(),
                )
                .map_err(|_| ErrorKind::Other)?;
                Ok(len)
            }
        }
    }
}

impl Write for FdKind {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            FdKind::File(arc) => arc.lock().unwrap().write(buf),
            FdKind::RawFile(arc) => arc.lock().unwrap().write(buf),
            FdKind::Stdio => {
                twizzler_abi::syscall::sys_kernel_console_write(
                    buf,
                    twizzler_abi::syscall::KernelConsoleWriteFlags::empty(),
                );
                Ok(buf.len())
            }
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            FdKind::File(arc) => arc.lock().unwrap().flush(),
            FdKind::RawFile(arc) => arc.lock().unwrap().flush(),
            FdKind::Stdio => Ok(()),
        }
    }
}

lazy_static! {
    static ref FD_SLOTS: Mutex<StableVec<FdKind>> = Mutex::new(StableVec::from([
        FdKind::Stdio,
        FdKind::Stdio,
        FdKind::Stdio
    ]));
}
static HANDLE: OnceLock<Mutex<DynamicNamingHandle>> = OnceLock::new();

fn get_fd_slots() -> &'static Mutex<StableVec<FdKind>> {
    &FD_SLOTS
}

fn get_naming_handle() -> &'static Mutex<DynamicNamingHandle> {
    HANDLE.get_or_init(|| Mutex::new(dynamic_naming_factory().unwrap()))
}

#[derive(Debug)]
pub enum CreateOptions {
    UNEXPECTED,
    CreateKindExisting,
    CreateKindNew,
    CreateKindEither,
}

impl From<create_options> for CreateOptions {
    fn from(value: create_options) -> Self {
        match value.kind {
            twizzler_rt_abi::bindings::CREATE_KIND_EITHER => CreateOptions::CreateKindEither,
            twizzler_rt_abi::bindings::CREATE_KIND_NEW => CreateOptions::CreateKindNew,
            twizzler_rt_abi::bindings::CREATE_KIND_EXISTING => CreateOptions::CreateKindExisting,
            _ => CreateOptions::UNEXPECTED,
        }
    }
}

bitflags! {
    #[derive(Debug)]
    pub struct OperationOptions: u32 {
        const OPEN_FLAG_READ = twizzler_rt_abi::bindings::OPEN_FLAG_READ;
        const OPEN_FLAG_WRITE = twizzler_rt_abi::bindings::OPEN_FLAG_WRITE;
        const OPEN_FLAG_TRUNCATE = twizzler_rt_abi::bindings::OPEN_FLAG_TRUNCATE;
        const OPEN_FLAG_TAIL = twizzler_rt_abi::bindings::OPEN_FLAG_TAIL;
    }
}

impl From<u32> for OperationOptions {
    fn from(value: u32) -> Self {
        OperationOptions::from_bits_truncate(value)
    }
}

impl ReferenceRuntime {
    pub fn open(
        &self,
        path: &str,
        create_opt: CreateOptions,
        open_opt: OperationOptions,
    ) -> std::io::Result<RawFd> {
        let mut session = get_naming_handle().lock().unwrap();

        if open_opt.contains(OperationOptions::OPEN_FLAG_TRUNCATE)
            && !open_opt.contains(OperationOptions::OPEN_FLAG_WRITE)
        {
            return Err(ErrorKind::InvalidInput.into());
        }
        let create = ObjectCreate::new(
            BackingType::Normal,
            LifetimeType::Persistent,
            None,
            ObjectCreateFlags::empty(),
        );
        let flags = match (
            open_opt.contains(OperationOptions::OPEN_FLAG_READ),
            open_opt.contains(OperationOptions::OPEN_FLAG_WRITE),
        ) {
            (true, true) => MapFlags::READ | MapFlags::WRITE,
            (true, false) => MapFlags::READ,
            (false, true) => MapFlags::WRITE,
            (false, false) => MapFlags::READ,
        };
        let (obj_id, did_create) = match create_opt {
            CreateOptions::UNEXPECTED => return Err(ErrorKind::InvalidInput.into()),
            CreateOptions::CreateKindExisting => (
                session.get(path).map_err(|_| ErrorKind::Other)?.into(),
                false,
            ),
            CreateOptions::CreateKindNew => {
                if session.get(path).is_ok() {
                    return Err(ErrorKind::InvalidInput.into());
                }
                (
                    sys_object_create(create, &[], &[]).map_err(|_| ErrorKind::Other)?,
                    true,
                )
            }
            CreateOptions::CreateKindEither => session
                .get(path)
                .map(|x| (ObjID::from(x), false))
                .unwrap_or((
                    sys_object_create(create, &[], &[]).map_err(|_| ErrorKind::Other)?,
                    true,
                )),
        };

        let elem = if let Ok(elem) = FileDesc::open(&open_opt, obj_id, flags, &create_opt) {
            FdKind::File(Arc::new(Mutex::new(elem)))
        } else {
            FdKind::RawFile(Arc::new(Mutex::new(RawFile::open(obj_id, flags)?)))
        };

        let mut binding = get_fd_slots().lock().unwrap();

        let fd = if binding.is_compact() {
            binding.push(elem)
        } else {
            let fd = binding.first_empty_slot_from(0).unwrap();
            binding.insert(fd, elem);
            fd
        };

        if did_create {
            session.put(path, obj_id)?;
        }

        drop(binding);
        if open_opt.contains(OperationOptions::OPEN_FLAG_TAIL) {
            self.seek(fd.try_into().unwrap(), SeekFrom::End(0))
                .map_err(|_| ErrorKind::Other)?;
        }
        Ok(fd.try_into().unwrap())
    }

    pub fn read(&self, fd: RawFd, buf: &mut [u8]) -> std::io::Result<usize> {
        let binding = get_fd_slots().lock().unwrap();
        let mut file_desc = binding
            .get(fd.try_into().unwrap())
            .cloned()
            .ok_or(ErrorKind::NotFound)?;
        drop(binding);

        file_desc.read(buf)
    }

    pub fn fd_pread(
        &self,
        fd: RawFd,
        off: Option<u64>,
        buf: &mut [u8],
        _flags: IoFlags,
    ) -> std::io::Result<usize> {
        if off.is_some() {
            return Err(ErrorKind::Unsupported.into());
        }
        self.read(fd, buf)
    }

    pub fn fd_pwrite(
        &self,
        fd: RawFd,
        off: Option<u64>,
        buf: &[u8],
        _flags: IoFlags,
    ) -> std::io::Result<usize> {
        if off.is_some() {
            return Err(ErrorKind::Unsupported.into());
        }
        self.write(fd, buf)
    }

    pub fn fd_pwritev(
        &self,
        _fd: RawFd,
        _off: Option<u64>,
        _buf: &[io_vec],
        _flags: IoFlags,
    ) -> std::io::Result<usize> {
        return Err(ErrorKind::Unsupported.into());
    }

    pub fn fd_preadv(
        &self,
        _fd: RawFd,
        _off: Option<u64>,
        _buf: &[io_vec],
        _flags: IoFlags,
    ) -> std::io::Result<usize> {
        return Err(ErrorKind::Unsupported.into());
    }

    pub fn fd_get_info(&self, fd: RawFd) -> Option<twizzler_rt_abi::bindings::fd_info> {
        let mut binding = get_fd_slots().lock().unwrap();
        let Some(fd) = binding.get_mut(fd.try_into().unwrap()) else {
            return None;
        };
        fd.stat().ok().map(|x| x.into())
    }

    pub fn fd_cmd(&self, fd: RawFd, cmd: u32, arg: *const u8, ret: *mut u8) -> u32 {
        let binding = get_fd_slots().lock().unwrap();
        let file_desc = binding.get(fd.try_into().unwrap()).cloned();
        drop(binding);

        file_desc
            .map(|file_desc| file_desc.fd_cmd(cmd, arg, ret))
            .unwrap_or(1)
    }

    pub fn write(&self, fd: RawFd, buf: &[u8]) -> std::io::Result<usize> {
        let binding = get_fd_slots().lock().unwrap();
        let mut file_desc = binding
            .get(fd.try_into().unwrap())
            .cloned()
            .ok_or(ErrorKind::NotFound)?;
        drop(binding);

        file_desc.write(buf)
    }

    pub fn close(&self, fd: RawFd) -> Option<()> {
        let _file_desc = get_fd_slots()
            .lock()
            .unwrap()
            .remove(fd.try_into().unwrap())?;

        Some(())
    }

    pub fn seek(&self, fd: RawFd, pos: SeekFrom) -> std::io::Result<usize> {
        let binding = get_fd_slots().lock().unwrap();
        let mut file_desc = binding
            .get(fd.try_into().unwrap())
            .cloned()
            .ok_or(ErrorKind::NotFound)?;
        drop(binding);

        file_desc.seek(pos)
    }

    pub fn fd_enumerate(
        &self,
        fd: RawFd,
        buf: &mut [twizzler_rt_abi::fd::NameEntry],
        off: usize,
    ) -> std::io::Result<usize> {
        tracing::debug!("fd_enumerate: {} {}", buf.len(), off);
        let stat = self.fd_get_info(fd).ok_or(ErrorKind::Other)?;
        let mut session = get_naming_handle().lock().unwrap();
        let names = session.enumerate_names_nsid(stat.id.into())?;
        if off >= names.len() {
            return Ok(0);
        }
        let end = (off + buf.len()).min(names.len());
        let count = end - off;
        for i in 0..count {
            let name = &names[off + i];
            let ne = twizzler_rt_abi::fd::NameEntry::new(
                name.name().as_bytes(),
                twizzler_rt_abi::fd::FdInfo {
                    kind: match name.kind {
                        naming_core::NsNodeKind::Namespace => {
                            twizzler_rt_abi::fd::FdKind::Directory
                        }
                        naming_core::NsNodeKind::Object => twizzler_rt_abi::fd::FdKind::Regular,
                    },
                    flags: twizzler_rt_abi::fd::FdFlags::empty(),
                    id: name.id.raw(),
                    size: 0,
                }
                .into(),
            );
            buf[i] = ne;
        }
        Ok(count)
    }
}
