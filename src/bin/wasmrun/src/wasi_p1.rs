//! WASI Preview 1 (wasi_snapshot_preview1) for Twizzler — synchronous implementation.
//!
//! Implements the classic WASI P1 function-level API for core WASM modules.
//! This complements the P2 component model support in wasi.rs.

use anyhow::{bail, Result};
use core::mem::MaybeUninit;

use twizzler_abi::syscall::{
    sys_get_random, sys_kernel_console_read, sys_kernel_console_write, GetRandomFlags,
    KernelConsoleReadFlags, KernelConsoleSource, KernelConsoleWriteFlags,
};
use twizzler_rt_abi::fd::{self, FdKind, NameEntry, RawFd};
use twizzler_rt_abi::io::{IoCtx, IoFlags};

use wasmtime::{Caller, Engine, Linker, Memory, Module, Store};

use crate::net;
use smoltcp::wire::{IpAddress, Ipv4Address};

// ── WASI errno codes ────────────────────────────────────────────────

const ERRNO_SUCCESS: i32 = 0;
const ERRNO_BADF: i32 = 8;
const ERRNO_EXIST: i32 = 20;
const ERRNO_FAULT: i32 = 21;
const ERRNO_INVAL: i32 = 28;
const ERRNO_IO: i32 = 29;
const ERRNO_ISDIR: i32 = 31;
const ERRNO_LOOP: i32 = 32;
const ERRNO_NOENT: i32 = 44;
const ERRNO_NOTDIR: i32 = 54;
const ERRNO_NOTEMPTY: i32 = 55;
const ERRNO_NOTSUP: i32 = 58;
const ERRNO_CONNREFUSED: i32 = 14;
const ERRNO_CONNRESET: i32 = 15;
const ERRNO_NOTCONN: i32 = 53;
const ERRNO_ADDRINUSE: i32 = 3;
const ERRNO_ADDRNOTAVAIL: i32 = 4;
const ERRNO_AGAIN: i32 = 6;

// ── WASI file types ─────────────────────────────────────────────────

const FILETYPE_UNKNOWN: u8 = 0;
const FILETYPE_CHARACTER_DEVICE: u8 = 2;
const FILETYPE_DIRECTORY: u8 = 3;
const FILETYPE_REGULAR_FILE: u8 = 4;
const FILETYPE_SOCKET_DGRAM: u8 = 5;
const FILETYPE_SOCKET_STREAM: u8 = 6;
const FILETYPE_SYMBOLIC_LINK: u8 = 7;

// ── WASI constants ──────────────────────────────────────────────────

const CLOCKID_REALTIME: i32 = 0;
const CLOCKID_MONOTONIC: i32 = 1;

const WHENCE_SET: i32 = 0;
const WHENCE_CUR: i32 = 1;
const WHENCE_END: i32 = 2;

const OFLAGS_CREAT: i32 = 1;
const OFLAGS_DIRECTORY: i32 = 2;
const OFLAGS_EXCL: i32 = 4;
const OFLAGS_TRUNC: i32 = 8;

const FDFLAGS_APPEND: i32 = 1;

const PREOPENTYPE_DIR: u8 = 0;

const RIGHTS_ALL: u64 = u64::MAX;

// ── Fd table ────────────────────────────────────────────────────────

enum P1Fd {
    Stdin,
    Stdout,
    Stderr,
    Dir { twz_fd: RawFd, path: String },
    File { twz_fd: RawFd, position: u64, append: bool },
    TcpUnbound,
    TcpBound { addr: net::NetAddr },
    TcpSocket { socket: net::NetSocket },
    TcpListener { listener: net::NetListener },
    UdpUnbound,
    UdpBound { socket: net::NetUdpSocket },
}

impl P1Fd {
    fn filetype(&self) -> u8 {
        match self {
            P1Fd::Stdin | P1Fd::Stdout | P1Fd::Stderr => FILETYPE_CHARACTER_DEVICE,
            P1Fd::Dir { .. } => FILETYPE_DIRECTORY,
            P1Fd::File { .. } => FILETYPE_REGULAR_FILE,
            P1Fd::TcpUnbound | P1Fd::TcpBound { .. } | P1Fd::TcpSocket { .. }
            | P1Fd::TcpListener { .. } => FILETYPE_SOCKET_STREAM,
            P1Fd::UdpUnbound | P1Fd::UdpBound { .. } => FILETYPE_SOCKET_DGRAM,
        }
    }

    fn twz_fd(&self) -> Option<RawFd> {
        match self {
            P1Fd::Dir { twz_fd, .. } | P1Fd::File { twz_fd, .. } => Some(*twz_fd),
            _ => None,
        }
    }
}

impl Drop for P1Fd {
    fn drop(&mut self) {
        if let Some(raw) = self.twz_fd() {
            fd::twz_rt_fd_close(raw);
        }
    }
}

// ── WASI P1 context ─────────────────────────────────────────────────

pub struct WasiP1Ctx {
    fds: Vec<Option<P1Fd>>,
    preopen_fds: Vec<(i32, String)>,
}

impl WasiP1Ctx {
    fn new() -> Self {
        let mut ctx = WasiP1Ctx {
            fds: Vec::new(),
            preopen_fds: Vec::new(),
        };

        ctx.fds.push(Some(P1Fd::Stdin));
        ctx.fds.push(Some(P1Fd::Stdout));
        ctx.fds.push(Some(P1Fd::Stderr));

        let create = twizzler_rt_abi::bindings::create_options {
            id: Default::default(),
            kind: twizzler_rt_abi::bindings::CREATE_KIND_EXISTING,
        };
        if let Ok(raw_fd) =
            fd::twz_rt_fd_open("/", create, twizzler_rt_abi::bindings::OPEN_FLAG_READ)
        {
            ctx.fds.push(Some(P1Fd::Dir {
                twz_fd: raw_fd,
                path: "/".to_string(),
            }));
            ctx.preopen_fds.push((3, "/".to_string()));
        }

        ctx
    }

    fn alloc_fd(&mut self, entry: P1Fd) -> i32 {
        for (i, slot) in self.fds.iter_mut().enumerate() {
            if slot.is_none() {
                *slot = Some(entry);
                return i as i32;
            }
        }
        let fd_num = self.fds.len() as i32;
        self.fds.push(Some(entry));
        fd_num
    }

    fn get_fd(&self, fd: i32) -> Option<&P1Fd> {
        self.fds.get(fd as usize).and_then(|f| f.as_ref())
    }

    fn close_fd(&mut self, fd: i32) {
        if let Some(slot) = self.fds.get_mut(fd as usize) {
            *slot = None;
        }
    }
}

// ── Helpers ─────────────────────────────────────────────────────────

struct Iov {
    buf_ptr: u32,
    buf_len: u32,
}

fn read_iovs(
    mem: &Memory,
    caller: &mut Caller<'_, WasiP1Ctx>,
    iovs_ptr: i32,
    iovs_len: i32,
) -> Option<Vec<Iov>> {
    let mut iovs = Vec::with_capacity(iovs_len as usize);
    for i in 0..iovs_len {
        let offset = (iovs_ptr + i * 8) as usize;
        let mut bytes = [0u8; 8];
        mem.read(&*caller, offset, &mut bytes).ok()?;
        iovs.push(Iov {
            buf_ptr: u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            buf_len: u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
        });
    }
    Some(iovs)
}

fn guest_string(
    mem: &Memory,
    caller: &mut Caller<'_, WasiP1Ctx>,
    ptr: i32,
    len: i32,
) -> Option<String> {
    let mut buf = vec![0u8; len as usize];
    mem.read(&*caller, ptr as usize, &mut buf).ok()?;
    String::from_utf8(buf).ok()
}

fn twz_err_to_errno(e: twizzler_rt_abi::error::TwzError) -> i32 {
    use twizzler_rt_abi::error::*;
    match e {
        TwzError::Naming(NamingError::NotFound) => ERRNO_NOENT,
        TwzError::Naming(NamingError::AlreadyExists) => ERRNO_EXIST,
        TwzError::Naming(NamingError::NotEmpty) => ERRNO_NOTEMPTY,
        TwzError::Naming(NamingError::WrongNameKind) => ERRNO_NOTDIR,
        TwzError::Naming(NamingError::LinkLoop) => ERRNO_LOOP,
        TwzError::Argument(ArgumentError::InvalidArgument) => ERRNO_INVAL,
        TwzError::Argument(ArgumentError::BadHandle) => ERRNO_BADF,
        _ => ERRNO_IO,
    }
}

fn fd_kind_to_filetype(kind: FdKind) -> u8 {
    match kind {
        FdKind::Regular => FILETYPE_REGULAR_FILE,
        FdKind::Directory => FILETYPE_DIRECTORY,
        FdKind::SymLink => FILETYPE_SYMBOLIC_LINK,
        FdKind::Other => FILETYPE_UNKNOWN,
    }
}

fn join_path(base: &str, name: &str) -> String {
    if base.ends_with('/') {
        format!("{base}{name}")
    } else {
        format!("{base}/{name}")
    }
}

fn get_mem(caller: &mut Caller<'_, WasiP1Ctx>) -> Option<Memory> {
    match caller.get_export("memory") {
        Some(wasmtime::Extern::Memory(m)) => Some(m),
        _ => None,
    }
}

// ── Socket helpers ──────────────────────────────────────────────────

/// Convert a NetError to a WASI P1 errno code.
fn net_err_to_errno(e: net::NetError) -> i32 {
    match e {
        net::NetError::WouldBlock => ERRNO_AGAIN,
        net::NetError::ConnectionRefused => ERRNO_CONNREFUSED,
        net::NetError::ConnectionReset => ERRNO_CONNRESET,
        net::NetError::NotConnected => ERRNO_NOTCONN,
        net::NetError::AddrInUse => ERRNO_ADDRINUSE,
        net::NetError::AddrNotAvailable => ERRNO_ADDRNOTAVAIL,
        net::NetError::InvalidArgument => ERRNO_INVAL,
        net::NetError::NotSupported => ERRNO_NOTSUP,
        net::NetError::PortExhaustion => ERRNO_ADDRINUSE,
        net::NetError::Other(_) => ERRNO_IO,
    }
}

/// Read a sockaddr from guest memory. Format: u16 family (LE) + u16 port (BE) + [u8;4] IPv4 addr.
fn read_sockaddr(
    mem: &Memory,
    caller: &mut Caller<'_, WasiP1Ctx>,
    ptr: i32,
    len: i32,
) -> Option<net::NetAddr> {
    if len < 8 {
        return None;
    }
    let mut buf = [0u8; 8];
    mem.read(&*caller, ptr as usize, &mut buf).ok()?;
    let family = u16::from_le_bytes([buf[0], buf[1]]);
    if family != 2 {
        return None; // AF_INET only
    }
    let port = u16::from_be_bytes([buf[2], buf[3]]);
    let ip = Ipv4Address::new(buf[4], buf[5], buf[6], buf[7]);
    Some(net::NetAddr {
        ip: ip.into(),
        port,
    })
}

/// Write a sockaddr to guest memory. Returns true on success.
fn write_sockaddr(
    mem: &Memory,
    caller: &mut Caller<'_, WasiP1Ctx>,
    ptr: i32,
    len_ptr: i32,
    addr: &net::NetAddr,
) -> bool {
    let mut buf = [0u8; 8];
    buf[0..2].copy_from_slice(&2u16.to_le_bytes()); // AF_INET
    buf[2..4].copy_from_slice(&addr.port.to_be_bytes());
    if let IpAddress::Ipv4(v4) = addr.ip {
        buf[4..8].copy_from_slice(&v4.0);
    }
    mem.write(&mut *caller, ptr as usize, &buf).is_ok()
        && mem
            .write(&mut *caller, len_ptr as usize, &8u32.to_le_bytes())
            .is_ok()
}

// ── Linker setup ────────────────────────────────────────────────────

fn add_wasi_p1_to_linker(linker: &mut Linker<WasiP1Ctx>) -> Result<()> {
    let ns = "wasi_snapshot_preview1";

    // ── args ────────────────────────────────────────────────────

    linker.func_wrap(
        ns,
        "args_sizes_get",
        |mut caller: Caller<'_, WasiP1Ctx>, argc_ptr: i32, buf_size_ptr: i32| -> i32 {
            let mem = match get_mem(&mut caller) {
                Some(m) => m,
                None => return ERRNO_IO,
            };
            let args: Vec<String> = std::env::args().collect();
            let argc = args.len() as u32;
            let buf_size: u32 = args.iter().map(|a| a.len() as u32 + 1).sum();
            if mem
                .write(&mut caller, argc_ptr as usize, &argc.to_le_bytes())
                .is_err()
            {
                return ERRNO_FAULT;
            }
            if mem
                .write(
                    &mut caller,
                    buf_size_ptr as usize,
                    &buf_size.to_le_bytes(),
                )
                .is_err()
            {
                return ERRNO_FAULT;
            }
            ERRNO_SUCCESS
        },
    )?;

    linker.func_wrap(
        ns,
        "args_get",
        |mut caller: Caller<'_, WasiP1Ctx>, argv_ptr: i32, argv_buf_ptr: i32| -> i32 {
            let mem = match get_mem(&mut caller) {
                Some(m) => m,
                None => return ERRNO_IO,
            };
            let args: Vec<String> = std::env::args().collect();
            let mut buf_offset = argv_buf_ptr as u32;
            for (i, arg) in args.iter().enumerate() {
                let ptr_offset = (argv_ptr as u32) + (i as u32) * 4;
                if mem
                    .write(
                        &mut caller,
                        ptr_offset as usize,
                        &buf_offset.to_le_bytes(),
                    )
                    .is_err()
                {
                    return ERRNO_FAULT;
                }
                let mut bytes = arg.as_bytes().to_vec();
                bytes.push(0);
                if mem
                    .write(&mut caller, buf_offset as usize, &bytes)
                    .is_err()
                {
                    return ERRNO_FAULT;
                }
                buf_offset += bytes.len() as u32;
            }
            ERRNO_SUCCESS
        },
    )?;

    // ── environ ─────────────────────────────────────────────────

    linker.func_wrap(
        ns,
        "environ_sizes_get",
        |mut caller: Caller<'_, WasiP1Ctx>, count_ptr: i32, buf_size_ptr: i32| -> i32 {
            let mem = match get_mem(&mut caller) {
                Some(m) => m,
                None => return ERRNO_IO,
            };
            let env: Vec<(String, String)> = std::env::vars().collect();
            let count = env.len() as u32;
            let buf_size: u32 = env
                .iter()
                .map(|(k, v)| k.len() as u32 + 1 + v.len() as u32 + 1)
                .sum();
            if mem
                .write(&mut caller, count_ptr as usize, &count.to_le_bytes())
                .is_err()
            {
                return ERRNO_FAULT;
            }
            if mem
                .write(
                    &mut caller,
                    buf_size_ptr as usize,
                    &buf_size.to_le_bytes(),
                )
                .is_err()
            {
                return ERRNO_FAULT;
            }
            ERRNO_SUCCESS
        },
    )?;

    linker.func_wrap(
        ns,
        "environ_get",
        |mut caller: Caller<'_, WasiP1Ctx>, environ_ptr: i32, environ_buf_ptr: i32| -> i32 {
            let mem = match get_mem(&mut caller) {
                Some(m) => m,
                None => return ERRNO_IO,
            };
            let env: Vec<(String, String)> = std::env::vars().collect();
            let mut buf_offset = environ_buf_ptr as u32;
            for (i, (k, v)) in env.iter().enumerate() {
                let ptr_offset = (environ_ptr as u32) + (i as u32) * 4;
                if mem
                    .write(
                        &mut caller,
                        ptr_offset as usize,
                        &buf_offset.to_le_bytes(),
                    )
                    .is_err()
                {
                    return ERRNO_FAULT;
                }
                let entry = format!("{k}={v}\0");
                if mem
                    .write(&mut caller, buf_offset as usize, entry.as_bytes())
                    .is_err()
                {
                    return ERRNO_FAULT;
                }
                buf_offset += entry.len() as u32;
            }
            ERRNO_SUCCESS
        },
    )?;

    // ── clocks ──────────────────────────────────────────────────

    linker.func_wrap(
        ns,
        "clock_res_get",
        |mut caller: Caller<'_, WasiP1Ctx>, _clock_id: i32, resolution_ptr: i32| -> i32 {
            let mem = match get_mem(&mut caller) {
                Some(m) => m,
                None => return ERRNO_IO,
            };
            let res: u64 = 1;
            if mem
                .write(&mut caller, resolution_ptr as usize, &res.to_le_bytes())
                .is_err()
            {
                return ERRNO_FAULT;
            }
            ERRNO_SUCCESS
        },
    )?;

    linker.func_wrap(
        ns,
        "clock_time_get",
        |mut caller: Caller<'_, WasiP1Ctx>,
         clock_id: i32,
         _precision: i64,
         time_ptr: i32|
         -> i32 {
            let mem = match get_mem(&mut caller) {
                Some(m) => m,
                None => return ERRNO_IO,
            };
            let nanos = match clock_id {
                CLOCKID_REALTIME => {
                    twizzler_rt_abi::time::twz_rt_get_system_time().as_nanos() as u64
                }
                CLOCKID_MONOTONIC => {
                    twizzler_rt_abi::time::twz_rt_get_monotonic_time().as_nanos() as u64
                }
                _ => return ERRNO_INVAL,
            };
            if mem
                .write(&mut caller, time_ptr as usize, &nanos.to_le_bytes())
                .is_err()
            {
                return ERRNO_FAULT;
            }
            ERRNO_SUCCESS
        },
    )?;

    // ── random ──────────────────────────────────────────────────

    linker.func_wrap(
        ns,
        "random_get",
        |mut caller: Caller<'_, WasiP1Ctx>, buf_ptr: i32, buf_len: i32| -> i32 {
            let mem = match get_mem(&mut caller) {
                Some(m) => m,
                None => return ERRNO_IO,
            };
            let mut buf = vec![0u8; buf_len as usize];
            let dest = unsafe {
                core::slice::from_raw_parts_mut(
                    buf.as_mut_ptr() as *mut MaybeUninit<u8>,
                    buf.len(),
                )
            };
            if sys_get_random(dest, GetRandomFlags::empty()).is_err() {
                return ERRNO_IO;
            }
            if mem
                .write(&mut caller, buf_ptr as usize, &buf)
                .is_err()
            {
                return ERRNO_FAULT;
            }
            ERRNO_SUCCESS
        },
    )?;

    // ── proc / sched ────────────────────────────────────────────

    linker.func_wrap(
        ns,
        "proc_exit",
        |_caller: Caller<'_, WasiP1Ctx>, code: i32| -> Result<()> {
            if code == 0 {
                bail!("wasi exit success")
            } else {
                bail!("wasi exit with code {code}")
            }
        },
    )?;

    linker.func_wrap(
        ns,
        "proc_raise",
        |_caller: Caller<'_, WasiP1Ctx>, _sig: i32| -> i32 { ERRNO_NOTSUP },
    )?;

    linker.func_wrap(
        ns,
        "sched_yield",
        |_caller: Caller<'_, WasiP1Ctx>| -> i32 { ERRNO_SUCCESS },
    )?;

    // ── fd_prestat ──────────────────────────────────────────────

    linker.func_wrap(
        ns,
        "fd_prestat_get",
        |mut caller: Caller<'_, WasiP1Ctx>, fd: i32, prestat_ptr: i32| -> i32 {
            let mem = match get_mem(&mut caller) {
                Some(m) => m,
                None => return ERRNO_IO,
            };
            let path_len = match caller
                .data()
                .preopen_fds
                .iter()
                .find(|(f, _)| *f == fd)
            {
                Some((_, path)) => path.len() as u32,
                None => return ERRNO_BADF,
            };
            // prestat: u8 tag + 3 padding + u32 name_len = 8 bytes
            let mut buf = [0u8; 8];
            buf[0] = PREOPENTYPE_DIR;
            buf[4..8].copy_from_slice(&path_len.to_le_bytes());
            if mem
                .write(&mut caller, prestat_ptr as usize, &buf)
                .is_err()
            {
                return ERRNO_FAULT;
            }
            ERRNO_SUCCESS
        },
    )?;

    linker.func_wrap(
        ns,
        "fd_prestat_dir_name",
        |mut caller: Caller<'_, WasiP1Ctx>,
         fd: i32,
         path_ptr: i32,
         path_len: i32|
         -> i32 {
            let mem = match get_mem(&mut caller) {
                Some(m) => m,
                None => return ERRNO_IO,
            };
            let path = match caller
                .data()
                .preopen_fds
                .iter()
                .find(|(f, _)| *f == fd)
            {
                Some((_, path)) => path.clone(),
                None => return ERRNO_BADF,
            };
            let bytes = path.as_bytes();
            let write_len = (path_len as usize).min(bytes.len());
            if mem
                .write(&mut caller, path_ptr as usize, &bytes[..write_len])
                .is_err()
            {
                return ERRNO_FAULT;
            }
            ERRNO_SUCCESS
        },
    )?;

    // ── fd_fdstat_get ───────────────────────────────────────────

    linker.func_wrap(
        ns,
        "fd_fdstat_get",
        |mut caller: Caller<'_, WasiP1Ctx>, fd: i32, stat_ptr: i32| -> i32 {
            let mem = match get_mem(&mut caller) {
                Some(m) => m,
                None => return ERRNO_IO,
            };
            let filetype = match caller.data().get_fd(fd) {
                Some(entry) => entry.filetype(),
                None => return ERRNO_BADF,
            };
            // fdstat: u8 filetype + 1pad + u16 flags + 4pad + u64 rights + u64 rights = 24
            let mut buf = [0u8; 24];
            buf[0] = filetype;
            buf[8..16].copy_from_slice(&RIGHTS_ALL.to_le_bytes());
            buf[16..24].copy_from_slice(&RIGHTS_ALL.to_le_bytes());
            if mem
                .write(&mut caller, stat_ptr as usize, &buf)
                .is_err()
            {
                return ERRNO_FAULT;
            }
            ERRNO_SUCCESS
        },
    )?;

    // ── fd_filestat_get ─────────────────────────────────────────

    linker.func_wrap(
        ns,
        "fd_filestat_get",
        |mut caller: Caller<'_, WasiP1Ctx>, fd: i32, stat_ptr: i32| -> i32 {
            let mem = match get_mem(&mut caller) {
                Some(m) => m,
                None => return ERRNO_IO,
            };
            let (filetype, twz_fd_opt) = match caller.data().get_fd(fd) {
                Some(P1Fd::Stdin) | Some(P1Fd::Stdout) | Some(P1Fd::Stderr) => {
                    (FILETYPE_CHARACTER_DEVICE, None)
                }
                Some(P1Fd::Dir { twz_fd, .. }) => (FILETYPE_DIRECTORY, Some(*twz_fd)),
                Some(P1Fd::File { twz_fd, .. }) => (FILETYPE_REGULAR_FILE, Some(*twz_fd)),
                Some(P1Fd::TcpUnbound | P1Fd::TcpBound { .. } | P1Fd::TcpSocket { .. } | P1Fd::TcpListener { .. }) => {
                    (FILETYPE_SOCKET_STREAM, None)
                }
                Some(P1Fd::UdpUnbound | P1Fd::UdpBound { .. }) => {
                    (FILETYPE_SOCKET_DGRAM, None)
                }
                None => return ERRNO_BADF,
            };
            let (size, atim, mtim, ctim) = if let Some(raw) = twz_fd_opt {
                match fd::twz_rt_fd_get_info(raw) {
                    Ok(info) => (
                        info.size,
                        info.accessed.as_nanos() as u64,
                        info.modified.as_nanos() as u64,
                        info.created.as_nanos() as u64,
                    ),
                    Err(_) => (0, 0, 0, 0),
                }
            } else {
                (0, 0, 0, 0)
            };
            // filestat: 64 bytes
            let mut buf = [0u8; 64];
            buf[16] = filetype;
            buf[24..32].copy_from_slice(&1u64.to_le_bytes()); // nlink
            buf[32..40].copy_from_slice(&size.to_le_bytes());
            buf[40..48].copy_from_slice(&atim.to_le_bytes());
            buf[48..56].copy_from_slice(&mtim.to_le_bytes());
            buf[56..64].copy_from_slice(&ctim.to_le_bytes());
            if mem
                .write(&mut caller, stat_ptr as usize, &buf)
                .is_err()
            {
                return ERRNO_FAULT;
            }
            ERRNO_SUCCESS
        },
    )?;

    // ── fd_read ─────────────────────────────────────────────────

    linker.func_wrap(
        ns,
        "fd_read",
        |mut caller: Caller<'_, WasiP1Ctx>,
         fd: i32,
         iovs_ptr: i32,
         iovs_len: i32,
         nread_ptr: i32|
         -> i32 {
            let mem = match get_mem(&mut caller) {
                Some(m) => m,
                None => return ERRNO_IO,
            };
            let iovs = match read_iovs(&mem, &mut caller, iovs_ptr, iovs_len) {
                Some(v) => v,
                None => return ERRNO_FAULT,
            };
            let fd_info = match caller.data().get_fd(fd) {
                Some(P1Fd::Stdin) => (true, None),
                Some(P1Fd::File {
                    twz_fd, position, ..
                }) => (false, Some((*twz_fd, *position))),
                Some(P1Fd::Dir { .. }) => return ERRNO_ISDIR,
                _ => return ERRNO_BADF,
            };
            let mut total = 0u32;
            for iov in &iovs {
                if iov.buf_len == 0 {
                    continue;
                }
                match fd_info {
                    (true, _) => {
                        let mut buf = vec![0u8; iov.buf_len as usize];
                        match sys_kernel_console_read(
                            KernelConsoleSource::Console,
                            &mut buf,
                            KernelConsoleReadFlags::empty(),
                        ) {
                            Ok(n) if n > 0 => {
                                if mem
                                    .write(&mut caller, iov.buf_ptr as usize, &buf[..n])
                                    .is_err()
                                {
                                    return ERRNO_FAULT;
                                }
                                total += n as u32;
                            }
                            _ => break,
                        }
                    }
                    (false, Some((twz_fd, pos))) => {
                        let mut buf = vec![0u8; iov.buf_len as usize];
                        let read_pos = pos + total as u64;
                        let mut ctx = IoCtx::new(Some(read_pos), IoFlags::empty(), None);
                        match twizzler_rt_abi::io::twz_rt_fd_pread(twz_fd, &mut buf, &mut ctx) {
                            Ok(0) => break,
                            Ok(n) => {
                                if mem
                                    .write(&mut caller, iov.buf_ptr as usize, &buf[..n])
                                    .is_err()
                                {
                                    return ERRNO_FAULT;
                                }
                                total += n as u32;
                            }
                            Err(e) => return twz_err_to_errno(e),
                        }
                    }
                    _ => return ERRNO_BADF,
                }
            }
            if let (false, Some(_)) = fd_info {
                if let Some(Some(P1Fd::File { position, .. })) =
                    caller.data_mut().fds.get_mut(fd as usize)
                {
                    *position += total as u64;
                }
            }
            if mem
                .write(&mut caller, nread_ptr as usize, &total.to_le_bytes())
                .is_err()
            {
                return ERRNO_FAULT;
            }
            ERRNO_SUCCESS
        },
    )?;

    // ── fd_write ────────────────────────────────────────────────

    linker.func_wrap(
        ns,
        "fd_write",
        |mut caller: Caller<'_, WasiP1Ctx>,
         fd: i32,
         iovs_ptr: i32,
         iovs_len: i32,
         nwritten_ptr: i32|
         -> i32 {
            let mem = match get_mem(&mut caller) {
                Some(m) => m,
                None => return ERRNO_IO,
            };
            let iovs = match read_iovs(&mem, &mut caller, iovs_ptr, iovs_len) {
                Some(v) => v,
                None => return ERRNO_FAULT,
            };
            let fd_info = match caller.data().get_fd(fd) {
                Some(P1Fd::Stdout) | Some(P1Fd::Stderr) => (true, None),
                Some(P1Fd::File {
                    twz_fd,
                    position,
                    append,
                    ..
                }) => (false, Some((*twz_fd, *position, *append))),
                Some(P1Fd::Dir { .. }) => return ERRNO_ISDIR,
                _ => return ERRNO_BADF,
            };
            let mut total = 0u32;
            for iov in &iovs {
                if iov.buf_len == 0 {
                    continue;
                }
                let mut data = vec![0u8; iov.buf_len as usize];
                if mem
                    .read(&caller, iov.buf_ptr as usize, &mut data)
                    .is_err()
                {
                    return ERRNO_FAULT;
                }
                match fd_info {
                    (true, _) => {
                        sys_kernel_console_write(
                            KernelConsoleSource::Console,
                            &data,
                            KernelConsoleWriteFlags::empty(),
                        );
                        total += data.len() as u32;
                    }
                    (false, Some((twz_fd, pos, is_append))) => {
                        let write_pos = if is_append {
                            None
                        } else {
                            Some(pos + total as u64)
                        };
                        let mut ctx = IoCtx::new(write_pos, IoFlags::empty(), None);
                        match twizzler_rt_abi::io::twz_rt_fd_pwrite(twz_fd, &data, &mut ctx) {
                            Ok(n) => total += n as u32,
                            Err(e) => return twz_err_to_errno(e),
                        }
                    }
                    _ => return ERRNO_BADF,
                }
            }
            if let (false, Some((_, _, is_append))) = fd_info {
                if !is_append {
                    if let Some(Some(P1Fd::File { position, .. })) =
                        caller.data_mut().fds.get_mut(fd as usize)
                    {
                        *position += total as u64;
                    }
                }
            }
            if mem
                .write(
                    &mut caller,
                    nwritten_ptr as usize,
                    &total.to_le_bytes(),
                )
                .is_err()
            {
                return ERRNO_FAULT;
            }
            ERRNO_SUCCESS
        },
    )?;

    // ── fd_pread ────────────────────────────────────────────────

    linker.func_wrap(
        ns,
        "fd_pread",
        |mut caller: Caller<'_, WasiP1Ctx>,
         fd: i32,
         iovs_ptr: i32,
         iovs_len: i32,
         offset: i64,
         nread_ptr: i32|
         -> i32 {
            let mem = match get_mem(&mut caller) {
                Some(m) => m,
                None => return ERRNO_IO,
            };
            let iovs = match read_iovs(&mem, &mut caller, iovs_ptr, iovs_len) {
                Some(v) => v,
                None => return ERRNO_FAULT,
            };
            let twz_fd = match caller.data().get_fd(fd) {
                Some(P1Fd::File { twz_fd, .. }) => *twz_fd,
                _ => return ERRNO_BADF,
            };
            let mut total = 0u32;
            for iov in &iovs {
                if iov.buf_len == 0 {
                    continue;
                }
                let mut buf = vec![0u8; iov.buf_len as usize];
                let read_pos = offset as u64 + total as u64;
                let mut ctx = IoCtx::new(Some(read_pos), IoFlags::empty(), None);
                match twizzler_rt_abi::io::twz_rt_fd_pread(twz_fd, &mut buf, &mut ctx) {
                    Ok(0) => break,
                    Ok(n) => {
                        if mem
                            .write(&mut caller, iov.buf_ptr as usize, &buf[..n])
                            .is_err()
                        {
                            return ERRNO_FAULT;
                        }
                        total += n as u32;
                    }
                    Err(e) => return twz_err_to_errno(e),
                }
            }
            if mem
                .write(&mut caller, nread_ptr as usize, &total.to_le_bytes())
                .is_err()
            {
                return ERRNO_FAULT;
            }
            ERRNO_SUCCESS
        },
    )?;

    // ── fd_pwrite ───────────────────────────────────────────────

    linker.func_wrap(
        ns,
        "fd_pwrite",
        |mut caller: Caller<'_, WasiP1Ctx>,
         fd: i32,
         iovs_ptr: i32,
         iovs_len: i32,
         offset: i64,
         nwritten_ptr: i32|
         -> i32 {
            let mem = match get_mem(&mut caller) {
                Some(m) => m,
                None => return ERRNO_IO,
            };
            let iovs = match read_iovs(&mem, &mut caller, iovs_ptr, iovs_len) {
                Some(v) => v,
                None => return ERRNO_FAULT,
            };
            let twz_fd = match caller.data().get_fd(fd) {
                Some(P1Fd::File { twz_fd, .. }) => *twz_fd,
                _ => return ERRNO_BADF,
            };
            let mut total = 0u32;
            for iov in &iovs {
                if iov.buf_len == 0 {
                    continue;
                }
                let mut data = vec![0u8; iov.buf_len as usize];
                if mem
                    .read(&caller, iov.buf_ptr as usize, &mut data)
                    .is_err()
                {
                    return ERRNO_FAULT;
                }
                let write_pos = offset as u64 + total as u64;
                let mut ctx = IoCtx::new(Some(write_pos), IoFlags::empty(), None);
                match twizzler_rt_abi::io::twz_rt_fd_pwrite(twz_fd, &data, &mut ctx) {
                    Ok(n) => total += n as u32,
                    Err(e) => return twz_err_to_errno(e),
                }
            }
            if mem
                .write(
                    &mut caller,
                    nwritten_ptr as usize,
                    &total.to_le_bytes(),
                )
                .is_err()
            {
                return ERRNO_FAULT;
            }
            ERRNO_SUCCESS
        },
    )?;

    // ── fd_seek / fd_tell ───────────────────────────────────────

    linker.func_wrap(
        ns,
        "fd_seek",
        |mut caller: Caller<'_, WasiP1Ctx>,
         fd: i32,
         offset: i64,
         whence: i32,
         newoffset_ptr: i32|
         -> i32 {
            let mem = match get_mem(&mut caller) {
                Some(m) => m,
                None => return ERRNO_IO,
            };
            let (current_pos, file_size) = match caller.data().get_fd(fd) {
                Some(P1Fd::File {
                    position, twz_fd, ..
                }) => {
                    let size = fd::twz_rt_fd_get_info(*twz_fd)
                        .map(|i| i.size)
                        .unwrap_or(0);
                    (*position, size)
                }
                _ => return ERRNO_BADF,
            };
            let new_pos = match whence {
                WHENCE_SET => {
                    if offset < 0 {
                        return ERRNO_INVAL;
                    }
                    offset as u64
                }
                WHENCE_CUR => (current_pos as i64 + offset) as u64,
                WHENCE_END => (file_size as i64 + offset) as u64,
                _ => return ERRNO_INVAL,
            };
            if let Some(Some(P1Fd::File { position, .. })) =
                caller.data_mut().fds.get_mut(fd as usize)
            {
                *position = new_pos;
            }
            if mem
                .write(
                    &mut caller,
                    newoffset_ptr as usize,
                    &new_pos.to_le_bytes(),
                )
                .is_err()
            {
                return ERRNO_FAULT;
            }
            ERRNO_SUCCESS
        },
    )?;

    linker.func_wrap(
        ns,
        "fd_tell",
        |mut caller: Caller<'_, WasiP1Ctx>, fd: i32, offset_ptr: i32| -> i32 {
            let mem = match get_mem(&mut caller) {
                Some(m) => m,
                None => return ERRNO_IO,
            };
            let pos = match caller.data().get_fd(fd) {
                Some(P1Fd::File { position, .. }) => *position,
                _ => return ERRNO_BADF,
            };
            if mem
                .write(&mut caller, offset_ptr as usize, &pos.to_le_bytes())
                .is_err()
            {
                return ERRNO_FAULT;
            }
            ERRNO_SUCCESS
        },
    )?;

    // ── fd_close ────────────────────────────────────────────────

    linker.func_wrap(
        ns,
        "fd_close",
        |mut caller: Caller<'_, WasiP1Ctx>, fd: i32| -> i32 {
            if caller.data().get_fd(fd).is_none() {
                return ERRNO_BADF;
            }
            caller.data_mut().close_fd(fd);
            ERRNO_SUCCESS
        },
    )?;

    // ── fd_sync / fd_datasync ───────────────────────────────────

    linker.func_wrap(
        ns,
        "fd_sync",
        |caller: Caller<'_, WasiP1Ctx>, fd: i32| -> i32 {
            match caller.data().get_fd(fd) {
                Some(P1Fd::File { twz_fd, .. }) | Some(P1Fd::Dir { twz_fd, .. }) => {
                    fd::twz_rt_fd_sync(*twz_fd);
                    ERRNO_SUCCESS
                }
                Some(_) => ERRNO_SUCCESS,
                None => ERRNO_BADF,
            }
        },
    )?;

    linker.func_wrap(
        ns,
        "fd_datasync",
        |caller: Caller<'_, WasiP1Ctx>, fd: i32| -> i32 {
            match caller.data().get_fd(fd) {
                Some(P1Fd::File { twz_fd, .. }) | Some(P1Fd::Dir { twz_fd, .. }) => {
                    fd::twz_rt_fd_sync(*twz_fd);
                    ERRNO_SUCCESS
                }
                Some(_) => ERRNO_SUCCESS,
                None => ERRNO_BADF,
            }
        },
    )?;

    // ── fd stubs ────────────────────────────────────────────────

    linker.func_wrap(
        ns,
        "fd_advise",
        |_: Caller<'_, WasiP1Ctx>, _fd: i32, _offset: i64, _len: i64, _advice: i32| -> i32 {
            ERRNO_SUCCESS
        },
    )?;

    linker.func_wrap(
        ns,
        "fd_allocate",
        |_: Caller<'_, WasiP1Ctx>, _fd: i32, _offset: i64, _len: i64| -> i32 { ERRNO_NOTSUP },
    )?;

    linker.func_wrap(
        ns,
        "fd_fdstat_set_flags",
        |_: Caller<'_, WasiP1Ctx>, _fd: i32, _flags: i32| -> i32 { ERRNO_NOTSUP },
    )?;

    linker.func_wrap(
        ns,
        "fd_fdstat_set_rights",
        |_: Caller<'_, WasiP1Ctx>, _fd: i32, _rb: i64, _ri: i64| -> i32 { ERRNO_SUCCESS },
    )?;

    linker.func_wrap(
        ns,
        "fd_filestat_set_size",
        |caller: Caller<'_, WasiP1Ctx>, fd: i32, size: i64| -> i32 {
            match caller.data().get_fd(fd) {
                Some(P1Fd::File { twz_fd, .. }) => {
                    match fd::twz_rt_fd_truncate(*twz_fd, size as u64) {
                        Ok(()) => ERRNO_SUCCESS,
                        Err(e) => twz_err_to_errno(e),
                    }
                }
                _ => ERRNO_BADF,
            }
        },
    )?;

    linker.func_wrap(
        ns,
        "fd_filestat_set_times",
        |_: Caller<'_, WasiP1Ctx>, _fd: i32, _atim: i64, _mtim: i64, _flags: i32| -> i32 {
            ERRNO_NOTSUP
        },
    )?;

    linker.func_wrap(
        ns,
        "fd_renumber",
        |_: Caller<'_, WasiP1Ctx>, _from: i32, _to: i32| -> i32 { ERRNO_NOTSUP },
    )?;

    // ── fd_readdir ──────────────────────────────────────────────

    linker.func_wrap(
        ns,
        "fd_readdir",
        |mut caller: Caller<'_, WasiP1Ctx>,
         fd: i32,
         buf_ptr: i32,
         buf_len: i32,
         cookie: i64,
         bufused_ptr: i32|
         -> i32 {
            let mem = match get_mem(&mut caller) {
                Some(m) => m,
                None => return ERRNO_IO,
            };
            let twz_fd = match caller.data().get_fd(fd) {
                Some(P1Fd::Dir { twz_fd, .. }) => *twz_fd,
                _ => return ERRNO_BADF,
            };
            let mut entries = vec![unsafe { core::mem::zeroed::<NameEntry>() }; 64];
            let count =
                match fd::twz_rt_fd_enumerate_names(twz_fd, &mut entries, cookie as usize) {
                    Ok(n) => n,
                    Err(e) => return twz_err_to_errno(e),
                };
            // WASI dirent: d_next(8) + d_ino(8) + d_namlen(4) + d_type(1) + 3pad = 24 + name
            let mut offset = 0u32;
            let buf_limit = buf_len as u32;
            for (i, entry) in entries[..count].iter().enumerate() {
                let name = entry.name_bytes();
                let kind = FdKind::from(entry.info.kind);
                let d_type = fd_kind_to_filetype(kind);
                let d_namlen = name.len() as u32;
                let entry_size = 24 + d_namlen;
                if offset + entry_size > buf_limit {
                    break;
                }
                let d_next = (cookie as u64) + (i as u64) + 1;
                let write_base = buf_ptr as u32 + offset;
                let mut header = [0u8; 24];
                header[0..8].copy_from_slice(&d_next.to_le_bytes());
                header[16..20].copy_from_slice(&d_namlen.to_le_bytes());
                header[20] = d_type;
                if mem
                    .write(&mut caller, write_base as usize, &header)
                    .is_err()
                {
                    return ERRNO_FAULT;
                }
                if mem
                    .write(&mut caller, (write_base + 24) as usize, name)
                    .is_err()
                {
                    return ERRNO_FAULT;
                }
                offset += entry_size;
            }
            if mem
                .write(
                    &mut caller,
                    bufused_ptr as usize,
                    &offset.to_le_bytes(),
                )
                .is_err()
            {
                return ERRNO_FAULT;
            }
            ERRNO_SUCCESS
        },
    )?;

    // ── path_open ───────────────────────────────────────────────

    linker.func_wrap(
        ns,
        "path_open",
        |mut caller: Caller<'_, WasiP1Ctx>,
         dir_fd: i32,
         _dirflags: i32,
         path_ptr: i32,
         path_len: i32,
         oflags: i32,
         _rights_base: i64,
         _rights_inherit: i64,
         fdflags: i32,
         opened_fd_ptr: i32|
         -> i32 {
            let mem = match get_mem(&mut caller) {
                Some(m) => m,
                None => return ERRNO_IO,
            };
            let base_path = match caller.data().get_fd(dir_fd) {
                Some(P1Fd::Dir { path, .. }) => path.clone(),
                _ => return ERRNO_BADF,
            };
            let rel_path = match guest_string(&mem, &mut caller, path_ptr, path_len) {
                Some(s) => s,
                None => return ERRNO_FAULT,
            };
            let full = join_path(&base_path, &rel_path);

            let kind = if oflags & OFLAGS_CREAT != 0 {
                if oflags & OFLAGS_EXCL != 0 {
                    twizzler_rt_abi::bindings::CREATE_KIND_NEW
                } else {
                    twizzler_rt_abi::bindings::CREATE_KIND_EITHER
                }
            } else {
                twizzler_rt_abi::bindings::CREATE_KIND_EXISTING
            };

            if oflags & OFLAGS_DIRECTORY != 0 && oflags & OFLAGS_CREAT != 0 {
                match fd::twz_rt_fd_mkns(&full) {
                    Ok(()) => {}
                    Err(twizzler_rt_abi::error::TwzError::Naming(
                        twizzler_rt_abi::error::NamingError::AlreadyExists,
                    )) => {}
                    Err(e) => return twz_err_to_errno(e),
                }
            }

            let create = twizzler_rt_abi::bindings::create_options {
                id: Default::default(),
                kind,
            };
            let mut flags = twizzler_rt_abi::bindings::OPEN_FLAG_READ;
            flags |= twizzler_rt_abi::bindings::OPEN_FLAG_WRITE;
            if oflags & OFLAGS_TRUNC != 0 {
                flags |= twizzler_rt_abi::bindings::OPEN_FLAG_TRUNCATE;
            }

            match fd::twz_rt_fd_open(&full, create, flags) {
                Ok(raw_fd) => {
                    let is_dir = fd::twz_rt_fd_get_info(raw_fd)
                        .map(|i| matches!(FdKind::from(i.kind), FdKind::Directory))
                        .unwrap_or(false);
                    let entry = if is_dir {
                        P1Fd::Dir {
                            twz_fd: raw_fd,
                            path: full,
                        }
                    } else {
                        P1Fd::File {
                            twz_fd: raw_fd,
                            position: 0,
                            append: fdflags & FDFLAGS_APPEND != 0,
                        }
                    };
                    let new_fd = caller.data_mut().alloc_fd(entry);
                    if mem
                        .write(
                            &mut caller,
                            opened_fd_ptr as usize,
                            &new_fd.to_le_bytes(),
                        )
                        .is_err()
                    {
                        return ERRNO_FAULT;
                    }
                    ERRNO_SUCCESS
                }
                Err(e) => twz_err_to_errno(e),
            }
        },
    )?;

    // ── path operations ─────────────────────────────────────────

    linker.func_wrap(
        ns,
        "path_create_directory",
        |mut caller: Caller<'_, WasiP1Ctx>, fd: i32, path_ptr: i32, path_len: i32| -> i32 {
            let mem = match get_mem(&mut caller) {
                Some(m) => m,
                None => return ERRNO_IO,
            };
            let base_path = match caller.data().get_fd(fd) {
                Some(P1Fd::Dir { path, .. }) => path.clone(),
                _ => return ERRNO_BADF,
            };
            let rel_path = match guest_string(&mem, &mut caller, path_ptr, path_len) {
                Some(s) => s,
                None => return ERRNO_FAULT,
            };
            match fd::twz_rt_fd_mkns(&join_path(&base_path, &rel_path)) {
                Ok(()) => ERRNO_SUCCESS,
                Err(e) => twz_err_to_errno(e),
            }
        },
    )?;

    linker.func_wrap(
        ns,
        "path_remove_directory",
        |mut caller: Caller<'_, WasiP1Ctx>, fd: i32, path_ptr: i32, path_len: i32| -> i32 {
            let mem = match get_mem(&mut caller) {
                Some(m) => m,
                None => return ERRNO_IO,
            };
            let base_path = match caller.data().get_fd(fd) {
                Some(P1Fd::Dir { path, .. }) => path.clone(),
                _ => return ERRNO_BADF,
            };
            let rel_path = match guest_string(&mem, &mut caller, path_ptr, path_len) {
                Some(s) => s,
                None => return ERRNO_FAULT,
            };
            match fd::twz_rt_fd_remove(&join_path(&base_path, &rel_path)) {
                Ok(()) => ERRNO_SUCCESS,
                Err(e) => twz_err_to_errno(e),
            }
        },
    )?;

    linker.func_wrap(
        ns,
        "path_unlink_file",
        |mut caller: Caller<'_, WasiP1Ctx>, fd: i32, path_ptr: i32, path_len: i32| -> i32 {
            let mem = match get_mem(&mut caller) {
                Some(m) => m,
                None => return ERRNO_IO,
            };
            let base_path = match caller.data().get_fd(fd) {
                Some(P1Fd::Dir { path, .. }) => path.clone(),
                _ => return ERRNO_BADF,
            };
            let rel_path = match guest_string(&mem, &mut caller, path_ptr, path_len) {
                Some(s) => s,
                None => return ERRNO_FAULT,
            };
            match fd::twz_rt_fd_remove(&join_path(&base_path, &rel_path)) {
                Ok(()) => ERRNO_SUCCESS,
                Err(e) => twz_err_to_errno(e),
            }
        },
    )?;

    linker.func_wrap(
        ns,
        "path_filestat_get",
        |mut caller: Caller<'_, WasiP1Ctx>,
         fd: i32,
         _flags: i32,
         path_ptr: i32,
         path_len: i32,
         stat_ptr: i32|
         -> i32 {
            let mem = match get_mem(&mut caller) {
                Some(m) => m,
                None => return ERRNO_IO,
            };
            let base_path = match caller.data().get_fd(fd) {
                Some(P1Fd::Dir { path, .. }) => path.clone(),
                _ => return ERRNO_BADF,
            };
            let rel_path = match guest_string(&mem, &mut caller, path_ptr, path_len) {
                Some(s) => s,
                None => return ERRNO_FAULT,
            };
            let full = join_path(&base_path, &rel_path);
            let create = twizzler_rt_abi::bindings::create_options {
                id: Default::default(),
                kind: twizzler_rt_abi::bindings::CREATE_KIND_EXISTING,
            };
            match fd::twz_rt_fd_open(&full, create, twizzler_rt_abi::bindings::OPEN_FLAG_READ) {
                Ok(child_fd) => {
                    let result = match fd::twz_rt_fd_get_info(child_fd) {
                        Ok(info) => {
                            let mut buf = [0u8; 64];
                            buf[16] = fd_kind_to_filetype(info.kind);
                            buf[24..32].copy_from_slice(&1u64.to_le_bytes());
                            buf[32..40].copy_from_slice(&info.size.to_le_bytes());
                            buf[40..48].copy_from_slice(
                                &(info.accessed.as_nanos() as u64).to_le_bytes(),
                            );
                            buf[48..56].copy_from_slice(
                                &(info.modified.as_nanos() as u64).to_le_bytes(),
                            );
                            buf[56..64].copy_from_slice(
                                &(info.created.as_nanos() as u64).to_le_bytes(),
                            );
                            if mem.write(&mut caller, stat_ptr as usize, &buf).is_err() {
                                ERRNO_FAULT
                            } else {
                                ERRNO_SUCCESS
                            }
                        }
                        Err(e) => twz_err_to_errno(e),
                    };
                    fd::twz_rt_fd_close(child_fd);
                    result
                }
                Err(e) => twz_err_to_errno(e),
            }
        },
    )?;

    linker.func_wrap(
        ns,
        "path_readlink",
        |mut caller: Caller<'_, WasiP1Ctx>,
         fd: i32,
         path_ptr: i32,
         path_len: i32,
         buf_ptr: i32,
         buf_len: i32,
         bufused_ptr: i32|
         -> i32 {
            let mem = match get_mem(&mut caller) {
                Some(m) => m,
                None => return ERRNO_IO,
            };
            let base_path = match caller.data().get_fd(fd) {
                Some(P1Fd::Dir { path, .. }) => path.clone(),
                _ => return ERRNO_BADF,
            };
            let rel_path = match guest_string(&mem, &mut caller, path_ptr, path_len) {
                Some(s) => s,
                None => return ERRNO_FAULT,
            };
            let full = join_path(&base_path, &rel_path);
            let mut target_buf = vec![0u8; buf_len as usize];
            match fd::twz_rt_fd_readlink(&full, &mut target_buf) {
                Ok(n) => {
                    if mem
                        .write(&mut caller, buf_ptr as usize, &target_buf[..n])
                        .is_err()
                    {
                        return ERRNO_FAULT;
                    }
                    if mem
                        .write(
                            &mut caller,
                            bufused_ptr as usize,
                            &(n as u32).to_le_bytes(),
                        )
                        .is_err()
                    {
                        return ERRNO_FAULT;
                    }
                    ERRNO_SUCCESS
                }
                Err(e) => twz_err_to_errno(e),
            }
        },
    )?;

    linker.func_wrap(
        ns,
        "path_symlink",
        |mut caller: Caller<'_, WasiP1Ctx>,
         old_path_ptr: i32,
         old_path_len: i32,
         fd: i32,
         new_path_ptr: i32,
         new_path_len: i32|
         -> i32 {
            let mem = match get_mem(&mut caller) {
                Some(m) => m,
                None => return ERRNO_IO,
            };
            let base_path = match caller.data().get_fd(fd) {
                Some(P1Fd::Dir { path, .. }) => path.clone(),
                _ => return ERRNO_BADF,
            };
            let old_path = match guest_string(&mem, &mut caller, old_path_ptr, old_path_len) {
                Some(s) => s,
                None => return ERRNO_FAULT,
            };
            let new_rel = match guest_string(&mem, &mut caller, new_path_ptr, new_path_len) {
                Some(s) => s,
                None => return ERRNO_FAULT,
            };
            match fd::twz_rt_fd_symlink(&join_path(&base_path, &new_rel), &old_path) {
                Ok(()) => ERRNO_SUCCESS,
                Err(e) => twz_err_to_errno(e),
            }
        },
    )?;

    // ── path stubs ──────────────────────────────────────────────

    linker.func_wrap(
        ns,
        "path_filestat_set_times",
        |_: Caller<'_, WasiP1Ctx>,
         _fd: i32,
         _flags: i32,
         _pp: i32,
         _pl: i32,
         _atim: i64,
         _mtim: i64,
         _fst: i32|
         -> i32 { ERRNO_NOTSUP },
    )?;

    linker.func_wrap(
        ns,
        "path_link",
        |_: Caller<'_, WasiP1Ctx>,
         _ofd: i32,
         _of: i32,
         _opp: i32,
         _opl: i32,
         _nfd: i32,
         _npp: i32,
         _npl: i32|
         -> i32 { ERRNO_NOTSUP },
    )?;

    linker.func_wrap(
        ns,
        "path_rename",
        |mut caller: Caller<'_, WasiP1Ctx>,
         old_fd: i32,
         old_path_ptr: i32,
         old_path_len: i32,
         new_fd: i32,
         new_path_ptr: i32,
         new_path_len: i32|
         -> i32 {
            let mem = match get_mem(&mut caller) {
                Some(m) => m,
                None => return ERRNO_IO,
            };
            let old_base = match caller.data().get_fd(old_fd) {
                Some(P1Fd::Dir { path, .. }) => path.clone(),
                _ => return ERRNO_BADF,
            };
            let new_base = match caller.data().get_fd(new_fd) {
                Some(P1Fd::Dir { path, .. }) => path.clone(),
                _ => return ERRNO_BADF,
            };
            let old_rel = match guest_string(&mem, &mut caller, old_path_ptr, old_path_len) {
                Some(s) => s,
                None => return ERRNO_FAULT,
            };
            let new_rel = match guest_string(&mem, &mut caller, new_path_ptr, new_path_len) {
                Some(s) => s,
                None => return ERRNO_FAULT,
            };
            match fd::twz_rt_fd_rename(
                &join_path(&old_base, &old_rel),
                &join_path(&new_base, &new_rel),
            ) {
                Ok(()) => ERRNO_SUCCESS,
                Err(e) => twz_err_to_errno(e),
            }
        },
    )?;

    // ── poll_oneoff ─────────────────────────────────────────────

    linker.func_wrap(
        ns,
        "poll_oneoff",
        |mut caller: Caller<'_, WasiP1Ctx>,
         in_ptr: i32,
         out_ptr: i32,
         nsubs: i32,
         nevents_ptr: i32|
         -> i32 {
            let mem = match get_mem(&mut caller) {
                Some(m) => m,
                None => return ERRNO_IO,
            };
            // All subscriptions are immediately ready (synchronous mode).
            // Subscription: 48 bytes (userdata(8) + u(40))
            // Event: 32 bytes (userdata(8) + error(2) + type(1) + pad(5) + fd_readwrite(16))
            for i in 0..nsubs {
                let sub_offset = (in_ptr + i * 48) as usize;
                let mut userdata = [0u8; 8];
                if mem.read(&caller, sub_offset, &mut userdata).is_err() {
                    return ERRNO_FAULT;
                }
                let mut event = [0u8; 32];
                event[0..8].copy_from_slice(&userdata);
                let evt_offset = (out_ptr + i * 32) as usize;
                if mem.write(&mut caller, evt_offset, &event).is_err() {
                    return ERRNO_FAULT;
                }
            }
            if mem
                .write(
                    &mut caller,
                    nevents_ptr as usize,
                    &(nsubs as u32).to_le_bytes(),
                )
                .is_err()
            {
                return ERRNO_FAULT;
            }
            ERRNO_SUCCESS
        },
    )?;

    // ── standard WASI P1 socket functions ─────────────────────

    linker.func_wrap(
        ns,
        "sock_accept",
        |mut caller: Caller<'_, WasiP1Ctx>, fd: i32, _flags: i32, rfd_ptr: i32| -> i32 {
            let mem = match get_mem(&mut caller) {
                Some(m) => m,
                None => return ERRNO_IO,
            };
            // Check if this fd is a listener — we need to extract it to call accept().
            // Since accept() is blocking, we must not hold borrows across it.
            let is_listener = matches!(caller.data().get_fd(fd), Some(P1Fd::TcpListener { .. }));
            if !is_listener {
                return ERRNO_BADF;
            }

            // We can't call accept while borrowing the fd table, so we need
            // to access the listener directly. Use a temporary reference.
            let listener_ptr = match caller.data().get_fd(fd) {
                Some(P1Fd::TcpListener { listener }) => listener as *const net::NetListener,
                _ => return ERRNO_BADF,
            };

            // SAFETY: The listener lives in the fd table which is owned by the store.
            // We are in a synchronous host call, so the store is not concurrently accessed.
            let (socket, _remote) = match unsafe { &*listener_ptr }.accept() {
                Ok(r) => r,
                Err(e) => return net_err_to_errno(e),
            };

            let new_fd = caller
                .data_mut()
                .alloc_fd(P1Fd::TcpSocket { socket });
            if mem
                .write(
                    &mut caller,
                    rfd_ptr as usize,
                    &(new_fd as u32).to_le_bytes(),
                )
                .is_err()
            {
                return ERRNO_FAULT;
            }
            ERRNO_SUCCESS
        },
    )?;

    linker.func_wrap(
        ns,
        "sock_recv",
        |mut caller: Caller<'_, WasiP1Ctx>,
         fd: i32,
         iovs_ptr: i32,
         iovs_len: i32,
         _ri_flags: i32,
         nread_ptr: i32,
         ro_flags_ptr: i32|
         -> i32 {
            let mem = match get_mem(&mut caller) {
                Some(m) => m,
                None => return ERRNO_IO,
            };
            let iovs = match read_iovs(&mem, &mut caller, iovs_ptr, iovs_len) {
                Some(v) => v,
                None => return ERRNO_FAULT,
            };
            let socket_ptr = match caller.data().get_fd(fd) {
                Some(P1Fd::TcpSocket { socket }) => socket as *const net::NetSocket,
                _ => return ERRNO_BADF,
            };
            let mut total = 0u32;
            for iov in &iovs {
                if iov.buf_len == 0 {
                    continue;
                }
                let mut buf = vec![0u8; iov.buf_len as usize];
                // SAFETY: see sock_accept — synchronous host call, no concurrent access.
                let n = match unsafe { &*socket_ptr }.read(&mut buf) {
                    Ok(n) => n,
                    Err(e) => {
                        if total > 0 {
                            break;
                        }
                        return net_err_to_errno(e);
                    }
                };
                if mem
                    .write(&mut caller, iov.buf_ptr as usize, &buf[..n])
                    .is_err()
                {
                    return ERRNO_FAULT;
                }
                total += n as u32;
                if n < iov.buf_len as usize {
                    break; // short read
                }
            }
            if mem
                .write(&mut caller, nread_ptr as usize, &total.to_le_bytes())
                .is_err()
            {
                return ERRNO_FAULT;
            }
            if mem
                .write(&mut caller, ro_flags_ptr as usize, &0u32.to_le_bytes())
                .is_err()
            {
                return ERRNO_FAULT;
            }
            ERRNO_SUCCESS
        },
    )?;

    linker.func_wrap(
        ns,
        "sock_send",
        |mut caller: Caller<'_, WasiP1Ctx>,
         fd: i32,
         iovs_ptr: i32,
         iovs_len: i32,
         _si_flags: i32,
         nwritten_ptr: i32|
         -> i32 {
            let mem = match get_mem(&mut caller) {
                Some(m) => m,
                None => return ERRNO_IO,
            };
            let iovs = match read_iovs(&mem, &mut caller, iovs_ptr, iovs_len) {
                Some(v) => v,
                None => return ERRNO_FAULT,
            };
            let socket_ptr = match caller.data().get_fd(fd) {
                Some(P1Fd::TcpSocket { socket }) => socket as *const net::NetSocket,
                _ => return ERRNO_BADF,
            };
            let mut total = 0u32;
            for iov in &iovs {
                if iov.buf_len == 0 {
                    continue;
                }
                let mut buf = vec![0u8; iov.buf_len as usize];
                if mem.read(&caller, iov.buf_ptr as usize, &mut buf).is_err() {
                    return ERRNO_FAULT;
                }
                let n = match unsafe { &*socket_ptr }.write(&buf) {
                    Ok(n) => n,
                    Err(e) => {
                        if total > 0 {
                            break;
                        }
                        return net_err_to_errno(e);
                    }
                };
                total += n as u32;
                if n < iov.buf_len as usize {
                    break;
                }
            }
            if mem
                .write(
                    &mut caller,
                    nwritten_ptr as usize,
                    &total.to_le_bytes(),
                )
                .is_err()
            {
                return ERRNO_FAULT;
            }
            ERRNO_SUCCESS
        },
    )?;

    linker.func_wrap(
        ns,
        "sock_shutdown",
        |caller: Caller<'_, WasiP1Ctx>, fd: i32, how: i32| -> i32 {
            let shutdown = match how {
                0 => net::NetShutdown::Read,
                1 => net::NetShutdown::Write,
                2 => net::NetShutdown::Both,
                _ => return ERRNO_INVAL,
            };
            let socket_ptr = match caller.data().get_fd(fd) {
                Some(P1Fd::TcpSocket { socket }) => socket as *const net::NetSocket,
                _ => return ERRNO_BADF,
            };
            match unsafe { &*socket_ptr }.shutdown(shutdown) {
                Ok(()) => ERRNO_SUCCESS,
                Err(e) => net_err_to_errno(e),
            }
        },
    )?;

    // ── non-standard socket extensions (WASIX-style) ─────────

    linker.func_wrap(
        ns,
        "sock_open",
        |mut caller: Caller<'_, WasiP1Ctx>,
         af: i32,
         socktype: i32,
         _proto: i32,
         result_fd_ptr: i32|
         -> i32 {
            let mem = match get_mem(&mut caller) {
                Some(m) => m,
                None => return ERRNO_IO,
            };
            // AF_INET = 2, SOCK_STREAM = 1, SOCK_DGRAM = 2
            if af != 2 {
                return ERRNO_NOTSUP;
            }
            let entry = match socktype {
                1 => P1Fd::TcpUnbound,
                2 => P1Fd::UdpUnbound,
                _ => return ERRNO_NOTSUP,
            };
            let fd_num = caller.data_mut().alloc_fd(entry);
            if mem
                .write(
                    &mut caller,
                    result_fd_ptr as usize,
                    &(fd_num as u32).to_le_bytes(),
                )
                .is_err()
            {
                return ERRNO_FAULT;
            }
            ERRNO_SUCCESS
        },
    )?;

    linker.func_wrap(
        ns,
        "sock_connect",
        |mut caller: Caller<'_, WasiP1Ctx>, fd: i32, addr_ptr: i32, addr_len: i32| -> i32 {
            let mem = match get_mem(&mut caller) {
                Some(m) => m,
                None => return ERRNO_IO,
            };
            match caller.data().get_fd(fd) {
                Some(P1Fd::TcpUnbound) => {}
                _ => return ERRNO_BADF,
            }
            let addr = match read_sockaddr(&mem, &mut caller, addr_ptr, addr_len) {
                Some(a) => a,
                None => return ERRNO_INVAL,
            };
            let socket = match net::NetSocket::connect(addr) {
                Ok(s) => s,
                Err(e) => return net_err_to_errno(e),
            };
            // Replace the TcpUnbound fd with a connected TcpSocket
            if let Some(slot) = caller.data_mut().fds.get_mut(fd as usize) {
                *slot = Some(P1Fd::TcpSocket { socket });
            }
            ERRNO_SUCCESS
        },
    )?;

    linker.func_wrap(
        ns,
        "sock_bind",
        |mut caller: Caller<'_, WasiP1Ctx>, fd: i32, addr_ptr: i32, addr_len: i32| -> i32 {
            let mem = match get_mem(&mut caller) {
                Some(m) => m,
                None => return ERRNO_IO,
            };
            let is_udp = match caller.data().get_fd(fd) {
                Some(P1Fd::TcpUnbound) => false,
                Some(P1Fd::UdpUnbound) => true,
                _ => return ERRNO_BADF,
            };
            let addr = match read_sockaddr(&mem, &mut caller, addr_ptr, addr_len) {
                Some(a) => a,
                None => return ERRNO_INVAL,
            };
            if is_udp {
                let socket = match net::NetUdpSocket::bind(addr) {
                    Ok(s) => s,
                    Err(e) => return net_err_to_errno(e),
                };
                if let Some(slot) = caller.data_mut().fds.get_mut(fd as usize) {
                    *slot = Some(P1Fd::UdpBound { socket });
                }
            } else {
                if let Some(slot) = caller.data_mut().fds.get_mut(fd as usize) {
                    *slot = Some(P1Fd::TcpBound { addr });
                }
            }
            ERRNO_SUCCESS
        },
    )?;

    linker.func_wrap(
        ns,
        "sock_listen",
        |mut caller: Caller<'_, WasiP1Ctx>, fd: i32, _backlog: i32| -> i32 {
            let addr = match caller.data().get_fd(fd) {
                Some(P1Fd::TcpBound { addr }) => *addr,
                _ => return ERRNO_BADF,
            };
            let listener = match net::NetListener::bind(addr) {
                Ok(l) => l,
                Err(e) => return net_err_to_errno(e),
            };
            if let Some(slot) = caller.data_mut().fds.get_mut(fd as usize) {
                *slot = Some(P1Fd::TcpListener { listener });
            }
            ERRNO_SUCCESS
        },
    )?;

    linker.func_wrap(
        ns,
        "sock_getlocaladdr",
        |mut caller: Caller<'_, WasiP1Ctx>, fd: i32, addr_ptr: i32, len_ptr: i32| -> i32 {
            let mem = match get_mem(&mut caller) {
                Some(m) => m,
                None => return ERRNO_IO,
            };
            let addr = match caller.data().get_fd(fd) {
                Some(P1Fd::TcpSocket { socket }) => match socket.local_addr() {
                    Ok(a) => a,
                    Err(e) => return net_err_to_errno(e),
                },
                Some(P1Fd::TcpListener { listener }) => match listener.local_addr() {
                    Ok(a) => a,
                    Err(e) => return net_err_to_errno(e),
                },
                Some(P1Fd::TcpBound { addr }) => *addr,
                _ => return ERRNO_BADF,
            };
            if !write_sockaddr(&mem, &mut caller, addr_ptr, len_ptr, &addr) {
                return ERRNO_FAULT;
            }
            ERRNO_SUCCESS
        },
    )?;

    linker.func_wrap(
        ns,
        "sock_getpeeraddr",
        |mut caller: Caller<'_, WasiP1Ctx>, fd: i32, addr_ptr: i32, len_ptr: i32| -> i32 {
            let mem = match get_mem(&mut caller) {
                Some(m) => m,
                None => return ERRNO_IO,
            };
            let addr = match caller.data().get_fd(fd) {
                Some(P1Fd::TcpSocket { socket }) => match socket.peer_addr() {
                    Ok(a) => a,
                    Err(e) => return net_err_to_errno(e),
                },
                _ => return ERRNO_BADF,
            };
            if !write_sockaddr(&mem, &mut caller, addr_ptr, len_ptr, &addr) {
                return ERRNO_FAULT;
            }
            ERRNO_SUCCESS
        },
    )?;

    linker.func_wrap(
        ns,
        "sock_sendto",
        |mut caller: Caller<'_, WasiP1Ctx>,
         fd: i32,
         buf_ptr: i32,
         buf_len: i32,
         _flags: i32,
         addr_ptr: i32,
         addr_len: i32,
         nwritten_ptr: i32|
         -> i32 {
            let mem = match get_mem(&mut caller) {
                Some(m) => m,
                None => return ERRNO_IO,
            };
            let socket_ptr = match caller.data().get_fd(fd) {
                Some(P1Fd::UdpBound { socket }) => socket as *const net::NetUdpSocket,
                _ => return ERRNO_BADF,
            };
            let addr = match read_sockaddr(&mem, &mut caller, addr_ptr, addr_len) {
                Some(a) => a,
                None => return ERRNO_INVAL,
            };
            let mut data = vec![0u8; buf_len as usize];
            if mem.read(&caller, buf_ptr as usize, &mut data).is_err() {
                return ERRNO_FAULT;
            }
            // SAFETY: synchronous host call, no concurrent access.
            let n = match unsafe { &*socket_ptr }.send_to(&data, addr) {
                Ok(n) => n,
                Err(e) => return net_err_to_errno(e),
            };
            if mem
                .write(
                    &mut caller,
                    nwritten_ptr as usize,
                    &(n as u32).to_le_bytes(),
                )
                .is_err()
            {
                return ERRNO_FAULT;
            }
            ERRNO_SUCCESS
        },
    )?;

    linker.func_wrap(
        ns,
        "sock_recvfrom",
        |mut caller: Caller<'_, WasiP1Ctx>,
         fd: i32,
         buf_ptr: i32,
         buf_len: i32,
         _flags: i32,
         addr_ptr: i32,
         addr_len_ptr: i32,
         nread_ptr: i32|
         -> i32 {
            let mem = match get_mem(&mut caller) {
                Some(m) => m,
                None => return ERRNO_IO,
            };
            let socket_ptr = match caller.data().get_fd(fd) {
                Some(P1Fd::UdpBound { socket }) => socket as *const net::NetUdpSocket,
                _ => return ERRNO_BADF,
            };
            let mut buf = vec![0u8; buf_len as usize];
            // SAFETY: synchronous host call, no concurrent access.
            let (n, remote) = match unsafe { &*socket_ptr }.recv_from(&mut buf) {
                Ok(r) => r,
                Err(e) => return net_err_to_errno(e),
            };
            if mem.write(&mut caller, buf_ptr as usize, &buf[..n]).is_err() {
                return ERRNO_FAULT;
            }
            if !write_sockaddr(&mem, &mut caller, addr_ptr, addr_len_ptr, &remote) {
                return ERRNO_FAULT;
            }
            if mem
                .write(
                    &mut caller,
                    nread_ptr as usize,
                    &(n as u32).to_le_bytes(),
                )
                .is_err()
            {
                return ERRNO_FAULT;
            }
            ERRNO_SUCCESS
        },
    )?;

    Ok(())
}

// ── Public API ──────────────────────────────────────────────────────

/// Run a WASI P1 core module (wasi_snapshot_preview1).
pub fn run_wasi_p1_module(module_bytes: &[u8]) -> Result<()> {
    let mut config = wasmtime::Config::new();
    config.memory_init_cow(false);
    config.memory_reservation(0);
    config.memory_guard_size(0);
    config.memory_reservation_for_growth(0);
    config.signals_based_traps(false);

    let engine = Engine::new(&config)?;
    let module = Module::new(&engine, module_bytes)?;

    let mut linker = Linker::new(&engine);
    add_wasi_p1_to_linker(&mut linker)?;

    let ctx = WasiP1Ctx::new();
    let mut store = Store::new(&engine, ctx);

    let instance = linker.instantiate(&mut store, &module)?;

    let start = instance.get_typed_func::<(), ()>(&mut store, "_start")?;
    match start.call(&mut store, ()) {
        Ok(()) => Ok(()),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("wasi exit success") || msg.contains("wasi exit with code 0") {
                Ok(())
            } else {
                Err(e)
            }
        }
    }
}

/// Detect whether a WASM binary is a P2 component or P1 core module.
/// Returns true for P2 components, false for P1 core modules.
pub fn is_component(bytes: &[u8]) -> bool {
    // WASM magic: \0asm
    // Core module version byte[4]: 0x01
    // Component version byte[4]: 0x0d
    bytes.len() >= 8 && bytes[0..4] == [0x00, 0x61, 0x73, 0x6d] && bytes[4] == 0x0d
}
