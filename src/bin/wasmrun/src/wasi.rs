//! WASI Preview 2 (component model) for Twizzler — synchronous implementation.
//!
//! Implements wasi:io, wasi:clocks, wasi:cli, wasi:random, and wasi:filesystem
//! backed by Twizzler runtime APIs. Network sockets are not supported; components
//! that import socket interfaces will fail at instantiation time.

use anyhow::{bail, Result};
use core::mem::MaybeUninit;

use twizzler_abi::syscall::{
    sys_get_random, sys_kernel_console_read, sys_kernel_console_write, GetRandomFlags,
    KernelConsoleReadFlags, KernelConsoleSource, KernelConsoleWriteFlags,
};
use twizzler_rt_abi::error::TwzError;
use twizzler_rt_abi::fd::{self, FdKind, NameEntry, RawFd};
use twizzler_rt_abi::io::{IoCtx, IoFlags};

use wasmtime::component::{Component, Linker, Resource, ResourceTable};
use wasmtime::{Config, Engine, Store};

// ── Resource backing types ──────────────────────────────────────────

pub struct IoError(String);

/// All pollables resolve immediately in synchronous mode.
pub struct PollableEntry;

pub enum InputStreamKind {
    Console,
    File { fd: RawFd, position: u64 },
}

pub enum OutputStreamKind {
    Console,
    File {
        fd: RawFd,
        position: u64,
        append: bool,
    },
}

pub struct TerminalInput;
pub struct TerminalOutput;

pub struct DescriptorEntry {
    fd: RawFd,
    path: String,
}

impl Drop for DescriptorEntry {
    fn drop(&mut self) {
        fd::twz_rt_fd_close(self.fd);
    }
}

pub struct DirEntryStream {
    fd: RawFd,
    offset: usize,
    buffer: Vec<NameEntry>,
    buffer_idx: usize,
}

// ── Bindgen ─────────────────────────────────────────────────────────

wasmtime::component::bindgen!({
    path: "wit",
    world: "wasi:cli/command",
    imports: { default: trappable },
    with: {
        "wasi:io/error/error": IoError,
        "wasi:io/poll/pollable": PollableEntry,
        "wasi:io/streams/input-stream": InputStreamKind,
        "wasi:io/streams/output-stream": OutputStreamKind,
        "wasi:cli/terminal-input/terminal-input": TerminalInput,
        "wasi:cli/terminal-output/terminal-output": TerminalOutput,
        "wasi:filesystem/types/descriptor": DescriptorEntry,
        "wasi:filesystem/types/directory-entry-stream": DirEntryStream,
    },
});

// ── WASI Context ────────────────────────────────────────────────────

pub struct WasiCtx {
    table: ResourceTable,
}

// ── wasi:io/error ───────────────────────────────────────────────────

impl wasi::io::error::Host for WasiCtx {}

impl wasi::io::error::HostError for WasiCtx {
    fn to_debug_string(&mut self, self_: Resource<IoError>) -> Result<String> {
        Ok(self.table.get(&self_)?.0.clone())
    }

    fn drop(&mut self, rep: Resource<IoError>) -> Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

// ── wasi:io/poll ────────────────────────────────────────────────────

impl wasi::io::poll::Host for WasiCtx {
    fn poll(&mut self, in_: Vec<Resource<PollableEntry>>) -> Result<Vec<u32>> {
        // All pollables are always ready in synchronous mode.
        Ok((0..in_.len() as u32).collect())
    }
}

impl wasi::io::poll::HostPollable for WasiCtx {
    fn ready(&mut self, _self_: Resource<PollableEntry>) -> Result<bool> {
        Ok(true)
    }

    fn block(&mut self, _self_: Resource<PollableEntry>) -> Result<()> {
        Ok(())
    }

    fn drop(&mut self, rep: Resource<PollableEntry>) -> Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

// ── wasi:io/streams ─────────────────────────────────────────────────

use wasi::io::streams::StreamError;

impl wasi::io::streams::Host for WasiCtx {}

impl wasi::io::streams::HostInputStream for WasiCtx {
    fn read(
        &mut self,
        self_: Resource<InputStreamKind>,
        len: u64,
    ) -> Result<Result<Vec<u8>, StreamError>> {
        // Extract info to avoid holding a borrow across table mutations.
        let file_info = {
            let s = self.table.get(&self_)?;
            match s {
                InputStreamKind::Console => None,
                InputStreamKind::File { fd, position } => Some((*fd, *position)),
            }
        };

        match file_info {
            None => {
                let mut buf = vec![0u8; (len as usize).min(4096)];
                match sys_kernel_console_read(
                    KernelConsoleSource::Console,
                    &mut buf,
                    KernelConsoleReadFlags::NONBLOCKING,
                ) {
                    Ok(n) if n > 0 => {
                        buf.truncate(n);
                        Ok(Ok(buf))
                    }
                    _ => Ok(Ok(Vec::new())),
                }
            }
            Some((fd, pos)) => {
                let mut buf = vec![0u8; (len as usize).min(65536)];
                let mut ctx = IoCtx::new(Some(pos), IoFlags::empty(), None);
                match twizzler_rt_abi::io::twz_rt_fd_pread(fd, &mut buf, &mut ctx) {
                    Ok(0) => Ok(Err(StreamError::Closed)),
                    Ok(n) => {
                        if let Ok(s) = self.table.get_mut(&self_) {
                            if let InputStreamKind::File { position, .. } = s {
                                *position += n as u64;
                            }
                        }
                        buf.truncate(n);
                        Ok(Ok(buf))
                    }
                    Err(e) => {
                        let err = self.table.push(IoError(format!("{e}")))?;
                        Ok(Err(StreamError::LastOperationFailed(err)))
                    }
                }
            }
        }
    }

    fn blocking_read(
        &mut self,
        self_: Resource<InputStreamKind>,
        len: u64,
    ) -> Result<Result<Vec<u8>, StreamError>> {
        let is_console = matches!(self.table.get(&self_)?, InputStreamKind::Console);

        if is_console {
            // For console stdin, do a blocking read.
            let mut buf = vec![0u8; (len as usize).min(4096)];
            match sys_kernel_console_read(
                KernelConsoleSource::Console,
                &mut buf,
                KernelConsoleReadFlags::empty(),
            ) {
                Ok(n) if n > 0 => {
                    buf.truncate(n);
                    Ok(Ok(buf))
                }
                _ => Ok(Ok(Vec::new())),
            }
        } else {
            self.read(self_, len)
        }
    }

    fn skip(
        &mut self,
        self_: Resource<InputStreamKind>,
        len: u64,
    ) -> Result<Result<u64, StreamError>> {
        match self.read(self_, len)? {
            Ok(data) => Ok(Ok(data.len() as u64)),
            Err(e) => Ok(Err(e)),
        }
    }

    fn blocking_skip(
        &mut self,
        self_: Resource<InputStreamKind>,
        len: u64,
    ) -> Result<Result<u64, StreamError>> {
        self.skip(self_, len)
    }

    fn subscribe(
        &mut self,
        _self_: Resource<InputStreamKind>,
    ) -> Result<Resource<PollableEntry>> {
        Ok(self.table.push(PollableEntry)?)
    }

    fn drop(&mut self, rep: Resource<InputStreamKind>) -> Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

impl wasi::io::streams::HostOutputStream for WasiCtx {
    fn check_write(
        &mut self,
        _self_: Resource<OutputStreamKind>,
    ) -> Result<Result<u64, StreamError>> {
        Ok(Ok(usize::MAX as u64))
    }

    fn write(
        &mut self,
        self_: Resource<OutputStreamKind>,
        contents: Vec<u8>,
    ) -> Result<Result<(), StreamError>> {
        let file_info = {
            let s = self.table.get(&self_)?;
            match s {
                OutputStreamKind::Console => None,
                OutputStreamKind::File {
                    fd,
                    position,
                    append,
                } => Some((*fd, *position, *append)),
            }
        };

        match file_info {
            None => {
                sys_kernel_console_write(
                    KernelConsoleSource::Console,
                    &contents,
                    KernelConsoleWriteFlags::empty(),
                );
                Ok(Ok(()))
            }
            Some((fd, pos, is_append)) => {
                let offset = if is_append { None } else { Some(pos) };
                let mut ctx = IoCtx::new(offset, IoFlags::empty(), None);
                match twizzler_rt_abi::io::twz_rt_fd_pwrite(fd, &contents, &mut ctx) {
                    Ok(n) => {
                        if !is_append {
                            if let Ok(s) = self.table.get_mut(&self_) {
                                if let OutputStreamKind::File { position, .. } = s {
                                    *position += n as u64;
                                }
                            }
                        }
                        Ok(Ok(()))
                    }
                    Err(e) => {
                        let err = self.table.push(IoError(format!("{e}")))?;
                        Ok(Err(StreamError::LastOperationFailed(err)))
                    }
                }
            }
        }
    }

    fn blocking_write_and_flush(
        &mut self,
        self_: Resource<OutputStreamKind>,
        contents: Vec<u8>,
    ) -> Result<Result<(), StreamError>> {
        self.write(self_, contents)
    }

    fn flush(
        &mut self,
        _self_: Resource<OutputStreamKind>,
    ) -> Result<Result<(), StreamError>> {
        Ok(Ok(()))
    }

    fn blocking_flush(
        &mut self,
        _self_: Resource<OutputStreamKind>,
    ) -> Result<Result<(), StreamError>> {
        Ok(Ok(()))
    }

    fn subscribe(
        &mut self,
        _self_: Resource<OutputStreamKind>,
    ) -> Result<Resource<PollableEntry>> {
        Ok(self.table.push(PollableEntry)?)
    }

    fn write_zeroes(
        &mut self,
        self_: Resource<OutputStreamKind>,
        len: u64,
    ) -> Result<Result<(), StreamError>> {
        let zeros = vec![0u8; (len as usize).min(65536)];
        self.write(self_, zeros)
    }

    fn blocking_write_zeroes_and_flush(
        &mut self,
        self_: Resource<OutputStreamKind>,
        len: u64,
    ) -> Result<Result<(), StreamError>> {
        self.write_zeroes(self_, len)
    }

    fn splice(
        &mut self,
        _self_: Resource<OutputStreamKind>,
        _src: Resource<InputStreamKind>,
        _len: u64,
    ) -> Result<Result<u64, StreamError>> {
        let err = self
            .table
            .push(IoError("splice not supported".to_string()))?;
        Ok(Err(StreamError::LastOperationFailed(err)))
    }

    fn blocking_splice(
        &mut self,
        self_: Resource<OutputStreamKind>,
        src: Resource<InputStreamKind>,
        len: u64,
    ) -> Result<Result<u64, StreamError>> {
        self.splice(self_, src, len)
    }

    fn drop(&mut self, rep: Resource<OutputStreamKind>) -> Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

// ── wasi:clocks ─────────────────────────────────────────────────────

impl wasi::clocks::monotonic_clock::Host for WasiCtx {
    fn now(&mut self) -> Result<wasi::clocks::monotonic_clock::Instant> {
        Ok(twizzler_rt_abi::time::twz_rt_get_monotonic_time().as_nanos() as u64)
    }

    fn resolution(&mut self) -> Result<wasi::clocks::monotonic_clock::Duration> {
        Ok(1)
    }

    fn subscribe_duration(
        &mut self,
        _duration: wasi::clocks::monotonic_clock::Duration,
    ) -> Result<Resource<PollableEntry>> {
        Ok(self.table.push(PollableEntry)?)
    }

    fn subscribe_instant(
        &mut self,
        _deadline: wasi::clocks::monotonic_clock::Instant,
    ) -> Result<Resource<PollableEntry>> {
        Ok(self.table.push(PollableEntry)?)
    }
}

impl wasi::clocks::wall_clock::Host for WasiCtx {
    fn now(&mut self) -> Result<wasi::clocks::wall_clock::Datetime> {
        let t = twizzler_rt_abi::time::twz_rt_get_system_time();
        Ok(wasi::clocks::wall_clock::Datetime {
            seconds: t.as_secs(),
            nanoseconds: t.subsec_nanos(),
        })
    }

    fn resolution(&mut self) -> Result<wasi::clocks::wall_clock::Datetime> {
        Ok(wasi::clocks::wall_clock::Datetime {
            seconds: 0,
            nanoseconds: 1,
        })
    }
}

// ── wasi:cli ────────────────────────────────────────────────────────

impl wasi::cli::environment::Host for WasiCtx {
    fn get_arguments(&mut self) -> Result<Vec<String>> {
        Ok(std::env::args().collect())
    }

    fn get_environment(&mut self) -> Result<Vec<(String, String)>> {
        Ok(std::env::vars().collect())
    }

    fn initial_cwd(&mut self) -> Result<Option<String>> {
        Ok(Some("/".to_string()))
    }
}

impl wasi::cli::exit::Host for WasiCtx {
    fn exit(&mut self, code: core::result::Result<(), ()>) -> Result<()> {
        if code.is_ok() {
            bail!("wasi exit success")
        } else {
            bail!("wasi exit error")
        }
    }

    fn exit_with_code(&mut self, code: u8) -> Result<()> {
        bail!("wasi exit with code {code}")
    }
}

impl wasi::cli::stdin::Host for WasiCtx {
    fn get_stdin(&mut self) -> Result<Resource<InputStreamKind>> {
        Ok(self.table.push(InputStreamKind::Console)?)
    }
}

impl wasi::cli::stdout::Host for WasiCtx {
    fn get_stdout(&mut self) -> Result<Resource<OutputStreamKind>> {
        Ok(self.table.push(OutputStreamKind::Console)?)
    }
}

impl wasi::cli::stderr::Host for WasiCtx {
    fn get_stderr(&mut self) -> Result<Resource<OutputStreamKind>> {
        Ok(self.table.push(OutputStreamKind::Console)?)
    }
}

impl wasi::cli::terminal_input::Host for WasiCtx {}
impl wasi::cli::terminal_input::HostTerminalInput for WasiCtx {
    fn drop(&mut self, rep: Resource<TerminalInput>) -> Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

impl wasi::cli::terminal_output::Host for WasiCtx {}
impl wasi::cli::terminal_output::HostTerminalOutput for WasiCtx {
    fn drop(&mut self, rep: Resource<TerminalOutput>) -> Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

impl wasi::cli::terminal_stdin::Host for WasiCtx {
    fn get_terminal_stdin(&mut self) -> Result<Option<Resource<TerminalInput>>> {
        Ok(None) // Not a real terminal.
    }
}

impl wasi::cli::terminal_stdout::Host for WasiCtx {
    fn get_terminal_stdout(&mut self) -> Result<Option<Resource<TerminalOutput>>> {
        Ok(None)
    }
}

impl wasi::cli::terminal_stderr::Host for WasiCtx {
    fn get_terminal_stderr(&mut self) -> Result<Option<Resource<TerminalOutput>>> {
        Ok(None)
    }
}

// ── wasi:random ─────────────────────────────────────────────────────

fn random_bytes(len: u64) -> Result<Vec<u8>> {
    let mut buf = vec![0u8; len as usize];
    let dest = unsafe {
        core::slice::from_raw_parts_mut(
            buf.as_mut_ptr() as *mut MaybeUninit<u8>,
            buf.len(),
        )
    };
    sys_get_random(dest, GetRandomFlags::empty())
        .map_err(|e| anyhow::anyhow!("sys_get_random failed: {e}"))?;
    Ok(buf)
}

fn random_u64() -> Result<u64> {
    let bytes = random_bytes(8)?;
    Ok(u64::from_le_bytes(bytes[..8].try_into().unwrap()))
}

impl wasi::random::random::Host for WasiCtx {
    fn get_random_bytes(&mut self, len: u64) -> Result<Vec<u8>> {
        random_bytes(len)
    }

    fn get_random_u64(&mut self) -> Result<u64> {
        random_u64()
    }
}

impl wasi::random::insecure::Host for WasiCtx {
    fn get_insecure_random_bytes(&mut self, len: u64) -> Result<Vec<u8>> {
        random_bytes(len)
    }

    fn get_insecure_random_u64(&mut self) -> Result<u64> {
        random_u64()
    }
}

impl wasi::random::insecure_seed::Host for WasiCtx {
    fn insecure_seed(&mut self) -> Result<(u64, u64)> {
        Ok((random_u64()?, random_u64()?))
    }
}

// ── wasi:filesystem/preopens ────────────────────────────────────────

impl wasi::filesystem::preopens::Host for WasiCtx {
    fn get_directories(
        &mut self,
    ) -> Result<Vec<(Resource<DescriptorEntry>, String)>> {
        let create = twizzler_rt_abi::bindings::create_options {
            id: Default::default(),
            kind: twizzler_rt_abi::bindings::CREATE_KIND_EXISTING,
        };
        match fd::twz_rt_fd_open("/", create, twizzler_rt_abi::bindings::OPEN_FLAG_READ) {
            Ok(raw_fd) => {
                let entry = DescriptorEntry {
                    fd: raw_fd,
                    path: "/".to_string(),
                };
                let desc = self.table.push(entry)?;
                Ok(vec![(desc, "/".to_string())])
            }
            Err(_) => Ok(Vec::new()),
        }
    }
}

// ── wasi:filesystem/types ───────────────────────────────────────────

use wasi::filesystem::types::ErrorCode;

impl wasi::filesystem::types::Host for WasiCtx {
    fn filesystem_error_code(
        &mut self,
        _err: Resource<IoError>,
    ) -> Result<Option<ErrorCode>> {
        Ok(None)
    }
}

impl wasi::filesystem::types::HostDescriptor for WasiCtx {
    fn read_via_stream(
        &mut self,
        desc: Resource<DescriptorEntry>,
        offset: u64,
    ) -> Result<Result<Resource<InputStreamKind>, ErrorCode>> {
        let entry = self.table.get(&desc)?;
        let stream = InputStreamKind::File {
            fd: entry.fd,
            position: offset,
        };
        Ok(Ok(self.table.push(stream)?))
    }

    fn write_via_stream(
        &mut self,
        desc: Resource<DescriptorEntry>,
        offset: u64,
    ) -> Result<Result<Resource<OutputStreamKind>, ErrorCode>> {
        let entry = self.table.get(&desc)?;
        let stream = OutputStreamKind::File {
            fd: entry.fd,
            position: offset,
            append: false,
        };
        Ok(Ok(self.table.push(stream)?))
    }

    fn append_via_stream(
        &mut self,
        desc: Resource<DescriptorEntry>,
    ) -> Result<Result<Resource<OutputStreamKind>, ErrorCode>> {
        let entry = self.table.get(&desc)?;
        let stream = OutputStreamKind::File {
            fd: entry.fd,
            position: 0,
            append: true,
        };
        Ok(Ok(self.table.push(stream)?))
    }

    fn advise(
        &mut self,
        _desc: Resource<DescriptorEntry>,
        _offset: u64,
        _length: u64,
        _advice: wasi::filesystem::types::Advice,
    ) -> Result<Result<(), ErrorCode>> {
        Ok(Ok(())) // Advisory only.
    }

    fn sync_data(
        &mut self,
        desc: Resource<DescriptorEntry>,
    ) -> Result<Result<(), ErrorCode>> {
        let entry = self.table.get(&desc)?;
        fd::twz_rt_fd_sync(entry.fd);
        Ok(Ok(()))
    }

    fn get_flags(
        &mut self,
        _desc: Resource<DescriptorEntry>,
    ) -> Result<Result<wasi::filesystem::types::DescriptorFlags, ErrorCode>> {
        Ok(Ok(
            wasi::filesystem::types::DescriptorFlags::READ
                | wasi::filesystem::types::DescriptorFlags::WRITE,
        ))
    }

    fn get_type(
        &mut self,
        desc: Resource<DescriptorEntry>,
    ) -> Result<Result<wasi::filesystem::types::DescriptorType, ErrorCode>> {
        let entry = self.table.get(&desc)?;
        match fd::twz_rt_fd_get_info(entry.fd) {
            Ok(info) => Ok(Ok(fd_kind_to_desc_type(info.kind))),
            Err(e) => Ok(Err(twz_err_to_wasi(e))),
        }
    }

    fn set_size(
        &mut self,
        desc: Resource<DescriptorEntry>,
        size: u64,
    ) -> Result<Result<(), ErrorCode>> {
        let entry = self.table.get(&desc)?;
        match fd::twz_rt_fd_truncate(entry.fd, size) {
            Ok(()) => Ok(Ok(())),
            Err(e) => Ok(Err(twz_err_to_wasi(e))),
        }
    }

    fn set_times(
        &mut self,
        _desc: Resource<DescriptorEntry>,
        _data_access: wasi::filesystem::types::NewTimestamp,
        _data_modification: wasi::filesystem::types::NewTimestamp,
    ) -> Result<Result<(), ErrorCode>> {
        Ok(Err(ErrorCode::Unsupported))
    }

    fn read(
        &mut self,
        desc: Resource<DescriptorEntry>,
        length: u64,
        offset: u64,
    ) -> Result<Result<(Vec<u8>, bool), ErrorCode>> {
        let entry = self.table.get(&desc)?;
        let mut buf = vec![0u8; length as usize];
        let mut ctx = IoCtx::new(Some(offset), IoFlags::empty(), None);
        match twizzler_rt_abi::io::twz_rt_fd_pread(entry.fd, &mut buf, &mut ctx) {
            Ok(n) => {
                buf.truncate(n);
                let eof = n < length as usize;
                Ok(Ok((buf, eof)))
            }
            Err(e) => Ok(Err(twz_err_to_wasi(e))),
        }
    }

    fn write(
        &mut self,
        desc: Resource<DescriptorEntry>,
        buffer: Vec<u8>,
        offset: u64,
    ) -> Result<Result<u64, ErrorCode>> {
        let entry = self.table.get(&desc)?;
        let mut ctx = IoCtx::new(Some(offset), IoFlags::empty(), None);
        match twizzler_rt_abi::io::twz_rt_fd_pwrite(entry.fd, &buffer, &mut ctx) {
            Ok(n) => Ok(Ok(n as u64)),
            Err(e) => Ok(Err(twz_err_to_wasi(e))),
        }
    }

    fn read_directory(
        &mut self,
        desc: Resource<DescriptorEntry>,
    ) -> Result<Result<Resource<DirEntryStream>, ErrorCode>> {
        let entry = self.table.get(&desc)?;
        let stream = DirEntryStream {
            fd: entry.fd,
            offset: 0,
            buffer: Vec::new(),
            buffer_idx: 0,
        };
        Ok(Ok(self.table.push(stream)?))
    }

    fn sync(
        &mut self,
        desc: Resource<DescriptorEntry>,
    ) -> Result<Result<(), ErrorCode>> {
        let entry = self.table.get(&desc)?;
        fd::twz_rt_fd_sync(entry.fd);
        Ok(Ok(()))
    }

    fn create_directory_at(
        &mut self,
        desc: Resource<DescriptorEntry>,
        path: String,
    ) -> Result<Result<(), ErrorCode>> {
        let entry = self.table.get(&desc)?;
        let full = join_path(&entry.path, &path);
        match fd::twz_rt_fd_mkns(&full) {
            Ok(()) => Ok(Ok(())),
            Err(e) => Ok(Err(twz_err_to_wasi(e))),
        }
    }

    fn stat(
        &mut self,
        desc: Resource<DescriptorEntry>,
    ) -> Result<Result<wasi::filesystem::types::DescriptorStat, ErrorCode>> {
        let entry = self.table.get(&desc)?;
        match fd::twz_rt_fd_get_info(entry.fd) {
            Ok(info) => Ok(Ok(fd_info_to_stat(&info))),
            Err(e) => Ok(Err(twz_err_to_wasi(e))),
        }
    }

    fn stat_at(
        &mut self,
        desc: Resource<DescriptorEntry>,
        _path_flags: wasi::filesystem::types::PathFlags,
        path: String,
    ) -> Result<Result<wasi::filesystem::types::DescriptorStat, ErrorCode>> {
        let entry = self.table.get(&desc)?;
        let full = join_path(&entry.path, &path);
        let create = twizzler_rt_abi::bindings::create_options {
            id: Default::default(),
            kind: twizzler_rt_abi::bindings::CREATE_KIND_EXISTING,
        };
        match fd::twz_rt_fd_open(&full, create, twizzler_rt_abi::bindings::OPEN_FLAG_READ) {
            Ok(child_fd) => {
                let result = match fd::twz_rt_fd_get_info(child_fd) {
                    Ok(info) => Ok(fd_info_to_stat(&info)),
                    Err(e) => Err(twz_err_to_wasi(e)),
                };
                fd::twz_rt_fd_close(child_fd);
                Ok(result)
            }
            Err(e) => Ok(Err(twz_err_to_wasi(e))),
        }
    }

    fn set_times_at(
        &mut self,
        _desc: Resource<DescriptorEntry>,
        _path_flags: wasi::filesystem::types::PathFlags,
        _path: String,
        _data_access: wasi::filesystem::types::NewTimestamp,
        _data_modification: wasi::filesystem::types::NewTimestamp,
    ) -> Result<Result<(), ErrorCode>> {
        Ok(Err(ErrorCode::Unsupported))
    }

    fn link_at(
        &mut self,
        _desc: Resource<DescriptorEntry>,
        _old_path_flags: wasi::filesystem::types::PathFlags,
        _old_path: String,
        _new_desc: Resource<DescriptorEntry>,
        _new_path: String,
    ) -> Result<Result<(), ErrorCode>> {
        Ok(Err(ErrorCode::Unsupported))
    }

    fn open_at(
        &mut self,
        desc: Resource<DescriptorEntry>,
        _path_flags: wasi::filesystem::types::PathFlags,
        path: String,
        open_flags: wasi::filesystem::types::OpenFlags,
        desc_flags: wasi::filesystem::types::DescriptorFlags,
    ) -> Result<Result<Resource<DescriptorEntry>, ErrorCode>> {
        let entry = self.table.get(&desc)?;
        let full = join_path(&entry.path, &path);

        let kind = if open_flags.contains(wasi::filesystem::types::OpenFlags::CREATE) {
            if open_flags.contains(wasi::filesystem::types::OpenFlags::EXCLUSIVE) {
                twizzler_rt_abi::bindings::CREATE_KIND_NEW
            } else {
                twizzler_rt_abi::bindings::CREATE_KIND_EITHER
            }
        } else {
            twizzler_rt_abi::bindings::CREATE_KIND_EXISTING
        };

        // If DIRECTORY + CREATE, ensure the namespace exists.
        if open_flags.contains(wasi::filesystem::types::OpenFlags::DIRECTORY)
            && open_flags.contains(wasi::filesystem::types::OpenFlags::CREATE)
        {
            match fd::twz_rt_fd_mkns(&full) {
                Ok(()) => {}
                Err(TwzError::Naming(twizzler_rt_abi::error::NamingError::AlreadyExists)) => {}
                Err(e) => return Ok(Err(twz_err_to_wasi(e))),
            }
        }

        let create = twizzler_rt_abi::bindings::create_options {
            id: Default::default(),
            kind,
        };

        let mut flags = 0u32;
        if desc_flags.contains(wasi::filesystem::types::DescriptorFlags::READ) {
            flags |= twizzler_rt_abi::bindings::OPEN_FLAG_READ;
        }
        if desc_flags.contains(wasi::filesystem::types::DescriptorFlags::WRITE) {
            flags |= twizzler_rt_abi::bindings::OPEN_FLAG_WRITE;
        }
        if open_flags.contains(wasi::filesystem::types::OpenFlags::TRUNCATE) {
            flags |= twizzler_rt_abi::bindings::OPEN_FLAG_TRUNCATE;
        }

        match fd::twz_rt_fd_open(&full, create, flags) {
            Ok(child_fd) => {
                let child_entry = DescriptorEntry {
                    fd: child_fd,
                    path: full,
                };
                Ok(Ok(self.table.push(child_entry)?))
            }
            Err(e) => Ok(Err(twz_err_to_wasi(e))),
        }
    }

    fn readlink_at(
        &mut self,
        desc: Resource<DescriptorEntry>,
        path: String,
    ) -> Result<Result<String, ErrorCode>> {
        let entry = self.table.get(&desc)?;
        let full = join_path(&entry.path, &path);
        let mut buf = vec![0u8; 4096];
        match fd::twz_rt_fd_readlink(&full, &mut buf) {
            Ok(n) => {
                buf.truncate(n);
                Ok(Ok(String::from_utf8_lossy(&buf).to_string()))
            }
            Err(e) => Ok(Err(twz_err_to_wasi(e))),
        }
    }

    fn remove_directory_at(
        &mut self,
        desc: Resource<DescriptorEntry>,
        path: String,
    ) -> Result<Result<(), ErrorCode>> {
        let entry = self.table.get(&desc)?;
        let full = join_path(&entry.path, &path);
        match fd::twz_rt_fd_remove(&full) {
            Ok(()) => Ok(Ok(())),
            Err(e) => Ok(Err(twz_err_to_wasi(e))),
        }
    }

    fn rename_at(
        &mut self,
        _desc: Resource<DescriptorEntry>,
        _old_path: String,
        _new_desc: Resource<DescriptorEntry>,
        _new_path: String,
    ) -> Result<Result<(), ErrorCode>> {
        Ok(Err(ErrorCode::Unsupported))
    }

    fn symlink_at(
        &mut self,
        desc: Resource<DescriptorEntry>,
        old_path: String,
        new_path: String,
    ) -> Result<Result<(), ErrorCode>> {
        let entry = self.table.get(&desc)?;
        let full_new = join_path(&entry.path, &new_path);
        match fd::twz_rt_fd_symlink(&full_new, &old_path) {
            Ok(()) => Ok(Ok(())),
            Err(e) => Ok(Err(twz_err_to_wasi(e))),
        }
    }

    fn unlink_file_at(
        &mut self,
        desc: Resource<DescriptorEntry>,
        path: String,
    ) -> Result<Result<(), ErrorCode>> {
        let entry = self.table.get(&desc)?;
        let full = join_path(&entry.path, &path);
        match fd::twz_rt_fd_remove(&full) {
            Ok(()) => Ok(Ok(())),
            Err(e) => Ok(Err(twz_err_to_wasi(e))),
        }
    }

    fn is_same_object(
        &mut self,
        a: Resource<DescriptorEntry>,
        b: Resource<DescriptorEntry>,
    ) -> Result<bool> {
        let a_entry = self.table.get(&a)?;
        let b_entry = self.table.get(&b)?;
        Ok(a_entry.path == b_entry.path)
    }

    fn metadata_hash(
        &mut self,
        desc: Resource<DescriptorEntry>,
    ) -> Result<Result<wasi::filesystem::types::MetadataHashValue, ErrorCode>> {
        let entry = self.table.get(&desc)?;
        match fd::twz_rt_fd_get_info(entry.fd) {
            Ok(info) => {
                let id_val: u128 = info.id.into();
                Ok(Ok(wasi::filesystem::types::MetadataHashValue {
                    upper: (id_val >> 64) as u64,
                    lower: id_val as u64,
                }))
            }
            Err(e) => Ok(Err(twz_err_to_wasi(e))),
        }
    }

    fn metadata_hash_at(
        &mut self,
        desc: Resource<DescriptorEntry>,
        _path_flags: wasi::filesystem::types::PathFlags,
        path: String,
    ) -> Result<Result<wasi::filesystem::types::MetadataHashValue, ErrorCode>> {
        let entry = self.table.get(&desc)?;
        let full = join_path(&entry.path, &path);
        let create = twizzler_rt_abi::bindings::create_options {
            id: Default::default(),
            kind: twizzler_rt_abi::bindings::CREATE_KIND_EXISTING,
        };
        match fd::twz_rt_fd_open(&full, create, twizzler_rt_abi::bindings::OPEN_FLAG_READ) {
            Ok(child_fd) => {
                let result = match fd::twz_rt_fd_get_info(child_fd) {
                    Ok(info) => {
                        let id_val: u128 = info.id.into();
                        Ok(wasi::filesystem::types::MetadataHashValue {
                            upper: (id_val >> 64) as u64,
                            lower: id_val as u64,
                        })
                    }
                    Err(e) => Err(twz_err_to_wasi(e)),
                };
                fd::twz_rt_fd_close(child_fd);
                Ok(result)
            }
            Err(e) => Ok(Err(twz_err_to_wasi(e))),
        }
    }

    fn drop(&mut self, desc: Resource<DescriptorEntry>) -> Result<()> {
        self.table.delete(desc)?;
        Ok(())
    }
}

impl wasi::filesystem::types::HostDirectoryEntryStream for WasiCtx {
    fn read_directory_entry(
        &mut self,
        stream: Resource<DirEntryStream>,
    ) -> Result<Result<Option<wasi::filesystem::types::DirectoryEntry>, ErrorCode>> {
        let data = self.table.get_mut(&stream)?;

        // Refill buffer if exhausted.
        if data.buffer_idx >= data.buffer.len() {
            let mut entries =
                vec![unsafe { core::mem::zeroed::<NameEntry>() }; 32];
            match fd::twz_rt_fd_enumerate_names(data.fd, &mut entries, data.offset) {
                Ok(0) => return Ok(Ok(None)),
                Ok(n) => {
                    data.buffer = entries[..n].to_vec();
                    data.buffer_idx = 0;
                    data.offset += n;
                }
                Err(e) => return Ok(Err(twz_err_to_wasi(e))),
            }
        }

        let entry = &data.buffer[data.buffer_idx];
        data.buffer_idx += 1;

        let name = String::from_utf8_lossy(entry.name_bytes()).to_string();
        let kind = FdKind::from(entry.info.kind);

        Ok(Ok(Some(wasi::filesystem::types::DirectoryEntry {
            type_: fd_kind_to_desc_type(kind),
            name,
        })))
    }

    fn drop(&mut self, stream: Resource<DirEntryStream>) -> Result<()> {
        self.table.delete(stream)?;
        Ok(())
    }
}

// ── wasi:sockets stubs ──────────────────────────────────────────────
// Network sockets are not supported on Twizzler yet.
// All create operations return ErrorCode::NotSupported.
// Resource methods bail since no sockets are ever created.

impl wasi::sockets::instance_network::Host for WasiCtx {
    fn instance_network(
        &mut self,
    ) -> Result<Resource<wasi::sockets::network::Network>> {
        bail!("networking not supported on Twizzler")
    }
}

impl wasi::sockets::network::Host for WasiCtx {
    fn network_error_code(
        &mut self,
        _err: Resource<IoError>,
    ) -> Result<Option<wasi::sockets::network::ErrorCode>> {
        Ok(None)
    }
}

impl wasi::sockets::network::HostNetwork for WasiCtx {
    fn drop(
        &mut self,
        _rep: Resource<wasi::sockets::network::Network>,
    ) -> Result<()> {
        bail!("unreachable: network not created")
    }
}

impl wasi::sockets::tcp_create_socket::Host for WasiCtx {
    fn create_tcp_socket(
        &mut self,
        _address_family: wasi::sockets::network::IpAddressFamily,
    ) -> Result<Result<Resource<wasi::sockets::tcp::TcpSocket>, wasi::sockets::network::ErrorCode>>
    {
        Ok(Err(wasi::sockets::network::ErrorCode::NotSupported))
    }
}

impl wasi::sockets::tcp::Host for WasiCtx {}

impl wasi::sockets::tcp::HostTcpSocket for WasiCtx {
    fn start_bind(
        &mut self,
        _self_: Resource<wasi::sockets::tcp::TcpSocket>,
        _network: Resource<wasi::sockets::network::Network>,
        _local_address: wasi::sockets::network::IpSocketAddress,
    ) -> Result<Result<(), wasi::sockets::network::ErrorCode>> {
        bail!("unreachable: tcp socket not created")
    }
    fn finish_bind(
        &mut self,
        _self_: Resource<wasi::sockets::tcp::TcpSocket>,
    ) -> Result<Result<(), wasi::sockets::network::ErrorCode>> {
        bail!("unreachable: tcp socket not created")
    }
    fn start_connect(
        &mut self,
        _self_: Resource<wasi::sockets::tcp::TcpSocket>,
        _network: Resource<wasi::sockets::network::Network>,
        _remote_address: wasi::sockets::network::IpSocketAddress,
    ) -> Result<Result<(), wasi::sockets::network::ErrorCode>> {
        bail!("unreachable: tcp socket not created")
    }
    fn finish_connect(
        &mut self,
        _self_: Resource<wasi::sockets::tcp::TcpSocket>,
    ) -> Result<
        Result<
            (Resource<InputStreamKind>, Resource<OutputStreamKind>),
            wasi::sockets::network::ErrorCode,
        >,
    > {
        bail!("unreachable: tcp socket not created")
    }
    fn start_listen(
        &mut self,
        _self_: Resource<wasi::sockets::tcp::TcpSocket>,
    ) -> Result<Result<(), wasi::sockets::network::ErrorCode>> {
        bail!("unreachable: tcp socket not created")
    }
    fn finish_listen(
        &mut self,
        _self_: Resource<wasi::sockets::tcp::TcpSocket>,
    ) -> Result<Result<(), wasi::sockets::network::ErrorCode>> {
        bail!("unreachable: tcp socket not created")
    }
    fn accept(
        &mut self,
        _self_: Resource<wasi::sockets::tcp::TcpSocket>,
    ) -> Result<
        Result<
            (
                Resource<wasi::sockets::tcp::TcpSocket>,
                Resource<InputStreamKind>,
                Resource<OutputStreamKind>,
            ),
            wasi::sockets::network::ErrorCode,
        >,
    > {
        bail!("unreachable: tcp socket not created")
    }
    fn local_address(
        &mut self,
        _self_: Resource<wasi::sockets::tcp::TcpSocket>,
    ) -> Result<Result<wasi::sockets::network::IpSocketAddress, wasi::sockets::network::ErrorCode>>
    {
        bail!("unreachable: tcp socket not created")
    }
    fn remote_address(
        &mut self,
        _self_: Resource<wasi::sockets::tcp::TcpSocket>,
    ) -> Result<Result<wasi::sockets::network::IpSocketAddress, wasi::sockets::network::ErrorCode>>
    {
        bail!("unreachable: tcp socket not created")
    }
    fn is_listening(
        &mut self,
        _self_: Resource<wasi::sockets::tcp::TcpSocket>,
    ) -> Result<bool> {
        bail!("unreachable: tcp socket not created")
    }
    fn address_family(
        &mut self,
        _self_: Resource<wasi::sockets::tcp::TcpSocket>,
    ) -> Result<wasi::sockets::network::IpAddressFamily> {
        bail!("unreachable: tcp socket not created")
    }
    fn set_listen_backlog_size(
        &mut self,
        _self_: Resource<wasi::sockets::tcp::TcpSocket>,
        _value: u64,
    ) -> Result<Result<(), wasi::sockets::network::ErrorCode>> {
        bail!("unreachable: tcp socket not created")
    }
    fn keep_alive_enabled(
        &mut self,
        _self_: Resource<wasi::sockets::tcp::TcpSocket>,
    ) -> Result<Result<bool, wasi::sockets::network::ErrorCode>> {
        bail!("unreachable: tcp socket not created")
    }
    fn set_keep_alive_enabled(
        &mut self,
        _self_: Resource<wasi::sockets::tcp::TcpSocket>,
        _value: bool,
    ) -> Result<Result<(), wasi::sockets::network::ErrorCode>> {
        bail!("unreachable: tcp socket not created")
    }
    fn keep_alive_idle_time(
        &mut self,
        _self_: Resource<wasi::sockets::tcp::TcpSocket>,
    ) -> Result<Result<wasi::clocks::monotonic_clock::Duration, wasi::sockets::network::ErrorCode>>
    {
        bail!("unreachable: tcp socket not created")
    }
    fn set_keep_alive_idle_time(
        &mut self,
        _self_: Resource<wasi::sockets::tcp::TcpSocket>,
        _value: wasi::clocks::monotonic_clock::Duration,
    ) -> Result<Result<(), wasi::sockets::network::ErrorCode>> {
        bail!("unreachable: tcp socket not created")
    }
    fn keep_alive_interval(
        &mut self,
        _self_: Resource<wasi::sockets::tcp::TcpSocket>,
    ) -> Result<Result<wasi::clocks::monotonic_clock::Duration, wasi::sockets::network::ErrorCode>>
    {
        bail!("unreachable: tcp socket not created")
    }
    fn set_keep_alive_interval(
        &mut self,
        _self_: Resource<wasi::sockets::tcp::TcpSocket>,
        _value: wasi::clocks::monotonic_clock::Duration,
    ) -> Result<Result<(), wasi::sockets::network::ErrorCode>> {
        bail!("unreachable: tcp socket not created")
    }
    fn keep_alive_count(
        &mut self,
        _self_: Resource<wasi::sockets::tcp::TcpSocket>,
    ) -> Result<Result<u32, wasi::sockets::network::ErrorCode>> {
        bail!("unreachable: tcp socket not created")
    }
    fn set_keep_alive_count(
        &mut self,
        _self_: Resource<wasi::sockets::tcp::TcpSocket>,
        _value: u32,
    ) -> Result<Result<(), wasi::sockets::network::ErrorCode>> {
        bail!("unreachable: tcp socket not created")
    }
    fn hop_limit(
        &mut self,
        _self_: Resource<wasi::sockets::tcp::TcpSocket>,
    ) -> Result<Result<u8, wasi::sockets::network::ErrorCode>> {
        bail!("unreachable: tcp socket not created")
    }
    fn set_hop_limit(
        &mut self,
        _self_: Resource<wasi::sockets::tcp::TcpSocket>,
        _value: u8,
    ) -> Result<Result<(), wasi::sockets::network::ErrorCode>> {
        bail!("unreachable: tcp socket not created")
    }
    fn receive_buffer_size(
        &mut self,
        _self_: Resource<wasi::sockets::tcp::TcpSocket>,
    ) -> Result<Result<u64, wasi::sockets::network::ErrorCode>> {
        bail!("unreachable: tcp socket not created")
    }
    fn set_receive_buffer_size(
        &mut self,
        _self_: Resource<wasi::sockets::tcp::TcpSocket>,
        _value: u64,
    ) -> Result<Result<(), wasi::sockets::network::ErrorCode>> {
        bail!("unreachable: tcp socket not created")
    }
    fn send_buffer_size(
        &mut self,
        _self_: Resource<wasi::sockets::tcp::TcpSocket>,
    ) -> Result<Result<u64, wasi::sockets::network::ErrorCode>> {
        bail!("unreachable: tcp socket not created")
    }
    fn set_send_buffer_size(
        &mut self,
        _self_: Resource<wasi::sockets::tcp::TcpSocket>,
        _value: u64,
    ) -> Result<Result<(), wasi::sockets::network::ErrorCode>> {
        bail!("unreachable: tcp socket not created")
    }
    fn subscribe(
        &mut self,
        _self_: Resource<wasi::sockets::tcp::TcpSocket>,
    ) -> Result<Resource<PollableEntry>> {
        bail!("unreachable: tcp socket not created")
    }
    fn shutdown(
        &mut self,
        _self_: Resource<wasi::sockets::tcp::TcpSocket>,
        _shutdown_type: wasi::sockets::tcp::ShutdownType,
    ) -> Result<Result<(), wasi::sockets::network::ErrorCode>> {
        bail!("unreachable: tcp socket not created")
    }
    fn drop(
        &mut self,
        _rep: Resource<wasi::sockets::tcp::TcpSocket>,
    ) -> Result<()> {
        bail!("unreachable: tcp socket not created")
    }
}

impl wasi::sockets::udp_create_socket::Host for WasiCtx {
    fn create_udp_socket(
        &mut self,
        _address_family: wasi::sockets::network::IpAddressFamily,
    ) -> Result<Result<Resource<wasi::sockets::udp::UdpSocket>, wasi::sockets::network::ErrorCode>>
    {
        Ok(Err(wasi::sockets::network::ErrorCode::NotSupported))
    }
}

impl wasi::sockets::udp::Host for WasiCtx {}

impl wasi::sockets::udp::HostUdpSocket for WasiCtx {
    fn start_bind(
        &mut self,
        _self_: Resource<wasi::sockets::udp::UdpSocket>,
        _network: Resource<wasi::sockets::network::Network>,
        _local_address: wasi::sockets::network::IpSocketAddress,
    ) -> Result<Result<(), wasi::sockets::network::ErrorCode>> {
        bail!("unreachable: udp socket not created")
    }
    fn finish_bind(
        &mut self,
        _self_: Resource<wasi::sockets::udp::UdpSocket>,
    ) -> Result<Result<(), wasi::sockets::network::ErrorCode>> {
        bail!("unreachable: udp socket not created")
    }
    fn stream(
        &mut self,
        _self_: Resource<wasi::sockets::udp::UdpSocket>,
        _remote_address: Option<wasi::sockets::network::IpSocketAddress>,
    ) -> Result<
        Result<
            (
                Resource<wasi::sockets::udp::IncomingDatagramStream>,
                Resource<wasi::sockets::udp::OutgoingDatagramStream>,
            ),
            wasi::sockets::network::ErrorCode,
        >,
    > {
        bail!("unreachable: udp socket not created")
    }
    fn local_address(
        &mut self,
        _self_: Resource<wasi::sockets::udp::UdpSocket>,
    ) -> Result<Result<wasi::sockets::network::IpSocketAddress, wasi::sockets::network::ErrorCode>>
    {
        bail!("unreachable: udp socket not created")
    }
    fn remote_address(
        &mut self,
        _self_: Resource<wasi::sockets::udp::UdpSocket>,
    ) -> Result<Result<wasi::sockets::network::IpSocketAddress, wasi::sockets::network::ErrorCode>>
    {
        bail!("unreachable: udp socket not created")
    }
    fn address_family(
        &mut self,
        _self_: Resource<wasi::sockets::udp::UdpSocket>,
    ) -> Result<wasi::sockets::network::IpAddressFamily> {
        bail!("unreachable: udp socket not created")
    }
    fn unicast_hop_limit(
        &mut self,
        _self_: Resource<wasi::sockets::udp::UdpSocket>,
    ) -> Result<Result<u8, wasi::sockets::network::ErrorCode>> {
        bail!("unreachable: udp socket not created")
    }
    fn set_unicast_hop_limit(
        &mut self,
        _self_: Resource<wasi::sockets::udp::UdpSocket>,
        _value: u8,
    ) -> Result<Result<(), wasi::sockets::network::ErrorCode>> {
        bail!("unreachable: udp socket not created")
    }
    fn receive_buffer_size(
        &mut self,
        _self_: Resource<wasi::sockets::udp::UdpSocket>,
    ) -> Result<Result<u64, wasi::sockets::network::ErrorCode>> {
        bail!("unreachable: udp socket not created")
    }
    fn set_receive_buffer_size(
        &mut self,
        _self_: Resource<wasi::sockets::udp::UdpSocket>,
        _value: u64,
    ) -> Result<Result<(), wasi::sockets::network::ErrorCode>> {
        bail!("unreachable: udp socket not created")
    }
    fn send_buffer_size(
        &mut self,
        _self_: Resource<wasi::sockets::udp::UdpSocket>,
    ) -> Result<Result<u64, wasi::sockets::network::ErrorCode>> {
        bail!("unreachable: udp socket not created")
    }
    fn set_send_buffer_size(
        &mut self,
        _self_: Resource<wasi::sockets::udp::UdpSocket>,
        _value: u64,
    ) -> Result<Result<(), wasi::sockets::network::ErrorCode>> {
        bail!("unreachable: udp socket not created")
    }
    fn subscribe(
        &mut self,
        _self_: Resource<wasi::sockets::udp::UdpSocket>,
    ) -> Result<Resource<PollableEntry>> {
        bail!("unreachable: udp socket not created")
    }
    fn drop(
        &mut self,
        _rep: Resource<wasi::sockets::udp::UdpSocket>,
    ) -> Result<()> {
        bail!("unreachable: udp socket not created")
    }
}

impl wasi::sockets::udp::HostIncomingDatagramStream for WasiCtx {
    fn receive(
        &mut self,
        _self_: Resource<wasi::sockets::udp::IncomingDatagramStream>,
        _max_results: u64,
    ) -> Result<
        Result<Vec<wasi::sockets::udp::IncomingDatagram>, wasi::sockets::network::ErrorCode>,
    > {
        bail!("unreachable: udp socket not created")
    }
    fn subscribe(
        &mut self,
        _self_: Resource<wasi::sockets::udp::IncomingDatagramStream>,
    ) -> Result<Resource<PollableEntry>> {
        bail!("unreachable: udp socket not created")
    }
    fn drop(
        &mut self,
        _rep: Resource<wasi::sockets::udp::IncomingDatagramStream>,
    ) -> Result<()> {
        bail!("unreachable: udp socket not created")
    }
}

impl wasi::sockets::udp::HostOutgoingDatagramStream for WasiCtx {
    fn check_send(
        &mut self,
        _self_: Resource<wasi::sockets::udp::OutgoingDatagramStream>,
    ) -> Result<Result<u64, wasi::sockets::network::ErrorCode>> {
        bail!("unreachable: udp socket not created")
    }
    fn send(
        &mut self,
        _self_: Resource<wasi::sockets::udp::OutgoingDatagramStream>,
        _datagrams: Vec<wasi::sockets::udp::OutgoingDatagram>,
    ) -> Result<Result<u64, wasi::sockets::network::ErrorCode>> {
        bail!("unreachable: udp socket not created")
    }
    fn subscribe(
        &mut self,
        _self_: Resource<wasi::sockets::udp::OutgoingDatagramStream>,
    ) -> Result<Resource<PollableEntry>> {
        bail!("unreachable: udp socket not created")
    }
    fn drop(
        &mut self,
        _rep: Resource<wasi::sockets::udp::OutgoingDatagramStream>,
    ) -> Result<()> {
        bail!("unreachable: udp socket not created")
    }
}

impl wasi::sockets::ip_name_lookup::Host for WasiCtx {
    fn resolve_addresses(
        &mut self,
        _network: Resource<wasi::sockets::network::Network>,
        _name: String,
    ) -> Result<
        Result<
            Resource<wasi::sockets::ip_name_lookup::ResolveAddressStream>,
            wasi::sockets::network::ErrorCode,
        >,
    > {
        Ok(Err(wasi::sockets::network::ErrorCode::NotSupported))
    }
}

impl wasi::sockets::ip_name_lookup::HostResolveAddressStream for WasiCtx {
    fn resolve_next_address(
        &mut self,
        _self_: Resource<wasi::sockets::ip_name_lookup::ResolveAddressStream>,
    ) -> Result<
        Result<Option<wasi::sockets::network::IpAddress>, wasi::sockets::network::ErrorCode>,
    > {
        bail!("unreachable: resolve not started")
    }
    fn subscribe(
        &mut self,
        _self_: Resource<wasi::sockets::ip_name_lookup::ResolveAddressStream>,
    ) -> Result<Resource<PollableEntry>> {
        bail!("unreachable: resolve not started")
    }
    fn drop(
        &mut self,
        _rep: Resource<wasi::sockets::ip_name_lookup::ResolveAddressStream>,
    ) -> Result<()> {
        bail!("unreachable: resolve not started")
    }
}

// ── Helper functions ────────────────────────────────────────────────

fn twz_err_to_wasi(e: TwzError) -> ErrorCode {
    use twizzler_rt_abi::error::*;
    match e {
        TwzError::Naming(NamingError::NotFound) => ErrorCode::NoEntry,
        TwzError::Naming(NamingError::AlreadyExists) => ErrorCode::Exist,
        TwzError::Naming(NamingError::NotEmpty) => ErrorCode::NotEmpty,
        TwzError::Naming(NamingError::WrongNameKind) => ErrorCode::NotDirectory,
        TwzError::Naming(NamingError::LinkLoop) => ErrorCode::Loop,
        TwzError::Argument(ArgumentError::InvalidArgument) => ErrorCode::Invalid,
        TwzError::Argument(ArgumentError::BadHandle) => ErrorCode::BadDescriptor,
        TwzError::Generic(GenericError::NotSupported) => ErrorCode::Unsupported,
        TwzError::Generic(GenericError::WouldBlock) => ErrorCode::WouldBlock,
        TwzError::Resource(ResourceError::OutOfMemory) => ErrorCode::InsufficientMemory,
        TwzError::Resource(ResourceError::OutOfResources) => ErrorCode::InsufficientSpace,
        _ => ErrorCode::Io,
    }
}

fn join_path(base: &str, name: &str) -> String {
    if base.ends_with('/') {
        format!("{base}{name}")
    } else {
        format!("{base}/{name}")
    }
}

fn fd_kind_to_desc_type(kind: FdKind) -> wasi::filesystem::types::DescriptorType {
    match kind {
        FdKind::Regular => wasi::filesystem::types::DescriptorType::RegularFile,
        FdKind::Directory => wasi::filesystem::types::DescriptorType::Directory,
        FdKind::SymLink => wasi::filesystem::types::DescriptorType::SymbolicLink,
        FdKind::Other => wasi::filesystem::types::DescriptorType::Unknown,
    }
}

fn fd_info_to_stat(info: &fd::FdInfo) -> wasi::filesystem::types::DescriptorStat {
    wasi::filesystem::types::DescriptorStat {
        type_: fd_kind_to_desc_type(info.kind),
        link_count: 1,
        size: info.size,
        data_access_timestamp: Some(wasi::clocks::wall_clock::Datetime {
            seconds: info.accessed.as_secs(),
            nanoseconds: info.accessed.subsec_nanos(),
        }),
        data_modification_timestamp: Some(wasi::clocks::wall_clock::Datetime {
            seconds: info.modified.as_secs(),
            nanoseconds: info.modified.subsec_nanos(),
        }),
        status_change_timestamp: Some(wasi::clocks::wall_clock::Datetime {
            seconds: info.created.as_secs(),
            nanoseconds: info.created.subsec_nanos(),
        }),
    }
}

// ── Linker setup ────────────────────────────────────────────────────

fn add_wasi_to_linker(linker: &mut Linker<WasiCtx>) -> Result<()> {
    type D = wasmtime::component::HasSelf<WasiCtx>;
    wasi::io::error::add_to_linker::<_, D>(linker, |t| t)?;
    wasi::io::poll::add_to_linker::<_, D>(linker, |t| t)?;
    wasi::io::streams::add_to_linker::<_, D>(linker, |t| t)?;
    wasi::clocks::monotonic_clock::add_to_linker::<_, D>(linker, |t| t)?;
    wasi::clocks::wall_clock::add_to_linker::<_, D>(linker, |t| t)?;
    wasi::cli::environment::add_to_linker::<_, D>(linker, |t| t)?;
    wasi::cli::exit::add_to_linker::<_, D>(linker, &Default::default(), |t| t)?;
    wasi::cli::stdin::add_to_linker::<_, D>(linker, |t| t)?;
    wasi::cli::stdout::add_to_linker::<_, D>(linker, |t| t)?;
    wasi::cli::stderr::add_to_linker::<_, D>(linker, |t| t)?;
    wasi::cli::terminal_input::add_to_linker::<_, D>(linker, |t| t)?;
    wasi::cli::terminal_output::add_to_linker::<_, D>(linker, |t| t)?;
    wasi::cli::terminal_stdin::add_to_linker::<_, D>(linker, |t| t)?;
    wasi::cli::terminal_stdout::add_to_linker::<_, D>(linker, |t| t)?;
    wasi::cli::terminal_stderr::add_to_linker::<_, D>(linker, |t| t)?;
    wasi::random::random::add_to_linker::<_, D>(linker, |t| t)?;
    wasi::random::insecure::add_to_linker::<_, D>(linker, |t| t)?;
    wasi::random::insecure_seed::add_to_linker::<_, D>(linker, |t| t)?;
    wasi::filesystem::preopens::add_to_linker::<_, D>(linker, |t| t)?;
    wasi::filesystem::types::add_to_linker::<_, D>(linker, |t| t)?;
    wasi::sockets::instance_network::add_to_linker::<_, D>(linker, |t| t)?;
    wasi::sockets::network::add_to_linker::<_, D>(linker, &Default::default(), |t| t)?;
    wasi::sockets::tcp::add_to_linker::<_, D>(linker, |t| t)?;
    wasi::sockets::tcp_create_socket::add_to_linker::<_, D>(linker, |t| t)?;
    wasi::sockets::udp::add_to_linker::<_, D>(linker, |t| t)?;
    wasi::sockets::udp_create_socket::add_to_linker::<_, D>(linker, |t| t)?;
    wasi::sockets::ip_name_lookup::add_to_linker::<_, D>(linker, |t| t)?;
    Ok(())
}

// ── Public API ──────────────────────────────────────────────────────

/// Run a WASI P2 component (wasi:cli/command world).
pub fn run_wasi_component(component_bytes: &[u8]) -> Result<()> {
    let mut config = Config::new();
    config.memory_init_cow(false);
    config.memory_reservation(0);
    config.memory_guard_size(0);
    config.memory_reservation_for_growth(0);
    config.signals_based_traps(false);

    let engine = Engine::new(&config)?;
    let component = Component::new(&engine, component_bytes)?;

    let mut linker = Linker::new(&engine);
    add_wasi_to_linker(&mut linker)?;

    let ctx = WasiCtx {
        table: ResourceTable::new(),
    };
    let mut store = Store::new(&engine, ctx);

    let command = Command::instantiate(&mut store, &component, &linker)?;
    match command.wasi_cli_run().call_run(&mut store)? {
        Ok(()) => Ok(()),
        Err(()) => bail!("WASI component returned error"),
    }
}
