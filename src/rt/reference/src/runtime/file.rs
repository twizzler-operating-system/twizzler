use std::{
    ffi::c_void,
    io::{ErrorKind, Read, SeekFrom, Write},
    mem::ManuallyDrop,
    net::{Shutdown, SocketAddr},
    ops::Deref,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

use bitflags::bitflags;
use lazy_static::lazy_static;
use monitor_api::{get_comp_config, CompartmentHandle};
use naming_core::{
    dynamic::{dynamic_naming_factory, DynamicNamingHandle},
    GetFlags, NsNodeKind,
};
use raw_file::RawFile;
use socket::SocketKind;
use twizzler_abi::{
    object::{ObjID, Protections},
    syscall::{
        sys_object_create, BackingType, KernelConsoleSource, LifetimeType, ObjectCreate,
        ObjectCreateFlags,
    },
};
use twizzler_io::{
    pipe::Pipe,
    pty::{PtyClientHandle, PtyServerHandle, PtySignal},
};
use twizzler_rt_abi::{
    bindings::{
        binding_info, create_options, io_ctx, io_vec, object_bind_info, open_kind,
        open_kind_OpenKind_KernelConsole, open_kind_OpenKind_Path, BIND_DATA_MAX, FD_CMD_DUP,
    },
    error::{ArgumentError, GenericError, IoError, NamingError, ResourceError, TwzError},
    fd::{FdInfo, OpenKind, RawFd, SocketAddress},
    object::MapFlags,
    Result,
};

use super::ReferenceRuntime;
use crate::runtime::file::{compartment::CompartmentFile, pty::PtyHandleKind};

mod compartment;
mod file_desc;
mod pty;
mod raw_file;
mod socket;

#[derive(Clone)]
enum FdKind {
    //File(Arc<Mutex<FileDesc>>),
    RawFile(Arc<Mutex<RawFile>>),
    KernelConsole,
    Dir(ObjID),
    SymLink,
    Socket(SocketKind),
    Pty(PtyHandleKind),
    Pipe(Pipe),
    Compartment(CompartmentFile),
}

impl FdKind {
    fn seek(&mut self, pos: SeekFrom) -> Result<usize> {
        match self {
            //FdKind::File(arc) => arc.lock().unwrap().seek(pos),
            FdKind::RawFile(arc) => arc.lock().unwrap().seek(pos),
            _ => Err(GenericError::NotSupported.into()),
        }
    }

    fn stat(&mut self) -> Result<FdInfo> {
        match self {
            //FdKind::File(arc) => arc.lock().unwrap().stat(),
            FdKind::RawFile(arc) => arc.lock().unwrap().stat(),
            FdKind::Dir(id) => Ok(FdInfo {
                flags: twizzler_rt_abi::fd::FdFlags::from_bits_truncate(0),
                kind: twizzler_rt_abi::fd::FdKind::Directory,
                size: 0,
                id: id.raw(),
                created: Duration::from_secs(0).into(),
                modified: Duration::from_secs(0).into(),
                accessed: Duration::from_secs(0).into(),
                unix_mode: 0,
            }),
            FdKind::SymLink => Ok(FdInfo {
                flags: twizzler_rt_abi::fd::FdFlags::from_bits_truncate(0),
                kind: twizzler_rt_abi::fd::FdKind::SymLink,
                size: 0,
                id: 0,
                created: Duration::from_secs(0).into(),
                modified: Duration::from_secs(0).into(),
                accessed: Duration::from_secs(0).into(),
                unix_mode: 0,
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

    pub fn fd_cmd(&mut self, cmd: u32, arg: *const u8, _ret: *mut u8) -> Result<()> {
        match self {
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
}

impl Read for FdKind {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            //FdKind::File(arc) => arc.lock().unwrap().read(buf),
            FdKind::RawFile(arc) => arc.lock().unwrap().read(buf),
            FdKind::KernelConsole => {
                let len = twizzler_abi::syscall::sys_kernel_console_read(
                    KernelConsoleSource::Console,
                    buf,
                    twizzler_abi::syscall::KernelConsoleReadFlags::empty(),
                )
                //TODO
                .map_err(|_| ErrorKind::Other)?;
                Ok(len)
            }
            FdKind::Dir(_) => Err(ErrorKind::IsADirectory.into()),
            FdKind::SymLink => Err(ErrorKind::InvalidData.into()),
            FdKind::Socket(socket) => socket.read(buf),
            FdKind::Pty(pty) => pty.read(buf),
            FdKind::Pipe(pipe) => pipe.read(buf),
            FdKind::Compartment(comp) => comp.read(buf),
        }
    }
}

impl Write for FdKind {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
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
            FdKind::Dir(_) => Err(ErrorKind::IsADirectory.into()),
            FdKind::SymLink => Err(ErrorKind::InvalidData.into()),
            FdKind::Socket(socket) => socket.write(buf),
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
            FdKind::Dir(_) => Err(ErrorKind::IsADirectory.into()),
            FdKind::SymLink => Err(ErrorKind::InvalidData.into()),
            FdKind::Socket(socket) => socket.flush(),
            FdKind::Pty(pty) => pty.flush(),
            FdKind::Pipe(pipe) => pipe.flush(),
            FdKind::Compartment(comp) => comp.flush(),
        }
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
    kind: FdKind,
    binding: MaybeNoDrop<Arc<binding_info>>,
}

impl FileDesc {
    pub fn new(
        kind: FdKind,
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
            bind_len,
        };
        if let Some(bind_info) = bind_info {
            binding.bind_data[0..bind_len].copy_from_slice(&bind_info[0..bind_len])
        }
        FileDesc {
            kind,
            binding: MaybeNoDrop::new(Arc::new(binding), should_drop),
        }
    }

    pub fn seek(&mut self, pos: SeekFrom) -> Result<usize> {
        self.kind.seek(pos)
    }

    pub fn stat(&mut self) -> Result<FdInfo> {
        self.kind.stat()
    }

    pub fn fd_cmd(&mut self, cmd: u32, arg: *const u8, ret: *mut u8) -> Result<()> {
        self.kind.fd_cmd(cmd, arg, ret)
    }
}

impl Read for FileDesc {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.kind.read(buf)
    }
}

impl Write for FileDesc {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.kind.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.kind.flush()
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

fn get_fd_slots() -> &'static Mutex<FdSlots> {
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
            //twizzler_abi::klog_println!("binding fd {} as {:?}", bi.fd, kind);
            let _ = self.open(
                Some(bi.fd),
                kind,
                OperationOptions::from_bits_truncate(bi.flags),
                bi.bind_data.as_ptr().cast(),
                bi.bind_len,
                false,
            );
        }
    }

    fn open_path(
        &self,
        path: &str,
        create_opt: CreateOptions,
        open_opt: OperationOptions,
        bind_info: &[u8],
        should_drop: bool,
    ) -> Result<RawFd> {
        let mut session = get_naming_handle().lock().unwrap();

        if open_opt.contains(OperationOptions::OPEN_FLAG_TRUNCATE)
            && !open_opt.contains(OperationOptions::OPEN_FLAG_WRITE)
        {
            return Err(TwzError::INVALID_ARGUMENT);
        }
        let create = ObjectCreate::new(
            BackingType::Normal,
            LifetimeType::Persistent,
            None,
            ObjectCreateFlags::empty(),
            Protections::all(),
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
        let get_flags = if open_opt.contains(OperationOptions::OPEN_FLAG_SYMLINK) {
            GetFlags::empty()
        } else {
            GetFlags::FOLLOW_SYMLINK
        };
        let (obj_id, did_create, kind) = match create_opt {
            CreateOptions::UNEXPECTED => return Err(TwzError::INVALID_ARGUMENT),
            CreateOptions::CreateKindExisting => {
                let n = session.get(path, get_flags)?;
                (n.id, false, n.kind)
            }
            CreateOptions::CreateKindNew => {
                if session.get(path, GetFlags::empty()).is_ok() {
                    return Err(NamingError::AlreadyExists.into());
                }
                (
                    sys_object_create(create, &[], &[])?,
                    true,
                    NsNodeKind::Object,
                )
            }
            CreateOptions::CreateKindBind(id) => {
                if session.get(path, GetFlags::empty()).is_ok() {
                    return Err(NamingError::AlreadyExists.into());
                }
                (id, true, NsNodeKind::Object)
            }
            CreateOptions::CreateKindEither => session
                .get(path, get_flags)
                .map(|x| (ObjID::from(x.id), false, x.kind))
                .unwrap_or((
                    sys_object_create(create, &[], &[])?,
                    true,
                    NsNodeKind::Object,
                )),
        };

        let elem = match kind {
            NsNodeKind::Namespace => FdKind::Dir(obj_id),
            NsNodeKind::Object => {
                //if let Ok(elem) = FileDesc::open(&open_opt, obj_id, flags, &create_opt) {
                //    FdKind::File(Arc::new(Mutex::new(elem)))
                //} else {
                FdKind::RawFile(Arc::new(Mutex::new(RawFile::open(obj_id, flags)?)))
                //}
            }
            NsNodeKind::SymLink => FdKind::SymLink,
        };
        let elem = FileDesc::new(
            elem,
            open_kind_OpenKind_Path,
            0,
            Some(bind_info),
            should_drop,
        );

        let mut binding = get_fd_slots().lock().unwrap();

        let fd = binding
            .insert_first_empty(elem)
            .ok_or(ResourceError::OutOfNames)?;

        if did_create {
            session.put(path, obj_id)?;
        }

        drop(binding);
        if open_opt.contains(OperationOptions::OPEN_FLAG_TAIL) {
            self.seek(fd.try_into().unwrap(), SeekFrom::End(0))?;
        }

        Ok(fd.try_into().unwrap())
    }

    pub fn mkns(&self, name: &str) -> Result<()> {
        let mut session = get_naming_handle().lock().unwrap();

        session.put_namespace(name, true)?;
        Ok(())
    }

    pub fn symlink(&self, name: &str, target: &str) -> Result<()> {
        let mut session = get_naming_handle().lock().unwrap();

        session.symlink(name, target)?;
        Ok(())
    }

    pub fn readlink(&self, name: &str, target: &mut [u8], read_len: &mut u64) -> Result<()> {
        let mut session = get_naming_handle().lock().unwrap();
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
        let bind_info_bytes =
            unsafe { core::slice::from_raw_parts(bind_info.cast::<u8>(), bind_info_len) };
        let mut elem = match kind {
            OpenKind::Path => {
                let info = bind_info as *const twizzler_rt_abi::bindings::open_info;
                let info = unsafe { &*info };
                let name = &info.name[0..info.len];
                let name = core::str::from_utf8(name)
                    .map_err(|_| twizzler_rt_abi::error::ArgumentError::InvalidArgument)?;
                return self.open_path(
                    name,
                    info.create.into(),
                    info.flags.into(),
                    bind_info_bytes,
                    should_drop,
                );
            }
            OpenKind::PtyServer => {
                let id = bind_info as *const twizzler_rt_abi::bindings::object_bind_info;
                let id = unsafe { &*id };
                let pty = PtyHandleKind::Server(PtyServerHandle::new(
                    ObjID::new(id.id),
                    Some(pty_signal_handler),
                )?);
                FdKind::Pty(pty)
            }
            OpenKind::PtyClient => {
                let id = bind_info as *const twizzler_rt_abi::bindings::object_bind_info;
                let id = unsafe { &*id };
                let pty = PtyHandleKind::Client(PtyClientHandle::new(ObjID::new(id.id))?);
                FdKind::Pty(pty)
            }
            OpenKind::Pipe => {
                let id = bind_info as *const twizzler_rt_abi::bindings::object_bind_info;
                let id = unsafe { (*id).id };
                if id == 0 {
                    let pipe = twizzler_io::pipe::Pipe::create_object(ObjectCreate::default())?;
                    FdKind::Pipe(pipe)
                } else {
                    let pipe = twizzler_io::pipe::Pipe::open_object(id.into())?;
                    FdKind::Pipe(pipe)
                }
            }
            OpenKind::Compartment => {
                let id = bind_info as *const twizzler_rt_abi::bindings::object_bind_info;
                let id = unsafe { (*id).id };
                let comp = CompartmentHandle::lookup_id(id.into())?;
                FdKind::Compartment(CompartmentFile::new(comp))
            }
            OpenKind::SocketConnect => {
                let addr = bind_info as *const twizzler_rt_abi::bindings::socket_bind_info;
                let addr = unsafe { &*addr };
                FdKind::Socket(SocketKind::connect(SocketAddr::from(SocketAddress(
                    addr.addr,
                )))?)
            }
            OpenKind::SocketBind => {
                let addr = bind_info as *const twizzler_rt_abi::bindings::socket_bind_info;
                if addr.is_null() {
                    FdKind::Socket(SocketKind::None)
                } else {
                    let addr = unsafe { &*addr };
                    FdKind::Socket(SocketKind::bind(SocketAddr::from(SocketAddress(
                        addr.addr,
                    )))?)
                }
            }
            OpenKind::SocketAccept => {
                let fd_ptr = bind_info as *const RawFd;
                let fd = unsafe { *fd_ptr };
                let binding = get_fd_slots().lock().unwrap();
                let Some(fd) = binding.get(fd.try_into().unwrap()) else {
                    return Err(TwzError::INVALID_ARGUMENT);
                };

                let socket = match &fd.kind {
                    FdKind::Socket(socket) => socket.clone(),
                    _ => return Err(TwzError::INVALID_ARGUMENT),
                };
                drop(binding);

                FdKind::Socket(SocketKind::accept(&socket)?)
            }
            OpenKind::KernelConsole => FdKind::KernelConsole,
            _ => {
                return Err(TwzError::NOT_SUPPORTED);
            }
        };

        let elem = match elem {
            FdKind::Pipe(ref mut pipe) => {
                let binding_info = object_bind_info {
                    id: pipe.id().raw(),
                };
                let bind_info_bytes = unsafe {
                    core::slice::from_raw_parts(
                        &binding_info as *const object_bind_info as *const u8,
                        std::mem::size_of::<object_bind_info>(),
                    )
                };

                if !open_opt.contains(OperationOptions::OPEN_FLAG_READ) {
                    pipe.close_reader();
                }

                if !open_opt.contains(OperationOptions::OPEN_FLAG_WRITE) {
                    pipe.close_writer();
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
        let mut session = get_naming_handle().lock().unwrap();
        Ok(session.rename(old, new)?)
    }

    pub fn reopen_anon(
        &self,
        fd: RawFd,
        kind: OpenAnonKind,
        _open_opt: OperationOptions,
        bind_info: *mut c_void,
        _bind_info_len: usize,
        _prot: prot_kind,
    ) -> Result<()> {
        tracing::debug!("reopen_anon: {:?}", kind);
        let elem = match kind {
            OpenAnonKind::SocketConnect => {
                let addr = bind_info as *const twizzler_rt_abi::bindings::socket_address;
                let addr = unsafe { &*addr };
                FdKind::Socket(SocketKind::connect(SocketAddr::from(SocketAddress(*addr)))?)
            }
            OpenAnonKind::SocketBind => {
                let addr = bind_info as *const twizzler_rt_abi::bindings::socket_address;
                if addr.is_null() {
                    FdKind::Socket(SocketKind::None)
                } else {
                    let addr = unsafe { &*addr };
                    FdKind::Socket(SocketKind::bind(SocketAddr::from(SocketAddress(*addr)))?)
                }
            }
            _ => {
                return Err(TwzError::INVALID_ARGUMENT);
            }
        };

        let mut binding = get_fd_slots().lock().unwrap();
        let _ = binding
            .insert(fd.try_into().unwrap(), elem)
            .ok_or(ArgumentError::BadHandle)?;
        drop(binding);

        Ok(())
    }

    pub fn remove(&self, path: &str) -> Result<()> {
        let mut session = get_naming_handle().lock().unwrap();
        Ok(session.remove(path)?)
    }

    pub fn read(&self, fd: RawFd, buf: &mut [u8], _ctx: *mut io_ctx) -> Result<usize> {
        let binding = get_fd_slots().lock().unwrap();
        let mut file_desc = binding
            .get(fd.try_into().unwrap())
            .cloned()
            .ok_or(ArgumentError::BadHandle)?;
        drop(binding);

        file_desc.read(buf).map_err(|_| IoError::Other.into())
    }

    pub fn fd_pread(&self, fd: RawFd, buf: &mut [u8], ctx: *mut io_ctx) -> Result<usize> {
        self.read(fd, buf, ctx).map_err(|_| IoError::Other.into())
    }

    pub fn fd_pwrite(&self, fd: RawFd, buf: &[u8], ctx: *mut io_ctx) -> Result<usize> {
        self.write(fd, buf, ctx).map_err(|_| IoError::Other.into())
    }

    pub fn fd_pwritev(&self, _fd: RawFd, _buf: &[io_vec], _ctx: *mut io_ctx) -> Result<usize> {
        return Err(TwzError::NOT_SUPPORTED);
    }

    pub fn fd_preadv(&self, _fd: RawFd, _buf: &[io_vec], _ctx: *mut io_ctx) -> Result<usize> {
        return Err(TwzError::NOT_SUPPORTED);
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

        match &mut fd.kind {
            //FdKind::Socket(socket_kind) => todo!(),
            //FdKind::Pty(pty_handle_kind) => todo!(),
            //FdKind::Pipe(pipe) => todo!(),
            FdKind::Compartment(compartment_file) => {
                return compartment_file.get_config(reg, val, val_len);
            }
            _ => {}
        }

        let buf = unsafe { core::slice::from_raw_parts_mut(val.cast::<u8>(), val_len) };
        buf.fill(0);

        Ok(())
    }

    pub fn fd_set_config(
        &self,
        fd: RawFd,
        reg: u32,
        val: *const c_void,
        val_len: usize,
    ) -> Result<()> {
        twizzler_abi::klog_println!("==> set_config {} {}", fd, reg);
        let mut binding = get_fd_slots().lock().unwrap();
        let Some(fd) = binding.get_mut(fd.try_into().unwrap()) else {
            return Err(TwzError::INVALID_ARGUMENT);
        };

        match &mut fd.kind {
            //FdKind::Socket(socket_kind) => todo!(),
            //FdKind::Pty(pty_handle_kind) => todo!(),
            //FdKind::Pipe(pipe) => todo!(),
            FdKind::Compartment(compartment_file) => {
                return compartment_file.set_config(reg, val, val_len);
            }
            _ => {}
        }
        Ok(())
    }

    pub fn fd_cmd(&self, fd: RawFd, cmd: u32, arg: *const u8, ret: *mut u8) -> Result<()> {
        let mut binding = get_fd_slots().lock().unwrap();
        let file_desc = binding.get_mut(fd.try_into().unwrap());

        let file_desc = file_desc.ok_or(TwzError::INVALID_ARGUMENT)?;

        if cmd == FD_CMD_DUP {
            let nfd = file_desc.clone();
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

    pub fn write(&self, fd: RawFd, buf: &[u8], _ctx: *mut io_ctx) -> Result<usize> {
        let binding = get_fd_slots().lock().unwrap();
        let mut file_desc = binding
            .get(fd.try_into().unwrap())
            .cloned()
            .ok_or(ArgumentError::BadHandle)?;
        drop(binding);

        file_desc.write(buf).map_err(|_| IoError::Other.into())
    }

    pub fn close(&self, fd: RawFd) -> Option<()> {
        let file_desc = get_fd_slots()
            .lock()
            .unwrap()
            .remove(fd.try_into().unwrap())?;

        match &file_desc.kind {
            FdKind::Socket(socket_kind) => socket_kind.close().ok()?,
            _ => (),
        }

        Some(())
    }

    pub fn seek(&self, fd: RawFd, pos: SeekFrom) -> Result<usize> {
        let binding = get_fd_slots().lock().unwrap();
        let mut file_desc = binding
            .get(fd.try_into().unwrap())
            .cloned()
            .ok_or(ArgumentError::BadHandle)?;
        drop(binding);

        file_desc.seek(pos).map_err(|_| IoError::Other.into())
    }

    pub fn fd_enumerate(
        &self,
        fd: RawFd,
        buf: &mut [twizzler_rt_abi::fd::NameEntry],
        off: usize,
    ) -> Result<usize> {
        let stat = self.fd_get_info(fd).ok_or(ArgumentError::BadHandle)?;
        let mut session = get_naming_handle().lock().unwrap();
        let names = session.enumerate_names_nsid(stat.id.into())?;
        if off >= names.len() {
            return Ok(0);
        }
        let end = (off + buf.len()).min(names.len());
        let count = end - off;
        for i in 0..count {
            let name = &names[off + i];
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
        Ok(count)
    }
}
