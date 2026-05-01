use std::{
    ffi::c_void,
    io::{ErrorKind, SeekFrom},
    mem::ManuallyDrop,
    net::{Shutdown, SocketAddr},
    ops::Deref,
    path::PathBuf,
    sync::{atomic::AtomicU64, Arc, Mutex, OnceLock},
    time::Duration,
};

use bitflags::bitflags;
use lazy_static::lazy_static;
use libc::{S_IFDIR, S_IFLNK, S_IRWXG, S_IRWXO, S_IRWXU};
use monitor_api::{get_comp_config, CompartmentHandle};
use naming_core::{
    dynamic::{dynamic_naming_factory, DynamicNamingHandle},
    GetFlags, NsNodeKind,
};
use raw_file::RawFile;
use socket::SocketKind;
use twizzler_abi::{
    aux::KernelInitInfo,
    object::{ObjID, Protections, MAX_SIZE, NULLPAGE_SIZE},
    syscall::{
        sys_object_create, BackingType, KernelConsoleSource, LifetimeType, ObjectCreate,
        ObjectCreateFlags, ThreadSyncSleep,
    },
};
use twizzler_io::{
    pipe::Pipe,
    pty::{PtyClientHandle, PtyServerHandle, PtySignal},
};
use twizzler_rt_abi::{
    bindings::{
        binding_info, create_options, endpoint, io_ctx, iovec, object_bind_info, open_kind,
        open_kind_OpenKind_KernelConsole, open_kind_OpenKind_Path, prot_kind_ProtKind_Stream,
        socket_address, wait_kind, BIND_DATA_MAX, FD_CMD_DUP, IO_REGISTER_IO_FLAGS, OPEN_FLAG_READ,
        OPEN_FLAG_WRITE,
    },
    error::{ArgumentError, NamingError, ResourceError, TwzError},
    fd::{FdInfo, NameRoot, OpenKind, RawFd, SocketAddress},
    io::{Endpoint, IoFlags},
    object::MapFlags,
    Result,
};

use super::ReferenceRuntime;
use crate::runtime::file::{compartment::CompartmentFile, pty::PtyHandleKind};

mod file_desc;
mod kinds;

trait Fd {
    fn read(
        &self,
        buf: &mut [u8],
        flags: IoFlags,
        offset: Option<u64>,
        ep: Option<&mut Endpoint>,
    ) -> Result<usize>;
    fn write(
        &self,
        buf: &[u8],
        flags: IoFlags,
        offset: Option<u64>,
        to: Option<&Endpoint>,
    ) -> Result<usize>;
    fn seek(&self, _pos: SeekFrom) -> Result<usize> {
        Err(ErrorKind::Unsupported.into())
    }
    fn flush(&self) -> Result<()> {
        Ok(())
    }
    fn stat(&self) -> Result<FdInfo>;
    fn fd_cmd(&self, _cmd: u32, _arg: *const u8, _ret: *mut u8) -> Result<()> {
        Ok((()))
    }
    fn get_config(&self, _reg: u32, _val: *mut c_void, _val_len: usize) -> Result<()> {
        Err(ErrorKind::Unsupported.into())
    }
    fn set_config(&self, _reg: u32, _val: *const c_void, _val_len: usize) -> Result<()> {
        Err(ErrorKind::Unsupported.into())
    }
    fn waitpoint(&self, _kind: wait_kind) -> Result<ThreadSyncSleep> {
        Err(ErrorKind::Unsupported.into())
    }
    fn shutdown(&self, _sh: Shutdown) -> Result<()> {
        Ok(())
    }
}

/*
impl FdKind {
    fn seek(&mut self, pos: SeekFrom) -> Result<usize> {
        match self {
            //FdKind::File(arc) => arc.lock().unwrap().seek(pos),
            FdKind::RawFile(arc) => arc.lock().unwrap().seek(pos),
            FdKind::Dir(_, fpos) => {
                tracing::trace!(
                    "seeking directory with pos {:?} and current pos {}",
                    pos,
                    *fpos.lock().unwrap()
                );
                let new_pos = match pos {
                    SeekFrom::Start(off) => off as isize,
                    SeekFrom::End(off) => *fpos.lock().unwrap() as isize + off as isize,
                    SeekFrom::Current(off) => *fpos.lock().unwrap() as isize + off as isize,
                };
                if new_pos < 0 {
                    return Err(ArgumentError::InvalidArgument.into());
                }
                *fpos.lock().unwrap() = new_pos as usize;
                Ok(new_pos as usize)
            }
            _ => Ok(0),
        }
    }

    fn stat(&mut self) -> Result<FdInfo> {
        match self {
            //FdKind::File(arc) => arc.lock().unwrap().stat(),
            FdKind::RawFile(arc) => arc.lock().unwrap().stat(),
            FdKind::Dir(id, _) => Ok(FdInfo {
                flags: twizzler_rt_abi::fd::FdFlags::from_bits_truncate(0),
                kind: twizzler_rt_abi::fd::FdKind::Directory,
                size: 0,
                id: id.raw(),
                created: Duration::from_secs(0).into(),
                modified: Duration::from_secs(0).into(),
                accessed: Duration::from_secs(0).into(),
                unix_mode: S_IFDIR | S_IRWXO | S_IRWXG | S_IRWXO | S_IRWXU,
            }),
            FdKind::SymLink => Ok(FdInfo {
                flags: twizzler_rt_abi::fd::FdFlags::from_bits_truncate(0),
                kind: twizzler_rt_abi::fd::FdKind::SymLink,
                size: 0,
                id: 0,
                created: Duration::from_secs(0).into(),
                modified: Duration::from_secs(0).into(),
                accessed: Duration::from_secs(0).into(),
                unix_mode: S_IFLNK | S_IRWXU | S_IRWXG | S_IRWXO,
            }),
            _ => Ok(FdInfo {
                flags: twizzler_rt_abi::fd::FdFlags::from_bits_truncate(0),
                kind: twizzler_rt_abi::fd::FdKind::Other,
                size: 0,
                id: 0,
                created: Duration::from_secs(0).into(),
                modified: Duration::from_secs(0).into(),
                accessed: Duration::from_secs(0).into(),
                unix_mode: 0,
            }),
        }
    }

    pub fn fd_cmd(&mut self, cmd: u32, arg: *const u8, ret: *mut u8) -> Result<()> {
        match self {
            FdKind::RawFile(file) => file.lock().unwrap().fd_cmd(cmd, arg, ret),
            //FdKind::File(arc) => arc.lock().unwrap().fd_cmd(cmd, arg, ret),
            FdKind::Socket(socket) => {
                if cmd == twizzler_rt_abi::bindings::FD_CMD_SHUTDOWN {
                    let val = unsafe { arg.cast::<u32>().read() };
                    let shutdown = match val {
                        0 => return Err(TwzError::INVALID_ARGUMENT),
                        1 => std::net::Shutdown::Read,
                        2 => std::net::Shutdown::Write,
                        _ => std::net::Shutdown::Both,
                    };
                    socket.shutdown(shutdown)?;
                    Ok(())
                } else {
                    Err(TwzError::NOT_SUPPORTED)
                }
            }
            FdKind::Pipe(pipe) => {
                if cmd == twizzler_rt_abi::bindings::FD_CMD_SHUTDOWN {
                    let val = unsafe { arg.cast::<u32>().read() };
                    let shutdown = match val {
                        0 => return Err(TwzError::INVALID_ARGUMENT),
                        1 => std::net::Shutdown::Read,
                        2 => std::net::Shutdown::Write,
                        _ => std::net::Shutdown::Both,
                    };
                    tracing::debug!("Pipe shutdown requested: {:?}", shutdown);
                    if matches!(shutdown, Shutdown::Both) || matches!(shutdown, Shutdown::Read) {
                        pipe.close_reader();
                    }
                    if matches!(shutdown, Shutdown::Both) || matches!(shutdown, Shutdown::Write) {
                        pipe.close_writer();
                    }
                    Ok(())
                } else {
                    Err(TwzError::NOT_SUPPORTED)
                }
            }
            _ => Err(TwzError::NOT_SUPPORTED),
        }
    }

    pub fn read_from(
        &mut self,
        buf: &mut [u8],
        ep: &mut twizzler_rt_abi::io::Endpoint,
        flags: IoFlags,
    ) -> std::io::Result<usize> {
        match self {
            //FdKind::File(arc) => arc.lock().unwrap().read(buf),
            FdKind::RawFile(arc) => arc.lock().unwrap().read(buf),
            FdKind::KernelConsole => {
                let len = twizzler_abi::syscall::sys_kernel_console_read(
                    KernelConsoleSource::Console,
                    buf,
                    twizzler_abi::syscall::KernelConsoleReadFlags::empty(),
                )?;
                Ok(len)
            }
            FdKind::Dir(_, _) => Err(ErrorKind::IsADirectory.into()),
            FdKind::SymLink => Err(ErrorKind::InvalidData.into()),
            FdKind::Socket(socket) => socket.read_from(buf, ep, flags),
            FdKind::Pty(pty) => pty.read(buf),
            FdKind::Pipe(pipe) => pipe.read(buf),
            FdKind::Compartment(comp) => comp.read(buf),
        }
    }

    pub fn read(&mut self, buf: &mut [u8], flags: IoFlags) -> std::io::Result<usize> {
        match self {
            //FdKind::File(arc) => arc.lock().unwrap().read(buf),
            FdKind::RawFile(arc) => arc.lock().unwrap().read(buf),
            FdKind::KernelConsole => {
                let len = twizzler_abi::syscall::sys_kernel_console_read(
                    KernelConsoleSource::Console,
                    buf,
                    twizzler_abi::syscall::KernelConsoleReadFlags::empty(),
                )?;
                Ok(len)
            }
            FdKind::Dir(_, _) => Err(ErrorKind::IsADirectory.into()),
            FdKind::SymLink => Err(ErrorKind::InvalidData.into()),
            FdKind::Socket(socket) => socket.read(buf, flags),
            FdKind::Pty(pty) => pty.read(buf),
            FdKind::Pipe(pipe) => pipe.read(buf),
            FdKind::Compartment(comp) => comp.read(buf),
        }
    }

    pub fn write_to(
        &mut self,
        buf: &[u8],
        ep: &twizzler_rt_abi::io::Endpoint,
        flags: IoFlags,
    ) -> std::io::Result<usize> {
        match self {
            //FdKind::File(arc) => arc.lock().unwrap().read(buf),
            FdKind::RawFile(arc) => arc.lock().unwrap().write(buf),
            FdKind::KernelConsole => {
                twizzler_abi::syscall::sys_kernel_console_write(
                    KernelConsoleSource::Console,
                    buf,
                    twizzler_abi::syscall::KernelConsoleWriteFlags::empty(),
                );
                Ok(buf.len())
            }
            FdKind::Dir(_, _) => Err(ErrorKind::IsADirectory.into()),
            FdKind::SymLink => Err(ErrorKind::InvalidData.into()),
            FdKind::Socket(socket) => socket.write_to(buf, ep, flags),
            FdKind::Pty(pty) => pty.write(buf),
            FdKind::Pipe(pipe) => pipe.write(buf),
            FdKind::Compartment(comp) => comp.write(buf),
        }
    }
}

impl FdKind {
    /// Positional read: read from `offset` (or current position if `None`) without updating the
    /// fd's internal position.
    fn pread(&mut self, buf: &mut [u8], offset: Option<u64>) -> std::io::Result<usize> {
        match self {
            FdKind::RawFile(arc) => {
                let mut f = arc.lock().unwrap();
                let off = offset.unwrap_or(f.pos);
                f.pread(buf, off)
            }
            // For everything else fall back to seek-then-read (or just read if no offset).
            other => {
                if let Some(off) = offset {
                    other.seek(SeekFrom::Start(off)).ok();
                }
                other.read(buf, IoFlags::empty())
            }
        }
    }

    /// Positional write: write at `offset` (or current position if `None`) without updating the
    /// fd's internal position.
    fn pwrite(&mut self, buf: &[u8], offset: Option<u64>) -> std::io::Result<usize> {
        match self {
            FdKind::RawFile(arc) => {
                let mut f = arc.lock().unwrap();
                let off = offset.unwrap_or(f.pos);
                f.pwrite(buf, off)
            }
            other => {
                if let Some(off) = offset {
                    other.seek(SeekFrom::Start(off)).ok();
                }
                other.write(buf, IoFlags::empty())
            }
        }
    }
}

impl FdKind {
    fn write(&mut self, buf: &[u8], flags: IoFlags) -> std::io::Result<usize> {
        match self {
            //FdKind::File(arc) => arc.lock().unwrap().write(buf),
            FdKind::RawFile(arc) => arc.lock().unwrap().write(buf),
            FdKind::KernelConsole => {
                twizzler_abi::syscall::sys_kernel_console_write(
                    KernelConsoleSource::Console,
                    buf,
                    twizzler_abi::syscall::KernelConsoleWriteFlags::empty(),
                );
                Ok(buf.len())
            }
            FdKind::Dir(_, _) => Err(ErrorKind::IsADirectory.into()),
            FdKind::SymLink => Err(ErrorKind::InvalidData.into()),
            FdKind::Socket(socket) => socket.write(buf, flags),
            FdKind::Pty(pty) => pty.write(buf),
            FdKind::Pipe(pipe) => pipe.write(buf),
            FdKind::Compartment(comp) => comp.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            //FdKind::File(arc) => arc.lock().unwrap().flush(),
            FdKind::RawFile(arc) => arc.lock().unwrap().flush(),
            FdKind::KernelConsole => Ok(()),
            FdKind::Dir(_, _) => Err(ErrorKind::IsADirectory.into()),
            FdKind::SymLink => Err(ErrorKind::InvalidData.into()),
            FdKind::Socket(socket) => socket.flush(),
            FdKind::Pty(pty) => pty.flush(),
            FdKind::Pipe(pipe) => pipe.flush(),
            FdKind::Compartment(comp) => comp.flush(),
        }
    }
}

*/
/// Extract the optional file offset from an `io_ctx`. Returns `None` when the offset is `FD_POS`
/// (meaning "use the fd's current position").
fn io_ctx_offset(ctx: *mut io_ctx) -> Option<u64> {
    let raw_offset = if ctx.is_null() {
        twizzler_rt_abi::bindings::FD_POS
    } else {
        unsafe { (*ctx).offset }
    };
    if raw_offset == twizzler_rt_abi::bindings::FD_POS {
        None
    } else {
        Some(raw_offset as u64)
    }
}

#[derive(Clone)]
struct MaybeNoDrop<T> {
    pub should_drop: bool,
    t: ManuallyDrop<T>,
}

impl<T> Deref for MaybeNoDrop<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.t.deref()
    }
}

impl<T> MaybeNoDrop<T> {
    fn new(t: T, should_drop: bool) -> Self {
        Self {
            should_drop,
            t: ManuallyDrop::new(t),
        }
    }
}

impl<T> AsRef<T> for MaybeNoDrop<T> {
    fn as_ref(&self) -> &T {
        &self.t
    }
}

impl<T> Drop for MaybeNoDrop<T> {
    fn drop(&mut self) {
        if self.should_drop {
            unsafe { ManuallyDrop::<T>::drop(&mut self.t) };
        }
    }
}

#[derive(Clone)]
struct FileDesc {
    file: Arc<dyn Fd + Send>,
    binding: MaybeNoDrop<Arc<binding_info>>,
    flags: IoFlags,
}

impl FileDesc {
    fn io_ctx_flags(&self, ctx: *mut io_ctx) -> IoFlags {
        self.flags
            | if ctx.is_null() {
                IoFlags::empty()
            } else {
                IoFlags::from_bits_truncate(unsafe { (*ctx).flags })
            }
    }

    pub fn new(
        file: Arc<dyn Fd + Send>,
        bind_kind: open_kind,
        flags: u32,
        bind_info: Option<&[u8]>,
        should_drop: bool,
    ) -> Self {
        let bind_len = bind_info.map_or(0, |bi| bi.len()).min(BIND_DATA_MAX);
        let mut binding = binding_info {
            kind: bind_kind,
            fd: 0,
            flags,
            bind_data: [0; _],
            bind_len: bind_len as u32,
        };
        if let Some(bind_info) = bind_info {
            binding.bind_data[0..bind_len].copy_from_slice(&bind_info[0..bind_len])
        }
        FileDesc {
            file,
            binding: MaybeNoDrop::new(Arc::new(binding), should_drop),
            flags: IoFlags::empty(),
        }
    }

    pub fn seek(&self, pos: SeekFrom) -> Result<usize> {
        self.file.seek(pos).into()
    }

    pub fn stat(&self) -> Result<FdInfo> {
        self.file.stat().into()
    }

    pub fn fd_cmd(&mut self, cmd: u32, arg: *const u8, ret: *mut u8) -> Result<()> {
        if cmd == twizzler_rt_abi::bindings::FD_CMD_SHUTDOWN {
            let val = unsafe { arg.cast::<u32>().read() };
            let shutdown = match val {
                0 => return Err(TwzError::INVALID_ARGUMENT),
                1 => std::net::Shutdown::Read,
                2 => std::net::Shutdown::Write,
                _ => std::net::Shutdown::Both,
            };
            let mut b = **self.binding;
            let flags = match shutdown {
                Shutdown::Read => b.flags & !OPEN_FLAG_READ,
                Shutdown::Write => b.flags & !OPEN_FLAG_WRITE,
                Shutdown::Both => b.flags & !(OPEN_FLAG_READ | OPEN_FLAG_WRITE),
            };
            b.flags = flags;
            self.binding = MaybeNoDrop::new(Arc::new(b), true);
        }
        self.file.fd_cmd(cmd, arg, ret).into()
    }

    fn pread(&mut self, buf: &mut [u8], ctx: *mut io_ctx) -> Result<usize> {
        let offset = io_ctx_offset(ctx);
        let flags = self.io_ctx_flags(ctx);
        self.file.read(buf, flags, offset, None)
    }

    fn pwrite(&mut self, buf: &[u8], ctx: *mut io_ctx) -> Result<usize> {
        let offset = io_ctx_offset(ctx);
        let flags = self.io_ctx_flags(ctx);
        self.file.write(buf, flags, offset, None)
    }

    fn pread_from(
        &mut self,
        buf: &mut [u8],
        ctx: *mut io_ctx,
        ep: &mut twizzler_rt_abi::io::Endpoint,
    ) -> Result<usize> {
        let offset = io_ctx_offset(ctx);
        let flags = self.io_ctx_flags(ctx);
        self.file.read(buf, flags, offset, Some(ep))
    }

    fn pwrite_to(
        &mut self,
        buf: &[u8],
        ctx: *mut io_ctx,
        ep: &twizzler_rt_abi::io::Endpoint,
    ) -> Result<usize> {
        let offset = io_ctx_offset(ctx);
        let flags = self.io_ctx_flags(ctx);
        self.file.write(buf, flags, offset, Some(ep))
    }
}

const MAX_FD: usize = 1024;

struct FdSlots {
    slots: [Option<FileDesc>; MAX_FD],
}

impl FdSlots {
    pub fn insert(&mut self, idx: usize, elem: FileDesc) -> Option<FileDesc> {
        self.slots[idx].replace(elem)
    }

    pub fn insert_first_empty(&mut self, elem: FileDesc) -> Option<usize> {
        for i in 0..MAX_FD {
            if self.slots[i].is_none() {
                self.insert(i, elem);
                return Some(i);
            }
        }
        None
    }

    pub fn get(&self, idx: usize) -> Option<&FileDesc> {
        self.slots[idx].as_ref()
    }

    pub fn get_mut(&mut self, idx: usize) -> Option<&mut FileDesc> {
        self.slots[idx].as_mut()
    }

    pub fn remove(&mut self, idx: usize) -> Option<FileDesc> {
        self.slots[idx].take()
    }
}

lazy_static! {
    static ref FD_SLOTS: Mutex<FdSlots> = {
        let mut slots = FdSlots {
            slots: [const { None }; MAX_FD],
        };
        slots.insert(
            0,
            FileDesc::new(
                FdKind::KernelConsole,
                open_kind_OpenKind_KernelConsole,
                0,
                None,
                false,
            ),
        );
        slots.insert(
            1,
            FileDesc::new(
                FdKind::KernelConsole,
                open_kind_OpenKind_KernelConsole,
                0,
                None,
                false,
            ),
        );
        slots.insert(
            2,
            FileDesc::new(
                FdKind::KernelConsole,
                open_kind_OpenKind_KernelConsole,
                0,
                None,
                false,
            ),
        );
        Mutex::new(slots)
    };
}

static HANDLE: OnceLock<Mutex<DynamicNamingHandle>> = OnceLock::new();

#[track_caller]
fn get_fd_slots() -> &'static Mutex<FdSlots> {
    &FD_SLOTS
}

pub fn get_naming_handle() -> Option<&'static Mutex<DynamicNamingHandle>> {
    if let Some(h) = HANDLE.get() {
        return Some(h);
    }
    if CompartmentHandle::lookup("naming").is_err() {
        return None;
    }
    HANDLE
        .get_or_try_init(|| {
            let f = dynamic_naming_factory().ok_or(())?;
            Ok::<_, ()>(Mutex::new(f))
        })
        .ok()
}

#[derive(Debug)]
pub enum CreateOptions {
    UNEXPECTED,
    CreateKindExisting,
    CreateKindNew,
    CreateKindEither,
    CreateKindBind(ObjID),
}

impl From<create_options> for CreateOptions {
    fn from(value: create_options) -> Self {
        match value.kind {
            twizzler_rt_abi::bindings::CREATE_KIND_EITHER => CreateOptions::CreateKindEither,
            twizzler_rt_abi::bindings::CREATE_KIND_NEW => {
                if value.id != 0 {
                    CreateOptions::CreateKindBind(value.id.into())
                } else {
                    CreateOptions::CreateKindNew
                }
            }
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
        const OPEN_FLAG_SYMLINK = twizzler_rt_abi::bindings::OPEN_FLAG_SYMLINK;
    }
}

impl From<u32> for OperationOptions {
    fn from(value: u32) -> Self {
        OperationOptions::from_bits_truncate(value)
    }
}

fn pty_signal_handler(server: &PtyServerHandle, sig: PtySignal) {
    let signal = match sig {
        PtySignal::Interrupt => libc::SIGINT,
        PtySignal::Quit => libc::SIGQUIT,
        PtySignal::Status => libc::SIGINFO,
    } as u64;
    let _ = monitor_api::post_signal(
        Some(server.object().id()),
        signal,
        monitor_api::PostSignalFlags::CONTROLLER,
    )
    .inspect_err(|e| {
        tracing::warn!(
            "failed to raise signal for controller {}: {}",
            server.object().id(),
            e
        )
    });
}

impl ReferenceRuntime {
    pub(crate) fn close_fds(&self) {
        for (_i, fd) in get_fd_slots().lock().unwrap().slots.iter_mut().enumerate() {
            if let Some(fd) = fd.take() {
                drop(fd);
            }
        }
    }

    pub(crate) fn init_fds(&self) {
        let loader_config = &get_comp_config().loader_config;

        if loader_config.fd_spec.is_null() {
            return;
        }

        let slice = unsafe {
            core::slice::from_raw_parts::<binding_info>(
                loader_config.fd_spec,
                loader_config.fd_spec_len,
            )
        };

        for bi in slice {
            let Ok(kind) = OpenKind::try_from(bi.kind) else {
                continue;
            };
            if bi.fd > 2 {
                continue;
            }
            let _ = self
                .open(
                    Some(bi.fd),
                    kind,
                    OperationOptions::from_bits_truncate(bi.flags),
                    bi.bind_data.as_ptr().cast(),
                    bi.bind_len as usize,
                    false,
                )
                .inspect_err(|e| {
                    twizzler_abi::klog_println!("Failed to open fd ({}): {}", bi.fd, e);
                });
        }
    }

    pub fn canon_name(
        &self,
        resolver: twizzler_rt_abi::fd::NameResolver,
        name: &[u8],
        out_name: &mut [u8],
    ) -> Result<usize> {
        if matches!(resolver, twizzler_rt_abi::fd::NameResolver::Socket) {
            let Ok(name) = str::from_utf8(name) else {
                return Err(TwzError::INVALID_ARGUMENT);
            };
            let out_slice: &mut [socket_address] = unsafe {
                core::slice::from_raw_parts_mut(
                    out_name.as_mut_ptr().cast(),
                    out_name.len() / size_of::<socket_address>(),
                )
            };

            let res = crate::runtime::file::socket::dns(name)?;
            for i in 0..res.len().min(out_slice.len()) {
                let sa = SocketAddress::from(res[i]);
                out_slice[i] = sa.0;
            }
            return Ok(res.len().min(out_slice.len()) * size_of::<socket_address>());
        }
        let path = PathBuf::from(str::from_utf8(name).map_err(|_| TwzError::INVALID_ARGUMENT)?);
        let path = if !path.is_absolute() {
            let mut cd = std::env::current_dir()?;
            cd.push(path);
            cd
        } else {
            path
        };

        let npath = path.normalize_lexically().unwrap_or(path);
        let path = npath.to_str().unwrap().as_bytes();

        let len = out_name.len().min(path.len());
        out_name[0..len].copy_from_slice(&path[0..len]);
        Ok(len)
    }

    pub fn resolve_name(
        &self,
        _resolver: twizzler_rt_abi::fd::NameResolver,
        name: &[u8],
    ) -> Result<ObjID> {
        let name = str::from_utf8(name).map_err(|_| TwzError::INVALID_ARGUMENT)?;
        let h = get_naming_handle();
        if h.is_none() {
            fn get_kernel_init_info() -> &'static KernelInitInfo {
                unsafe {
                    (((twizzler_abi::slot::RESERVED_KERNEL_INIT * MAX_SIZE) + NULLPAGE_SIZE)
                        as *const KernelInitInfo)
                        .as_ref()
                        .unwrap()
                }
            }

            fn find_init_name(name: &str) -> Option<(ObjID, String)> {
                let init_info = get_kernel_init_info();
                for n in init_info.names() {
                    if n.name() == name {
                        return Some((n.id(), name.to_string()));
                    }
                }
                None
            }
            let id = find_init_name(name).ok_or(NamingError::NotFound)?;
            return Ok(id.0);
        }
        let mut session = get_naming_handle().unwrap().lock().unwrap();
        let res = session.get(name, GetFlags::FOLLOW_SYMLINK)?;
        tracing::trace!("resolve got {:?}", res);
        Ok(res.id)
    }

    pub fn mkns(&self, name: &str) -> Result<()> {
        let mut session = get_naming_handle()
            .ok_or(TwzError::NOT_SUPPORTED)?
            .lock()
            .unwrap();

        session.put_namespace(name, true)?;
        Ok(())
    }

    pub fn symlink(&self, name: &str, target: &str) -> Result<()> {
        let mut session = get_naming_handle()
            .ok_or(TwzError::NOT_SUPPORTED)?
            .lock()
            .unwrap();

        session.symlink(name, target)?;
        Ok(())
    }

    pub fn readlink(&self, name: &str, target: &mut [u8], read_len: &mut u64) -> Result<()> {
        let mut session = get_naming_handle()
            .ok_or(TwzError::NOT_SUPPORTED)?
            .lock()
            .unwrap();
        let node = session.get(name, GetFlags::empty())?;

        let link = node.readlink()?;
        let len = target.len().min(link.as_bytes().len());
        target[0..len].copy_from_slice(&link.as_bytes()[0..len]);
        *read_len = len as u64;
        Ok(())
    }

    pub fn read_binds(&self, binds: &mut [binding_info]) -> usize {
        let bindings = get_fd_slots().lock().unwrap();
        let mut idx = 0;
        for (fd, info) in bindings.slots.iter().enumerate() {
            if idx >= binds.len() {
                return idx;
            }
            if let Some(info) = info {
                binds[idx] = **info.binding;
                binds[idx].fd = fd.try_into().unwrap();
                idx += 1;
            }
        }
        return idx;
    }

    pub fn open(
        &self,
        existing_fd: Option<RawFd>,
        kind: OpenKind,
        open_opt: OperationOptions,
        bind_info: *const c_void,
        bind_info_len: usize,
        should_drop: bool,
    ) -> Result<RawFd> {
        let bind_info_bytes = if bind_info.is_null() {
            &[]
        } else {
            unsafe { core::slice::from_raw_parts(bind_info.cast::<u8>(), bind_info_len) }
        };
        let mut elem = kinds::open(existing_fd, kind, binding, binding_len, opts)?;

        if elem.is_none() && existing_fd.is_none() {
            return Err(TwzError::NOT_SUPPORTED);
        }

        if elem.is_none() {
            return Ok(existing_fd.unwrap());
        }
        let elem = elem.unwrap();

        let elem = match kind {
            OpenKind::Pipe => {
                let binding_info = object_bind_info {
                    id: elem.id().raw(),
                };
                let bind_info_bytes = unsafe {
                    core::slice::from_raw_parts(
                        &binding_info as *const object_bind_info as *const u8,
                        std::mem::size_of::<object_bind_info>(),
                    )
                };

                if !open_opt.contains(OperationOptions::OPEN_FLAG_READ) {
                    let _ = elem.shutdown(Shutdown::Read);
                }

                if !open_opt.contains(OperationOptions::OPEN_FLAG_WRITE) {
                    let _ = elem.shutdown(Shutdown::Write);
                }

                FileDesc::new(
                    elem,
                    kind as u32,
                    open_opt.bits(),
                    Some(bind_info_bytes),
                    should_drop,
                )
            }
            _ => FileDesc::new(
                elem,
                kind as u32,
                open_opt.bits(),
                Some(bind_info_bytes),
                should_drop,
            ),
        };

        let mut binding = get_fd_slots().lock().unwrap();

        let fd = if let Some(fd) = existing_fd {
            binding.insert(fd.try_into().unwrap(), elem);
            Some(fd as usize)
        } else {
            binding.insert_first_empty(elem)
        }
        .ok_or(ResourceError::OutOfNames)?;

        drop(binding);
        if open_opt.contains(OperationOptions::OPEN_FLAG_TAIL) {
            self.seek(fd.try_into().unwrap(), SeekFrom::End(0))?;
        }
        Ok(fd.try_into().unwrap())
    }

    pub fn rename(&self, old: &str, new: &str) -> Result<()> {
        let mut session = get_naming_handle()
            .ok_or(TwzError::NOT_SUPPORTED)?
            .lock()
            .unwrap();
        Ok(session.rename(old, new)?)
    }

    pub fn remove(&self, path: &str) -> Result<()> {
        let mut session = get_naming_handle()
            .ok_or(TwzError::NOT_SUPPORTED)?
            .lock()
            .unwrap();
        Ok(session.remove(path)?)
    }

    pub fn read(&self, fd: RawFd, buf: &mut [u8], ctx: *mut io_ctx) -> Result<usize> {
        let binding = get_fd_slots().lock().unwrap();
        let file_desc = binding
            .get(fd.try_into().unwrap())
            .cloned()
            .ok_or(ArgumentError::BadHandle)?;
        drop(binding);

        let len = file_desc
            .file
            .read(buf, file_desc.io_ctx_flags(ctx), None, None)?;
        Ok(len)
    }

    pub fn fd_pread_from(
        &self,
        fd: RawFd,
        buf: &mut [u8],
        ctx: *mut io_ctx,
        ep: *mut endpoint,
    ) -> Result<usize> {
        let ep = unsafe { ep.cast::<twizzler_rt_abi::io::Endpoint>().as_mut().unwrap() };
        let binding = get_fd_slots().lock().unwrap();
        let mut file_desc = binding
            .get(fd.try_into().unwrap())
            .cloned()
            .ok_or(ArgumentError::BadHandle)?;
        drop(binding);
        Ok(file_desc.pread_from(buf, ctx, ep)?)
    }

    pub fn fd_pwrite_to(
        &self,
        fd: RawFd,
        buf: &[u8],
        ctx: *mut io_ctx,
        ep: *const endpoint,
    ) -> Result<usize> {
        let ep = unsafe { ep.cast::<twizzler_rt_abi::io::Endpoint>().as_ref().unwrap() };
        let binding = get_fd_slots().lock().unwrap();
        let mut file_desc = binding
            .get(fd.try_into().unwrap())
            .cloned()
            .ok_or(ArgumentError::BadHandle)?;
        drop(binding);
        Ok(file_desc.pwrite_to(buf, ctx, ep)?)
    }

    pub fn fd_pread(&self, fd: RawFd, buf: &mut [u8], ctx: *mut io_ctx) -> Result<usize> {
        let binding = get_fd_slots().lock().unwrap();
        let mut file_desc = binding
            .get(fd.try_into().unwrap())
            .cloned()
            .ok_or(ArgumentError::BadHandle)?;
        drop(binding);
        Ok(file_desc.pread(buf, ctx)?)
    }

    pub fn fd_pwrite(&self, fd: RawFd, buf: &[u8], ctx: *mut io_ctx) -> Result<usize> {
        let binding = get_fd_slots().lock().unwrap();
        let mut file_desc = binding
            .get(fd.try_into().unwrap())
            .cloned()
            .ok_or(ArgumentError::BadHandle)?;
        drop(binding);
        Ok(file_desc.pwrite(buf, ctx)?)
    }

    pub fn fd_pwritev(&self, fd: RawFd, iovs: &[iovec], ctx: *mut io_ctx) -> Result<usize> {
        let binding = get_fd_slots().lock().unwrap();
        let mut file_desc = binding
            .get(fd.try_into().unwrap())
            .cloned()
            .ok_or(ArgumentError::BadHandle)?;
        drop(binding);
        let mut total = 0usize;
        for iov in iovs {
            let slice =
                unsafe { core::slice::from_raw_parts(iov.iov_base.cast::<u8>(), iov.iov_len) };
            total += file_desc.pwrite(slice, ctx)?;
        }
        Ok(total)
    }

    pub fn fd_preadv(&self, fd: RawFd, iovs: &[iovec], ctx: *mut io_ctx) -> Result<usize> {
        let binding = get_fd_slots().lock().unwrap();
        let mut file_desc = binding
            .get(fd.try_into().unwrap())
            .cloned()
            .ok_or(ArgumentError::BadHandle)?;
        drop(binding);
        let mut total = 0usize;
        for iov in iovs {
            let slice =
                unsafe { core::slice::from_raw_parts_mut(iov.iov_base.cast::<u8>(), iov.iov_len) };
            total += file_desc.pread(slice, ctx)?;
        }
        Ok(total)
    }

    pub fn fd_get_info(&self, fd: RawFd) -> Option<twizzler_rt_abi::bindings::fd_info> {
        let mut binding = get_fd_slots().lock().unwrap();
        let Some(fd) = binding.get_mut(fd.try_into().unwrap()) else {
            return None;
        };
        fd.stat().ok().map(|x| x.into())
    }

    pub fn fd_get_config(
        &self,
        fd: RawFd,
        reg: u32,
        val: *mut c_void,
        val_len: usize,
    ) -> Result<()> {
        let mut binding = get_fd_slots().lock().unwrap();
        let Some(fd) = binding.get_mut(fd.try_into().unwrap()) else {
            return Err(TwzError::INVALID_ARGUMENT);
        };

        if reg == IO_REGISTER_IO_FLAGS {
            if val_len != size_of::<u32>() {
                return Err(TwzError::INVALID_ARGUMENT);
            }
            unsafe { val.cast::<u32>().write(fd.flags.bits()) };
            return Ok(());
        }

        let buf = unsafe { core::slice::from_raw_parts_mut(val.cast::<u8>(), val_len) };
        buf.fill(0);
        fd.file.get_config(reg, val, val_len).into()
    }

    pub fn fd_set_config(
        &self,
        fd: RawFd,
        reg: u32,
        val: *const c_void,
        val_len: usize,
    ) -> Result<()> {
        let mut binding = get_fd_slots().lock().unwrap();
        let Some(fd) = binding.get_mut(fd.try_into().unwrap()) else {
            return Err(TwzError::INVALID_ARGUMENT);
        };

        if reg == IO_REGISTER_IO_FLAGS {
            if val_len != size_of::<u32>() {
                return Err(TwzError::INVALID_ARGUMENT);
            }
            let val = unsafe { val.cast::<u32>().read() };
            fd.flags = IoFlags::from_bits_truncate(val);
            return Ok(());
        }
        fd.file.set_config(reg, val, val_len).into()
    }

    pub fn fd_cmd(&self, fd: RawFd, cmd: u32, arg: *const u8, ret: *mut u8) -> Result<()> {
        let mut binding = get_fd_slots().lock().unwrap();
        let file_desc = binding.get_mut(fd.try_into().unwrap());

        let file_desc = file_desc.ok_or(TwzError::INVALID_ARGUMENT)?;

        if cmd == FD_CMD_DUP {
            let mut nfd = file_desc.clone();
            let b = **nfd.binding;
            nfd.binding = MaybeNoDrop::new(Arc::new(b), true);
            let newfd = binding
                .insert_first_empty(nfd)
                .ok_or(ResourceError::OutOfNames)?;
            unsafe {
                ret.cast::<RawFd>().write(newfd.try_into().unwrap());
            }
            return Ok(());
        }
        file_desc.fd_cmd(cmd, arg, ret)
    }

    pub fn write(&self, fd: RawFd, buf: &[u8], ctx: *mut io_ctx) -> Result<usize> {
        let binding = get_fd_slots().lock().unwrap();
        let file_desc = binding
            .get(fd.try_into().unwrap())
            .cloned()
            .ok_or(ArgumentError::BadHandle)?;
        drop(binding);

        let len = file_desc
            .file
            .write(buf, file_desc.io_ctx_flags(ctx), None, None)?;
        Ok(len)
    }

    pub fn close(&self, fd: RawFd) -> Option<()> {
        let Some(file_desc) = get_fd_slots()
            .lock()
            .unwrap()
            .remove(fd.try_into().unwrap())
        else {
            return Some(());
        };

        file_desc.file.shutdown(Shutdown::Both).ok()?;

        Some(())
    }

    pub fn seek(&self, fd: RawFd, pos: SeekFrom) -> Result<usize> {
        let binding = get_fd_slots().lock().unwrap();
        let file_desc = binding
            .get(fd.try_into().unwrap())
            .cloned()
            .ok_or(ArgumentError::BadHandle)?;
        drop(binding);

        file_desc.seek(pos)
    }

    pub fn set_nameroot(&self, root: NameRoot, slice: &[u8]) -> Result<()> {
        let path = PathBuf::from(str::from_utf8(slice).unwrap());
        let mut nr = self.nameroots.lock();
        if let Some(namer) = get_naming_handle() {
            namer.lock().unwrap().change_namespace(&path)?;
        }
        if path.is_absolute() {
            let path = path.canonicalize()?;
            nr.insert(root, path);
            return Ok(());
        }
        let mut cur = nr.get(&root).cloned().unwrap_or_else(|| PathBuf::from("/"));
        cur.push(path);
        let cur = cur.canonicalize()?;
        nr.insert(root, cur);

        Ok(())
    }

    pub fn fd_waitpoint(&self, fd: RawFd, kind: wait_kind) -> Result<(*const AtomicU64, u64)> {
        let binding = get_fd_slots().lock().unwrap();
        let file_desc = binding
            .get(fd.try_into().unwrap())
            .cloned()
            .ok_or(ArgumentError::BadHandle)?;
        drop(binding);

        file_desc.file.waitpoint(kind).into()
    }

    pub fn get_nameroot(&self, root: NameRoot, slice: &mut [u8]) -> Result<usize> {
        let nr = self.nameroots.lock();
        let data = nr
            .get(&root)
            .map(|n| n.to_str().unwrap().as_bytes())
            .unwrap_or(b"/");
        let len = data.len().min(slice.len());
        slice[0..len].copy_from_slice(&data[0..len]);
        Ok(len)
    }

    pub fn fd_enumerate(
        &self,
        fd: RawFd,
        buf: &mut [twizzler_rt_abi::fd::NameEntry],
        off: usize,
    ) -> Result<usize> {
        tracing::trace!(
            "fd_enumerate: fd={}, off={} ({}), buf_len={}",
            fd,
            off,
            off * size_of::<twizzler_rt_abi::fd::NameEntry>(),
            buf.len()
        );
        let stat = self.fd_get_info(fd).ok_or(ArgumentError::BadHandle)?;
        let mut session = get_naming_handle()
            .ok_or(TwzError::NOT_SUPPORTED)?
            .lock()
            .unwrap();
        let names = session.enumerate_names_nsid(stat.id.into(), off, buf.len())?;
        tracing::trace!("enumerate_names_nsid returned {} entries", names.len());
        let end = buf.len().min(names.len());
        for i in 0..end {
            let name = &names[i];
            let Ok(entry_name) = name.name() else {
                continue;
            };
            let ne = if name.kind == NsNodeKind::SymLink {
                twizzler_rt_abi::fd::NameEntry::new_symlink(
                    entry_name.as_bytes(),
                    name.readlink()?.as_bytes(),
                    twizzler_rt_abi::fd::FdInfo {
                        kind: match name.kind {
                            naming_core::NsNodeKind::Namespace => {
                                twizzler_rt_abi::fd::FdKind::Directory
                            }
                            naming_core::NsNodeKind::Object => twizzler_rt_abi::fd::FdKind::Regular,
                            naming_core::NsNodeKind::SymLink => {
                                twizzler_rt_abi::fd::FdKind::SymLink
                            }
                        },
                        flags: twizzler_rt_abi::fd::FdFlags::empty(),
                        id: name.id.raw(),
                        size: 0,
                        unix_mode: 0,
                        accessed: std::time::Duration::ZERO,
                        modified: std::time::Duration::ZERO,
                        created: std::time::Duration::ZERO,
                    }
                    .into(),
                )
            } else {
                twizzler_rt_abi::fd::NameEntry::new(
                    entry_name.as_bytes(),
                    twizzler_rt_abi::fd::FdInfo {
                        kind: match name.kind {
                            naming_core::NsNodeKind::Namespace => {
                                twizzler_rt_abi::fd::FdKind::Directory
                            }
                            naming_core::NsNodeKind::Object => twizzler_rt_abi::fd::FdKind::Regular,
                            naming_core::NsNodeKind::SymLink => {
                                twizzler_rt_abi::fd::FdKind::SymLink
                            }
                        },
                        flags: twizzler_rt_abi::fd::FdFlags::empty(),
                        id: name.id.raw(),
                        size: 0,
                        unix_mode: 0,
                        accessed: std::time::Duration::ZERO,
                        modified: std::time::Duration::ZERO,
                        created: std::time::Duration::ZERO,
                    }
                    .into(),
                )
            };
            buf[i] = ne;
        }
        Ok(end)
    }
}
