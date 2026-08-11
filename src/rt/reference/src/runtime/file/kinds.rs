use std::{
    ffi::c_void,
    io::ErrorKind,
    net::SocketAddr,
    sync::{Arc, OnceLock},
};

use monitor_api::CompartmentHandle;
use naming_core::{GetFlags, NsNodeKind};
use secgate::TwzError;
use twizzler_abi::{object::ObjID, syscall::ObjectCreate};
use twizzler_io::pty::{PtyClientHandle, PtyServerHandle};
use twizzler_rt_abi::{
    bindings::{open_info, prot_kind_ProtKind_Stream},
    error::NamingError,
    fd::{OpenKind, RawFd, SocketAddress},
    object::MapFlags,
    Result,
};

use crate::runtime::file::{
    get_fd_slots, get_naming_handle,
    kinds::{
        compartment::CompartmentFile, dir::DirFile, kconsole::KernelConsoleFile,
        pty::PtyHandleKind, raw_file::RawFile, socket::SocketKind, symlink::SymLinkFile,
    },
    pty_signal_handler, CreateOptions, FdImpl, OperationOptions,
};

pub mod compartment;
pub mod dir;
pub mod kconsole;
pub mod pty;
pub mod raw_file;
pub mod socket;
pub mod symlink;

fn binding_ref<'a, T>(binding: *const c_void, binding_len: usize) -> std::io::Result<&'a T> {
    if std::mem::size_of::<T>() <= binding_len {
        Ok(unsafe { &*binding.cast::<T>() })
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "binding size is too small",
        ))
    }
}

// Temporary instrumentation for the File::open latency hunt (pagerperf.md).
mod openstats {
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNT: AtomicU64 = AtomicU64::new(0);
    static LOCK: AtomicU64 = AtomicU64::new(0);
    static GET: AtomicU64 = AtomicU64::new(0);
    static OBJ: AtomicU64 = AtomicU64::new(0);

    pub fn record(lock: u64, get: u64, obj: u64) {
        let n = COUNT.fetch_add(1, Ordering::Relaxed) + 1;
        let l = LOCK.fetch_add(lock, Ordering::Relaxed) + lock;
        let g = GET.fetch_add(get, Ordering::Relaxed) + get;
        let o = OBJ.fetch_add(obj, Ordering::Relaxed) + obj;
        if n.is_power_of_two() {
            twizzler_abi::klog_println!(
                "OPENSTATS {} opens: lock {} us, get {} us, obj {} us (per open: {} us)",
                n,
                l / 1000,
                g / 1000,
                o / 1000,
                (l + g + o) / (n * 1000),
            );
        }
    }
}

fn open_path(path: &str, create_opt: CreateOptions, open_opt: OperationOptions) -> Result<FdImpl> {
    let t_start = std::time::Instant::now();
    let mut session = get_naming_handle()
        .ok_or(TwzError::NOT_SUPPORTED)?;
    let lock_ns = t_start.elapsed().as_nanos() as u64;

    if open_opt.contains(OperationOptions::OPEN_FLAG_TRUNCATE)
        && !open_opt.contains(OperationOptions::OPEN_FLAG_WRITE)
    {
        return Err(TwzError::INVALID_ARGUMENT);
    }
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
    let t_get = std::time::Instant::now();
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
            let node = session.get(path, GetFlags::CREATE)?;
            (node.id, false, NsNodeKind::Object)
        }
        CreateOptions::CreateKindBind(id) => {
            if session.get(path, GetFlags::empty()).is_ok() {
                return Err(NamingError::AlreadyExists.into());
            }
            (id, true, NsNodeKind::Object)
        }
        CreateOptions::CreateKindEither => session
            .get(path, get_flags | GetFlags::CREATE)
            .map(|x| (ObjID::from(x.id), false, x.kind))?,
    };

    let get_ns = t_get.elapsed().as_nanos() as u64;

    tracing::trace!(
        "open_path: path = {}, obj_id = {:x}, did_create = {}, kind = {:?}",
        path,
        obj_id,
        did_create,
        kind
    );
    if did_create {
        session.put(path, obj_id)?;
    }

    let t_obj = std::time::Instant::now();
    let res = match kind {
        NsNodeKind::Namespace => Arc::new(DirFile::new(obj_id)?) as FdImpl,
        NsNodeKind::Object => {
            let file = RawFile::open(obj_id, flags)?;
            if open_opt.contains(OperationOptions::OPEN_FLAG_TRUNCATE) {
                file.truncate(0)?;
            }
            Arc::new(file)
        }
        NsNodeKind::SymLink => Arc::new(SymLinkFile::new(obj_id)?),
    };
    openstats::record(lock_ns, get_ns, t_obj.elapsed().as_nanos() as u64);

    Ok(res)
}

pub fn open(
    existing_fd: Option<RawFd>,
    kind: OpenKind,
    binding: *const c_void,
    binding_len: usize,
    opts: OperationOptions,
) -> Result<Option<FdImpl>> {
    Ok(match kind {
        OpenKind::Path => {
            let info = binding_ref::<open_info>(binding, binding_len)?;
            let name = &info.name[0..info.len];
            let name = core::str::from_utf8(name).map_err(|_| ErrorKind::InvalidInput)?;
            open_path(name, info.create.into(), opts).map(Some)?
        }
        OpenKind::PtyServer => {
            let id =
                binding_ref::<twizzler_rt_abi::bindings::object_bind_info>(binding, binding_len)?;
            let pty = PtyHandleKind::Server(PtyServerHandle::new(
                ObjID::new(id.id),
                Some(pty_signal_handler),
            )?);
            Some(Arc::new(pty))
        }
        OpenKind::PtyClient => {
            let id =
                binding_ref::<twizzler_rt_abi::bindings::object_bind_info>(binding, binding_len)?;
            let pty = PtyHandleKind::Client(PtyClientHandle::new(ObjID::new(id.id))?);
            Some(Arc::new(pty))
        }
        OpenKind::Pipe => {
            let id =
                binding_ref::<twizzler_rt_abi::bindings::object_bind_info>(binding, binding_len)
                    .map(|b| b.id)
                    .unwrap_or(0);
            let pipe = if id == 0 {
                twizzler_io::pipe::Pipe::create_object(ObjectCreate::default())?
            } else {
                twizzler_io::pipe::Pipe::open_object(id.into())?
            };
            Some(Arc::new(pipe))
        }
        OpenKind::Compartment => {
            let id =
                binding_ref::<twizzler_rt_abi::bindings::object_bind_info>(binding, binding_len)?;
            let id = id.id;
            let comp = CompartmentHandle::lookup_id(id.into())?;
            Some(Arc::new(CompartmentFile::new(comp)?))
        }
        OpenKind::SocketConnect => {
            let addr =
                binding_ref::<twizzler_rt_abi::bindings::socket_bind_info>(binding, binding_len)?;
            if addr.prot == prot_kind_ProtKind_Stream {
                Some(Arc::new(SocketKind::connect(SocketAddr::from(
                    SocketAddress(addr.addr),
                ))?))
            } else {
                let binding = get_fd_slots().lock().unwrap();
                let Some(fd) = binding.get(existing_fd.unwrap() as usize) else {
                    return Err(TwzError::INVALID_ARGUMENT);
                };

                let file = fd.file.clone();
                let downcast = file.as_socket();
                let Some(socket) = downcast else {
                    tracing::warn!("tried to connect from a non-socket fd");
                    return Err(TwzError::INVALID_ARGUMENT);
                };
                socket.udp_connect(SocketAddr::from(SocketAddress(addr.addr)))?;

                drop(binding);
                None
            }
        }
        OpenKind::SocketBind => {
            match binding_ref::<twizzler_rt_abi::bindings::socket_bind_info>(binding, binding_len) {
                Err(_) => {
                    // If we can't read the bind info, treat it as a "no bind info" case and create
                    // an unbound socket
                    Some(Arc::new(SocketKind::UdpSocket(OnceLock::new())))
                }
                Ok(addr) => {
                    if addr.prot == prot_kind_ProtKind_Stream {
                        Some(Arc::new(SocketKind::bind(SocketAddr::from(
                            SocketAddress(addr.addr),
                        ))?))
                    } else {
                        Some(Arc::new(SocketKind::udp_bind(SocketAddr::from(
                            SocketAddress(addr.addr),
                        ))?))
                    }
                }
            }
        }
        OpenKind::SocketAccept => {
            let fd =
                binding_ref::<twizzler_rt_abi::bindings::object_bind_info>(binding, binding_len)
                    .inspect_err(|e| {
                        twizzler_abi::klog_println!(
                            "SocketAccept: binding_ref failed (binding_len = {}): {}",
                            binding_len,
                            e
                        );
                    })?;
            let fd = fd.id as usize;
            let binding = get_fd_slots().lock().unwrap();
            let Some(fd) = binding.get(fd) else {
                twizzler_abi::klog_println!("SocketAccept: no fd slot at index {}", fd);
                return Err(ErrorKind::InvalidInput.into());
            };

            let file = fd.file.clone();
            let downcast = file.as_socket();
            let Some(socket) = downcast else {
                tracing::warn!("tried to accept on a non-socket fd");
                twizzler_abi::klog_println!("SocketAccept: fd is not a socket");
                return Err(ErrorKind::InvalidInput.into());
            };
            drop(binding);

            Some(Arc::new(SocketKind::accept(&socket).inspect_err(|e| {
                twizzler_abi::klog_println!("SocketAccept: SocketKind::accept failed: {}", e);
            })?))
        }
        OpenKind::KernelConsole => Some(Arc::new(KernelConsoleFile::new())),
        OpenKind::Kqueue => Some(Arc::new(super::KqueueFile::new())),
        _ => Err(ErrorKind::Unsupported)?,
    })
}
