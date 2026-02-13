//! WASI Preview 2 (component model) for Twizzler — async with fiber stacks.
//!
//! Implements wasi:io, wasi:clocks, wasi:cli, wasi:random, wasi:filesystem,
//! wasi:sockets (TCP, UDP, DNS), and the WASI-GFX interfaces (wasi:graphics-context,
//! wasi:frame-buffer) backed by Twizzler runtime APIs and userspace smoltcp
//! networking.

use anyhow::{bail, Result};
use core::mem::MaybeUninit;

use twizzler_abi::syscall::{
    sys_get_random, sys_kernel_console_read, sys_kernel_console_write, GetRandomFlags,
    KernelConsoleReadFlags, KernelConsoleSource, KernelConsoleWriteFlags,
};
use twizzler_rt_abi::error::TwzError;
use twizzler_rt_abi::fd::{self, FdFlags, FdKind, NameEntry, RawFd};
use twizzler_rt_abi::io::{IoCtx, IoFlags};

use wasmtime::component::{Component, Linker, Resource, ResourceTable};
use wasmtime::{Engine, Store};

use crate::net;

// ── Resource backing types ──────────────────────────────────────────

pub struct IoError(String);

/// Typed pollable entry for real async I/O multiplexing.
pub enum PollableEntry {
    /// Console, file, or DNS — always immediately ready.
    AlwaysReady,
    /// Monotonic clock timer — ready when deadline is reached.
    MonotonicTimer { deadline_ns: u64 },
    /// TCP socket readable (data available or peer closed).
    TcpReadable { socket: net::NetSocket },
    /// TCP socket writable (send buffer has space).
    TcpWritable { socket: net::NetSocket },
    /// TCP listener has a connection ready to accept.
    TcpAcceptable { listener: net::NetListener },
    /// UDP socket has incoming datagrams.
    UdpReadable { socket: net::NetUdpSocket },
    /// UDP socket can send datagrams.
    UdpWritable { socket: net::NetUdpSocket },
}

impl PollableEntry {
    fn is_ready(&self) -> bool {
        match self {
            PollableEntry::AlwaysReady => true,
            PollableEntry::MonotonicTimer { deadline_ns } => {
                let now = twizzler_rt_abi::time::twz_rt_get_monotonic_time().as_nanos() as u64;
                now >= *deadline_ns
            }
            PollableEntry::TcpReadable { socket } => socket.can_read(),
            PollableEntry::TcpWritable { socket } => socket.can_write(),
            PollableEntry::TcpAcceptable { listener } => listener.can_accept(),
            PollableEntry::UdpReadable { socket } => socket.can_recv(),
            PollableEntry::UdpWritable { socket } => socket.can_send(),
        }
    }

    fn is_network(&self) -> bool {
        matches!(
            self,
            PollableEntry::TcpReadable { .. }
                | PollableEntry::TcpWritable { .. }
                | PollableEntry::TcpAcceptable { .. }
                | PollableEntry::UdpReadable { .. }
                | PollableEntry::UdpWritable { .. }
        )
    }

    fn deadline_ns(&self) -> Option<u64> {
        match self {
            PollableEntry::MonotonicTimer { deadline_ns } => Some(*deadline_ns),
            _ => None,
        }
    }
}

pub enum InputStreamKind {
    Console,
    File { fd: RawFd, position: u64 },
    TcpSocket { socket: net::NetSocket },
}

pub enum OutputStreamKind {
    Console,
    File {
        fd: RawFd,
        position: u64,
        append: bool,
    },
    TcpSocket { socket: net::NetSocket },
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

// ── WASI-GFX resource backing types ─────────────────────────────────

/// Backing type for wasi:graphics-context/context.
pub struct GfxContext {
    /// Index into WasiCtx.display_pixels for the current frame buffer.
    has_surface: bool,
}

/// Backing type for wasi:graphics-context/abstract-buffer (opaque token).
pub struct AbstractBuffer;

/// Backing type for wasi:frame-buffer/device.
pub struct FrameBufferDevice;

/// Backing type for wasi:frame-buffer/buffer.
pub struct FrameBuffer {
    data: Vec<u8>,
}

// ── Socket resource backing types ────────────────────────────────────

/// Backing type for wasi:sockets/network/network.
pub struct NetworkEntry;

/// TCP socket state machine.
pub enum TcpSocketState {
    Unbound,
    Bound { addr: net::NetAddr },
    Listening { listener: net::NetListener },
    Connected { socket: net::NetSocket },
    Closed,
}

/// Backing type for wasi:sockets/tcp/tcp-socket.
pub struct TcpSocketEntry {
    state: TcpSocketState,
    family: wasi::sockets::network::IpAddressFamily,
    keep_alive_enabled: bool,
    hop_limit: u8,
    receive_buffer_size: u64,
    send_buffer_size: u64,
}

/// UDP socket state machine.
pub enum UdpSocketState {
    Unbound,
    Bound { socket: net::NetUdpSocket },
}

/// Backing type for wasi:sockets/udp/udp-socket.
pub struct UdpSocketEntry {
    state: UdpSocketState,
    family: wasi::sockets::network::IpAddressFamily,
    remote_address: Option<net::NetAddr>,
    hop_limit: u8,
    receive_buffer_size: u64,
    send_buffer_size: u64,
}

/// Backing type for wasi:sockets/udp/incoming-datagram-stream.
pub struct IncomingDatagramStreamEntry {
    socket: net::NetUdpSocket,
    remote_address: Option<net::NetAddr>,
}

/// Backing type for wasi:sockets/udp/outgoing-datagram-stream.
pub struct OutgoingDatagramStreamEntry {
    socket: net::NetUdpSocket,
    remote_address: Option<net::NetAddr>,
}

/// Backing type for wasi:sockets/ip-name-lookup/resolve-address-stream.
pub struct ResolveAddressStreamEntry {
    addresses: Vec<smoltcp::wire::IpAddress>,
    index: usize,
}

// ── Bindgen ─────────────────────────────────────────────────────────

wasmtime::component::bindgen!({
    path: "wit",
    world: "wasmtime:wasi/command",
    imports: { default: trappable | async },
    exports: { default: async },
    with: {
        "wasi:io/error/error": IoError,
        "wasi:io/poll/pollable": PollableEntry,
        "wasi:io/streams/input-stream": InputStreamKind,
        "wasi:io/streams/output-stream": OutputStreamKind,
        "wasi:cli/terminal-input/terminal-input": TerminalInput,
        "wasi:cli/terminal-output/terminal-output": TerminalOutput,
        "wasi:filesystem/types/descriptor": DescriptorEntry,
        "wasi:filesystem/types/directory-entry-stream": DirEntryStream,
        "wasi:graphics-context/graphics-context/context": GfxContext,
        "wasi:graphics-context/graphics-context/abstract-buffer": AbstractBuffer,
        "wasi:frame-buffer/frame-buffer/device": FrameBufferDevice,
        "wasi:frame-buffer/frame-buffer/buffer": FrameBuffer,
        "wasi:sockets/network/network": NetworkEntry,
        "wasi:sockets/tcp/tcp-socket": TcpSocketEntry,
        "wasi:sockets/udp/udp-socket": UdpSocketEntry,
        "wasi:sockets/udp/incoming-datagram-stream": IncomingDatagramStreamEntry,
        "wasi:sockets/udp/outgoing-datagram-stream": OutgoingDatagramStreamEntry,
        "wasi:sockets/ip-name-lookup/resolve-address-stream": ResolveAddressStreamEntry,
    },
});

// ── WASI Context ────────────────────────────────────────────────────

pub struct WasiCtx {
    table: ResourceTable,
    // Display state for WASI-GFX
    display_window: Option<twizzler_display::WindowHandle>,
    display_width: u32,
    display_height: u32,
    display_pixels: Vec<u8>,
}

// ── wasi:io/error ───────────────────────────────────────────────────

impl wasi::io::error::Host for WasiCtx {}

impl wasi::io::error::HostError for WasiCtx {
    async fn to_debug_string(&mut self, self_: Resource<IoError>) -> Result<String> {
        Ok(self.table.get(&self_)?.0.clone())
    }

    async fn drop(&mut self, rep: Resource<IoError>) -> Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

// ── wasi:io/poll ────────────────────────────────────────────────────

impl wasi::io::poll::Host for WasiCtx {
    async fn poll(&mut self, in_: Vec<Resource<PollableEntry>>) -> Result<Vec<u32>> {
        loop {
            // Check if any network pollables are present.
            let has_network = in_
                .iter()
                .any(|r| self.table.get(r).map_or(false, |e| e.is_network()));

            if has_network {
                net::trigger_poll();
            }

            // Collect ready indices.
            let ready: Vec<u32> = in_
                .iter()
                .enumerate()
                .filter_map(|(i, r)| {
                    self.table
                        .get(r)
                        .ok()
                        .filter(|e| e.is_ready())
                        .map(|_| i as u32)
                })
                .collect();

            if !ready.is_empty() {
                return Ok(ready);
            }

            // Compute earliest timer deadline.
            let earliest_deadline = in_
                .iter()
                .filter_map(|r| self.table.get(r).ok().and_then(|e| e.deadline_ns()))
                .min();

            let now =
                twizzler_rt_abi::time::twz_rt_get_monotonic_time().as_nanos() as u64;

            if has_network {
                let timeout = earliest_deadline
                    .map(|d| std::time::Duration::from_nanos(d.saturating_sub(now)));
                net::wait_for_network_event(timeout);
            } else if let Some(deadline) = earliest_deadline {
                let remaining = deadline.saturating_sub(now);
                if remaining > 0 {
                    std::thread::sleep(std::time::Duration::from_nanos(remaining));
                }
            } else {
                // No network, no timers — avoid spinning.
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
    }
}

impl wasi::io::poll::HostPollable for WasiCtx {
    async fn ready(&mut self, self_: Resource<PollableEntry>) -> Result<bool> {
        Ok(self.table.get(&self_)?.is_ready())
    }

    async fn block(&mut self, self_: Resource<PollableEntry>) -> Result<()> {
        loop {
            let entry = self.table.get(&self_)?;
            if entry.is_ready() {
                return Ok(());
            }

            let is_network = entry.is_network();
            let deadline = entry.deadline_ns();

            let now =
                twizzler_rt_abi::time::twz_rt_get_monotonic_time().as_nanos() as u64;

            if is_network {
                net::trigger_poll();
                let timeout =
                    deadline.map(|d| std::time::Duration::from_nanos(d.saturating_sub(now)));
                net::wait_for_network_event(timeout);
            } else if let Some(dl) = deadline {
                let remaining = dl.saturating_sub(now);
                if remaining > 0 {
                    std::thread::sleep(std::time::Duration::from_nanos(remaining));
                }
            }
        }
    }

    async fn drop(&mut self, rep: Resource<PollableEntry>) -> Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

// ── wasi:io/streams ─────────────────────────────────────────────────

use wasi::io::streams::StreamError;

impl wasi::io::streams::Host for WasiCtx {}

impl wasi::io::streams::HostInputStream for WasiCtx {
    async fn read(
        &mut self,
        self_: Resource<InputStreamKind>,
        len: u64,
    ) -> Result<Result<Vec<u8>, StreamError>> {
        // Extract info to avoid holding a borrow across table mutations.
        enum ReadKind {
            Console,
            File(RawFd, u64),
            TcpSocket,
        }
        let kind = {
            let s = self.table.get(&self_)?;
            match s {
                InputStreamKind::Console => ReadKind::Console,
                InputStreamKind::File { fd, position } => ReadKind::File(*fd, *position),
                InputStreamKind::TcpSocket { .. } => ReadKind::TcpSocket,
            }
        };

        match kind {
            ReadKind::Console => {
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
            ReadKind::File(fd, pos) => {
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
            ReadKind::TcpSocket => {
                // Clone the socket Arc to avoid holding a table borrow across blocking I/O.
                let socket = {
                    let s = self.table.get(&self_)?;
                    match s {
                        InputStreamKind::TcpSocket { socket } => socket.clone_socket(),
                        _ => unreachable!(),
                    }
                };
                let mut buf = vec![0u8; (len as usize).min(65536)];
                match socket.read(&mut buf) {
                    Ok(0) => Ok(Err(StreamError::Closed)),
                    Ok(n) => {
                        buf.truncate(n);
                        Ok(Ok(buf))
                    }
                    Err(e) => {
                        let err = self.table.push(IoError(format!("{e:?}")))?;
                        Ok(Err(StreamError::LastOperationFailed(err)))
                    }
                }
            }
        }
    }

    async fn blocking_read(
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
            self.read(self_, len).await
        }
    }

    async fn skip(
        &mut self,
        self_: Resource<InputStreamKind>,
        len: u64,
    ) -> Result<Result<u64, StreamError>> {
        match self.read(self_, len).await? {
            Ok(data) => Ok(Ok(data.len() as u64)),
            Err(e) => Ok(Err(e)),
        }
    }

    async fn blocking_skip(
        &mut self,
        self_: Resource<InputStreamKind>,
        len: u64,
    ) -> Result<Result<u64, StreamError>> {
        self.skip(self_, len).await
    }

    async fn subscribe(
        &mut self,
        self_: Resource<InputStreamKind>,
    ) -> Result<Resource<PollableEntry>> {
        let entry = match self.table.get(&self_)? {
            InputStreamKind::Console | InputStreamKind::File { .. } => PollableEntry::AlwaysReady,
            InputStreamKind::TcpSocket { socket } => PollableEntry::TcpReadable {
                socket: socket.clone_socket(),
            },
        };
        Ok(self.table.push(entry)?)
    }

    async fn drop(&mut self, rep: Resource<InputStreamKind>) -> Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

impl wasi::io::streams::HostOutputStream for WasiCtx {
    async fn check_write(
        &mut self,
        _self_: Resource<OutputStreamKind>,
    ) -> Result<Result<u64, StreamError>> {
        Ok(Ok(usize::MAX as u64))
    }

    async fn write(
        &mut self,
        self_: Resource<OutputStreamKind>,
        contents: Vec<u8>,
    ) -> Result<Result<(), StreamError>> {
        enum WriteKind {
            Console,
            File(RawFd, u64, bool),
            TcpSocket,
        }
        let kind = {
            let s = self.table.get(&self_)?;
            match s {
                OutputStreamKind::Console => WriteKind::Console,
                OutputStreamKind::File {
                    fd,
                    position,
                    append,
                } => WriteKind::File(*fd, *position, *append),
                OutputStreamKind::TcpSocket { .. } => WriteKind::TcpSocket,
            }
        };

        match kind {
            WriteKind::Console => {
                sys_kernel_console_write(
                    KernelConsoleSource::Console,
                    &contents,
                    KernelConsoleWriteFlags::empty(),
                );
                Ok(Ok(()))
            }
            WriteKind::TcpSocket => {
                let socket = {
                    let s = self.table.get(&self_)?;
                    match s {
                        OutputStreamKind::TcpSocket { socket } => socket.clone_socket(),
                        _ => unreachable!(),
                    }
                };
                let mut written = 0;
                while written < contents.len() {
                    match socket.write(&contents[written..]) {
                        Ok(n) => written += n,
                        Err(e) => {
                            let err = self.table.push(IoError(format!("{e:?}")))?;
                            return Ok(Err(StreamError::LastOperationFailed(err)));
                        }
                    }
                }
                Ok(Ok(()))
            }
            WriteKind::File(fd, pos, is_append) => {
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

    async fn blocking_write_and_flush(
        &mut self,
        self_: Resource<OutputStreamKind>,
        contents: Vec<u8>,
    ) -> Result<Result<(), StreamError>> {
        self.write(self_, contents).await
    }

    async fn flush(
        &mut self,
        _self_: Resource<OutputStreamKind>,
    ) -> Result<Result<(), StreamError>> {
        Ok(Ok(()))
    }

    async fn blocking_flush(
        &mut self,
        _self_: Resource<OutputStreamKind>,
    ) -> Result<Result<(), StreamError>> {
        Ok(Ok(()))
    }

    async fn subscribe(
        &mut self,
        self_: Resource<OutputStreamKind>,
    ) -> Result<Resource<PollableEntry>> {
        let entry = match self.table.get(&self_)? {
            OutputStreamKind::Console | OutputStreamKind::File { .. } => PollableEntry::AlwaysReady,
            OutputStreamKind::TcpSocket { socket } => PollableEntry::TcpWritable {
                socket: socket.clone_socket(),
            },
        };
        Ok(self.table.push(entry)?)
    }

    async fn write_zeroes(
        &mut self,
        self_: Resource<OutputStreamKind>,
        len: u64,
    ) -> Result<Result<(), StreamError>> {
        let zeros = vec![0u8; (len as usize).min(65536)];
        self.write(self_, zeros).await
    }

    async fn blocking_write_zeroes_and_flush(
        &mut self,
        self_: Resource<OutputStreamKind>,
        len: u64,
    ) -> Result<Result<(), StreamError>> {
        self.write_zeroes(self_, len).await
    }

    async fn splice(
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

    async fn blocking_splice(
        &mut self,
        self_: Resource<OutputStreamKind>,
        src: Resource<InputStreamKind>,
        len: u64,
    ) -> Result<Result<u64, StreamError>> {
        self.splice(self_, src, len).await
    }

    async fn drop(&mut self, rep: Resource<OutputStreamKind>) -> Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

// ── wasi:clocks ─────────────────────────────────────────────────────

impl wasi::clocks::monotonic_clock::Host for WasiCtx {
    async fn now(&mut self) -> Result<wasi::clocks::monotonic_clock::Instant> {
        Ok(twizzler_rt_abi::time::twz_rt_get_monotonic_time().as_nanos() as u64)
    }

    async fn resolution(&mut self) -> Result<wasi::clocks::monotonic_clock::Duration> {
        Ok(1)
    }

    async fn subscribe_duration(
        &mut self,
        duration: wasi::clocks::monotonic_clock::Duration,
    ) -> Result<Resource<PollableEntry>> {
        let now = twizzler_rt_abi::time::twz_rt_get_monotonic_time().as_nanos() as u64;
        Ok(self
            .table
            .push(PollableEntry::MonotonicTimer {
                deadline_ns: now + duration,
            })?)
    }

    async fn subscribe_instant(
        &mut self,
        deadline: wasi::clocks::monotonic_clock::Instant,
    ) -> Result<Resource<PollableEntry>> {
        Ok(self
            .table
            .push(PollableEntry::MonotonicTimer {
                deadline_ns: deadline,
            })?)
    }
}

impl wasi::clocks::wall_clock::Host for WasiCtx {
    async fn now(&mut self) -> Result<wasi::clocks::wall_clock::Datetime> {
        let t = twizzler_rt_abi::time::twz_rt_get_system_time();
        Ok(wasi::clocks::wall_clock::Datetime {
            seconds: t.as_secs(),
            nanoseconds: t.subsec_nanos(),
        })
    }

    async fn resolution(&mut self) -> Result<wasi::clocks::wall_clock::Datetime> {
        Ok(wasi::clocks::wall_clock::Datetime {
            seconds: 0,
            nanoseconds: 1,
        })
    }
}

// ── wasi:cli ────────────────────────────────────────────────────────

impl wasi::cli::environment::Host for WasiCtx {
    async fn get_arguments(&mut self) -> Result<Vec<String>> {
        Ok(std::env::args().collect())
    }

    async fn get_environment(&mut self) -> Result<Vec<(String, String)>> {
        Ok(std::env::vars().collect())
    }

    async fn initial_cwd(&mut self) -> Result<Option<String>> {
        Ok(Some("/".to_string()))
    }
}

impl wasi::cli::exit::Host for WasiCtx {
    async fn exit(&mut self, code: core::result::Result<(), ()>) -> Result<()> {
        if code.is_ok() {
            bail!("wasi exit success")
        } else {
            bail!("wasi exit error")
        }
    }

    async fn exit_with_code(&mut self, code: u8) -> Result<()> {
        bail!("wasi exit with code {code}")
    }
}

impl wasi::cli::stdin::Host for WasiCtx {
    async fn get_stdin(&mut self) -> Result<Resource<InputStreamKind>> {
        Ok(self.table.push(InputStreamKind::Console)?)
    }
}

impl wasi::cli::stdout::Host for WasiCtx {
    async fn get_stdout(&mut self) -> Result<Resource<OutputStreamKind>> {
        Ok(self.table.push(OutputStreamKind::Console)?)
    }
}

impl wasi::cli::stderr::Host for WasiCtx {
    async fn get_stderr(&mut self) -> Result<Resource<OutputStreamKind>> {
        Ok(self.table.push(OutputStreamKind::Console)?)
    }
}

impl wasi::cli::terminal_input::Host for WasiCtx {}
impl wasi::cli::terminal_input::HostTerminalInput for WasiCtx {
    async fn drop(&mut self, rep: Resource<TerminalInput>) -> Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

impl wasi::cli::terminal_output::Host for WasiCtx {}
impl wasi::cli::terminal_output::HostTerminalOutput for WasiCtx {
    async fn drop(&mut self, rep: Resource<TerminalOutput>) -> Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

impl wasi::cli::terminal_stdin::Host for WasiCtx {
    async fn get_terminal_stdin(&mut self) -> Result<Option<Resource<TerminalInput>>> {
        if let Ok(info) = fd::twz_rt_fd_get_info(0) {
            if info.flags.contains(FdFlags::IS_TERMINAL) {
                return Ok(Some(self.table.push(TerminalInput)?));
            }
        }
        Ok(None)
    }
}

impl wasi::cli::terminal_stdout::Host for WasiCtx {
    async fn get_terminal_stdout(&mut self) -> Result<Option<Resource<TerminalOutput>>> {
        if let Ok(info) = fd::twz_rt_fd_get_info(1) {
            if info.flags.contains(FdFlags::IS_TERMINAL) {
                return Ok(Some(self.table.push(TerminalOutput)?));
            }
        }
        Ok(None)
    }
}

impl wasi::cli::terminal_stderr::Host for WasiCtx {
    async fn get_terminal_stderr(&mut self) -> Result<Option<Resource<TerminalOutput>>> {
        if let Ok(info) = fd::twz_rt_fd_get_info(2) {
            if info.flags.contains(FdFlags::IS_TERMINAL) {
                return Ok(Some(self.table.push(TerminalOutput)?));
            }
        }
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
    async fn get_random_bytes(&mut self, len: u64) -> Result<Vec<u8>> {
        random_bytes(len)
    }

    async fn get_random_u64(&mut self) -> Result<u64> {
        random_u64()
    }
}

impl wasi::random::insecure::Host for WasiCtx {
    async fn get_insecure_random_bytes(&mut self, len: u64) -> Result<Vec<u8>> {
        random_bytes(len)
    }

    async fn get_insecure_random_u64(&mut self) -> Result<u64> {
        random_u64()
    }
}

impl wasi::random::insecure_seed::Host for WasiCtx {
    async fn insecure_seed(&mut self) -> Result<(u64, u64)> {
        Ok((random_u64()?, random_u64()?))
    }
}

// ── wasi:filesystem/preopens ────────────────────────────────────────

impl wasi::filesystem::preopens::Host for WasiCtx {
    async fn get_directories(
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
    async fn filesystem_error_code(
        &mut self,
        _err: Resource<IoError>,
    ) -> Result<Option<ErrorCode>> {
        Ok(None)
    }
}

impl wasi::filesystem::types::HostDescriptor for WasiCtx {
    async fn read_via_stream(
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

    async fn write_via_stream(
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

    async fn append_via_stream(
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

    async fn advise(
        &mut self,
        _desc: Resource<DescriptorEntry>,
        _offset: u64,
        _length: u64,
        _advice: wasi::filesystem::types::Advice,
    ) -> Result<Result<(), ErrorCode>> {
        Ok(Ok(())) // Advisory only.
    }

    async fn sync_data(
        &mut self,
        desc: Resource<DescriptorEntry>,
    ) -> Result<Result<(), ErrorCode>> {
        let entry = self.table.get(&desc)?;
        fd::twz_rt_fd_sync(entry.fd);
        Ok(Ok(()))
    }

    async fn get_flags(
        &mut self,
        _desc: Resource<DescriptorEntry>,
    ) -> Result<Result<wasi::filesystem::types::DescriptorFlags, ErrorCode>> {
        Ok(Ok(
            wasi::filesystem::types::DescriptorFlags::READ
                | wasi::filesystem::types::DescriptorFlags::WRITE,
        ))
    }

    async fn get_type(
        &mut self,
        desc: Resource<DescriptorEntry>,
    ) -> Result<Result<wasi::filesystem::types::DescriptorType, ErrorCode>> {
        let entry = self.table.get(&desc)?;
        match fd::twz_rt_fd_get_info(entry.fd) {
            Ok(info) => Ok(Ok(fd_kind_to_desc_type(info.kind))),
            Err(e) => Ok(Err(twz_err_to_wasi(e))),
        }
    }

    async fn set_size(
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

    async fn set_times(
        &mut self,
        _desc: Resource<DescriptorEntry>,
        _data_access: wasi::filesystem::types::NewTimestamp,
        _data_modification: wasi::filesystem::types::NewTimestamp,
    ) -> Result<Result<(), ErrorCode>> {
        Ok(Err(ErrorCode::Unsupported))
    }

    async fn read(
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

    async fn write(
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

    async fn read_directory(
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

    async fn sync(
        &mut self,
        desc: Resource<DescriptorEntry>,
    ) -> Result<Result<(), ErrorCode>> {
        let entry = self.table.get(&desc)?;
        fd::twz_rt_fd_sync(entry.fd);
        Ok(Ok(()))
    }

    async fn create_directory_at(
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

    async fn stat(
        &mut self,
        desc: Resource<DescriptorEntry>,
    ) -> Result<Result<wasi::filesystem::types::DescriptorStat, ErrorCode>> {
        let entry = self.table.get(&desc)?;
        match fd::twz_rt_fd_get_info(entry.fd) {
            Ok(info) => Ok(Ok(fd_info_to_stat(&info))),
            Err(e) => Ok(Err(twz_err_to_wasi(e))),
        }
    }

    async fn stat_at(
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

    async fn set_times_at(
        &mut self,
        _desc: Resource<DescriptorEntry>,
        _path_flags: wasi::filesystem::types::PathFlags,
        _path: String,
        _data_access: wasi::filesystem::types::NewTimestamp,
        _data_modification: wasi::filesystem::types::NewTimestamp,
    ) -> Result<Result<(), ErrorCode>> {
        Ok(Err(ErrorCode::Unsupported))
    }

    async fn link_at(
        &mut self,
        _desc: Resource<DescriptorEntry>,
        _old_path_flags: wasi::filesystem::types::PathFlags,
        _old_path: String,
        _new_desc: Resource<DescriptorEntry>,
        _new_path: String,
    ) -> Result<Result<(), ErrorCode>> {
        Ok(Err(ErrorCode::Unsupported))
    }

    async fn open_at(
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

    async fn readlink_at(
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

    async fn remove_directory_at(
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

    async fn rename_at(
        &mut self,
        desc: Resource<DescriptorEntry>,
        old_path: String,
        new_desc: Resource<DescriptorEntry>,
        new_path: String,
    ) -> Result<Result<(), ErrorCode>> {
        let old_entry = self.table.get(&desc)?;
        let old_full = join_path(&old_entry.path, &old_path);
        let new_entry = self.table.get(&new_desc)?;
        let new_full = join_path(&new_entry.path, &new_path);
        match fd::twz_rt_fd_rename(&old_full, &new_full) {
            Ok(()) => Ok(Ok(())),
            Err(e) => Ok(Err(twz_err_to_wasi(e))),
        }
    }

    async fn symlink_at(
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

    async fn unlink_file_at(
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

    async fn is_same_object(
        &mut self,
        a: Resource<DescriptorEntry>,
        b: Resource<DescriptorEntry>,
    ) -> Result<bool> {
        let a_entry = self.table.get(&a)?;
        let b_entry = self.table.get(&b)?;
        Ok(a_entry.path == b_entry.path)
    }

    async fn metadata_hash(
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

    async fn metadata_hash_at(
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

    async fn drop(&mut self, desc: Resource<DescriptorEntry>) -> Result<()> {
        self.table.delete(desc)?;
        Ok(())
    }
}

impl wasi::filesystem::types::HostDirectoryEntryStream for WasiCtx {
    async fn read_directory_entry(
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

    async fn drop(&mut self, stream: Resource<DirEntryStream>) -> Result<()> {
        self.table.delete(stream)?;
        Ok(())
    }
}

// ── wasi:sockets stubs ──────────────────────────────────────────────
// Network sockets are not supported on Twizzler yet.
// All create operations return ErrorCode::NotSupported.
// Resource methods bail since no sockets are ever created.

impl wasi::sockets::instance_network::Host for WasiCtx {
    async fn instance_network(
        &mut self,
    ) -> Result<Resource<wasi::sockets::network::Network>> {
        Ok(self.table.push(NetworkEntry)?)
    }
}

impl wasi::sockets::network::Host for WasiCtx {
    async fn network_error_code(
        &mut self,
        _err: Resource<IoError>,
    ) -> Result<Option<wasi::sockets::network::ErrorCode>> {
        Ok(None)
    }
}

impl wasi::sockets::network::HostNetwork for WasiCtx {
    async fn drop(
        &mut self,
        rep: Resource<wasi::sockets::network::Network>,
    ) -> Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

impl wasi::sockets::tcp_create_socket::Host for WasiCtx {
    async fn create_tcp_socket(
        &mut self,
        address_family: wasi::sockets::network::IpAddressFamily,
    ) -> Result<Result<Resource<wasi::sockets::tcp::TcpSocket>, wasi::sockets::network::ErrorCode>>
    {
        if matches!(address_family, wasi::sockets::network::IpAddressFamily::Ipv6) {
            return Ok(Err(wasi::sockets::network::ErrorCode::NotSupported));
        }
        let entry = TcpSocketEntry {
            state: TcpSocketState::Unbound,
            family: address_family,
            keep_alive_enabled: false,
            hop_limit: 64,
            receive_buffer_size: 65536,
            send_buffer_size: 8192,
        };
        Ok(Ok(self.table.push(entry)?))
    }
}

impl wasi::sockets::tcp::Host for WasiCtx {}

impl wasi::sockets::tcp::HostTcpSocket for WasiCtx {
    async fn start_bind(
        &mut self,
        self_: Resource<wasi::sockets::tcp::TcpSocket>,
        _network: Resource<wasi::sockets::network::Network>,
        local_address: wasi::sockets::network::IpSocketAddress,
    ) -> Result<Result<(), wasi::sockets::network::ErrorCode>> {
        let entry = self.table.get_mut(&self_)?;
        if !matches!(entry.state, TcpSocketState::Unbound) {
            return Ok(Err(wasi::sockets::network::ErrorCode::InvalidState));
        }
        let addr = match wasi_addr_to_net(&local_address) {
            Ok(a) => a,
            Err(e) => return Ok(Err(e)),
        };
        entry.state = TcpSocketState::Bound { addr };
        Ok(Ok(()))
    }
    async fn finish_bind(
        &mut self,
        self_: Resource<wasi::sockets::tcp::TcpSocket>,
    ) -> Result<Result<(), wasi::sockets::network::ErrorCode>> {
        let entry = self.table.get(&self_)?;
        if matches!(entry.state, TcpSocketState::Bound { .. }) {
            Ok(Ok(()))
        } else {
            Ok(Err(wasi::sockets::network::ErrorCode::NotInProgress))
        }
    }
    async fn start_connect(
        &mut self,
        self_: Resource<wasi::sockets::tcp::TcpSocket>,
        _network: Resource<wasi::sockets::network::Network>,
        remote_address: wasi::sockets::network::IpSocketAddress,
    ) -> Result<Result<(), wasi::sockets::network::ErrorCode>> {
        let entry = self.table.get_mut(&self_)?;
        match entry.state {
            TcpSocketState::Unbound | TcpSocketState::Bound { .. } => {}
            _ => return Ok(Err(wasi::sockets::network::ErrorCode::InvalidState)),
        }
        let addr = match wasi_addr_to_net(&remote_address) {
            Ok(a) => a,
            Err(e) => return Ok(Err(e)),
        };
        let socket = match net::NetSocket::connect(addr) {
            Ok(s) => s,
            Err(e) => return Ok(Err(net_err_to_wasi(e))),
        };
        entry.state = TcpSocketState::Connected { socket };
        Ok(Ok(()))
    }
    async fn finish_connect(
        &mut self,
        self_: Resource<wasi::sockets::tcp::TcpSocket>,
    ) -> Result<
        Result<
            (Resource<InputStreamKind>, Resource<OutputStreamKind>),
            wasi::sockets::network::ErrorCode,
        >,
    > {
        let socket = {
            let entry = self.table.get(&self_)?;
            match &entry.state {
                TcpSocketState::Connected { socket } => socket.clone_socket(),
                _ => return Ok(Err(wasi::sockets::network::ErrorCode::NotInProgress)),
            }
        };
        let in_stream = self.table.push(InputStreamKind::TcpSocket {
            socket: socket.clone_socket(),
        })?;
        let out_stream = self.table.push(OutputStreamKind::TcpSocket {
            socket,
        })?;
        Ok(Ok((in_stream, out_stream)))
    }
    async fn start_listen(
        &mut self,
        self_: Resource<wasi::sockets::tcp::TcpSocket>,
    ) -> Result<Result<(), wasi::sockets::network::ErrorCode>> {
        let entry = self.table.get_mut(&self_)?;
        let addr = match &entry.state {
            TcpSocketState::Bound { addr } => *addr,
            _ => return Ok(Err(wasi::sockets::network::ErrorCode::InvalidState)),
        };
        let listener = match net::NetListener::bind(addr) {
            Ok(l) => l,
            Err(e) => return Ok(Err(net_err_to_wasi(e))),
        };
        entry.state = TcpSocketState::Listening { listener };
        Ok(Ok(()))
    }
    async fn finish_listen(
        &mut self,
        self_: Resource<wasi::sockets::tcp::TcpSocket>,
    ) -> Result<Result<(), wasi::sockets::network::ErrorCode>> {
        let entry = self.table.get(&self_)?;
        if matches!(entry.state, TcpSocketState::Listening { .. }) {
            Ok(Ok(()))
        } else {
            Ok(Err(wasi::sockets::network::ErrorCode::NotInProgress))
        }
    }
    async fn accept(
        &mut self,
        self_: Resource<wasi::sockets::tcp::TcpSocket>,
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
        let entry = self.table.get(&self_)?;
        let listener = match &entry.state {
            TcpSocketState::Listening { listener } => listener as *const net::NetListener,
            _ => return Ok(Err(wasi::sockets::network::ErrorCode::InvalidState)),
        };
        let family = entry.family;
        // SAFETY: listener lives in the resource table; synchronous host call.
        let (socket, _remote) = match unsafe { &*listener }.accept() {
            Ok(r) => r,
            Err(e) => return Ok(Err(net_err_to_wasi(e))),
        };
        let in_stream = self.table.push(InputStreamKind::TcpSocket {
            socket: socket.clone_socket(),
        })?;
        let out_stream = self.table.push(OutputStreamKind::TcpSocket {
            socket: socket.clone_socket(),
        })?;
        let accepted = TcpSocketEntry {
            state: TcpSocketState::Connected { socket },
            family,
            keep_alive_enabled: false,
            hop_limit: 64,
            receive_buffer_size: 65536,
            send_buffer_size: 8192,
        };
        let tcp_res = self.table.push(accepted)?;
        Ok(Ok((tcp_res, in_stream, out_stream)))
    }
    async fn local_address(
        &mut self,
        self_: Resource<wasi::sockets::tcp::TcpSocket>,
    ) -> Result<Result<wasi::sockets::network::IpSocketAddress, wasi::sockets::network::ErrorCode>>
    {
        let entry = self.table.get(&self_)?;
        match &entry.state {
            TcpSocketState::Bound { addr } => Ok(Ok(net_addr_to_wasi(addr))),
            TcpSocketState::Connected { socket } => match socket.local_addr() {
                Ok(a) => Ok(Ok(net_addr_to_wasi(&a))),
                Err(e) => Ok(Err(net_err_to_wasi(e))),
            },
            TcpSocketState::Listening { listener } => match listener.local_addr() {
                Ok(a) => Ok(Ok(net_addr_to_wasi(&a))),
                Err(e) => Ok(Err(net_err_to_wasi(e))),
            },
            _ => Ok(Err(wasi::sockets::network::ErrorCode::InvalidState)),
        }
    }
    async fn remote_address(
        &mut self,
        self_: Resource<wasi::sockets::tcp::TcpSocket>,
    ) -> Result<Result<wasi::sockets::network::IpSocketAddress, wasi::sockets::network::ErrorCode>>
    {
        let entry = self.table.get(&self_)?;
        match &entry.state {
            TcpSocketState::Connected { socket } => match socket.peer_addr() {
                Ok(a) => Ok(Ok(net_addr_to_wasi(&a))),
                Err(e) => Ok(Err(net_err_to_wasi(e))),
            },
            _ => Ok(Err(wasi::sockets::network::ErrorCode::InvalidState)),
        }
    }
    async fn is_listening(
        &mut self,
        self_: Resource<wasi::sockets::tcp::TcpSocket>,
    ) -> Result<bool> {
        let entry = self.table.get(&self_)?;
        Ok(matches!(entry.state, TcpSocketState::Listening { .. }))
    }
    async fn address_family(
        &mut self,
        self_: Resource<wasi::sockets::tcp::TcpSocket>,
    ) -> Result<wasi::sockets::network::IpAddressFamily> {
        let entry = self.table.get(&self_)?;
        Ok(entry.family)
    }
    async fn set_listen_backlog_size(
        &mut self,
        _self_: Resource<wasi::sockets::tcp::TcpSocket>,
        _value: u64,
    ) -> Result<Result<(), wasi::sockets::network::ErrorCode>> {
        Ok(Ok(())) // smoltcp uses fixed backlog
    }
    async fn keep_alive_enabled(
        &mut self,
        self_: Resource<wasi::sockets::tcp::TcpSocket>,
    ) -> Result<Result<bool, wasi::sockets::network::ErrorCode>> {
        Ok(Ok(self.table.get(&self_)?.keep_alive_enabled))
    }
    async fn set_keep_alive_enabled(
        &mut self,
        self_: Resource<wasi::sockets::tcp::TcpSocket>,
        value: bool,
    ) -> Result<Result<(), wasi::sockets::network::ErrorCode>> {
        self.table.get_mut(&self_)?.keep_alive_enabled = value;
        Ok(Ok(()))
    }
    async fn keep_alive_idle_time(
        &mut self,
        _self_: Resource<wasi::sockets::tcp::TcpSocket>,
    ) -> Result<Result<wasi::clocks::monotonic_clock::Duration, wasi::sockets::network::ErrorCode>>
    {
        Ok(Ok(7_200_000_000_000)) // 2 hours in ns
    }
    async fn set_keep_alive_idle_time(
        &mut self,
        _self_: Resource<wasi::sockets::tcp::TcpSocket>,
        _value: wasi::clocks::monotonic_clock::Duration,
    ) -> Result<Result<(), wasi::sockets::network::ErrorCode>> {
        Ok(Ok(()))
    }
    async fn keep_alive_interval(
        &mut self,
        _self_: Resource<wasi::sockets::tcp::TcpSocket>,
    ) -> Result<Result<wasi::clocks::monotonic_clock::Duration, wasi::sockets::network::ErrorCode>>
    {
        Ok(Ok(75_000_000_000)) // 75s in ns
    }
    async fn set_keep_alive_interval(
        &mut self,
        _self_: Resource<wasi::sockets::tcp::TcpSocket>,
        _value: wasi::clocks::monotonic_clock::Duration,
    ) -> Result<Result<(), wasi::sockets::network::ErrorCode>> {
        Ok(Ok(()))
    }
    async fn keep_alive_count(
        &mut self,
        _self_: Resource<wasi::sockets::tcp::TcpSocket>,
    ) -> Result<Result<u32, wasi::sockets::network::ErrorCode>> {
        Ok(Ok(9))
    }
    async fn set_keep_alive_count(
        &mut self,
        _self_: Resource<wasi::sockets::tcp::TcpSocket>,
        _value: u32,
    ) -> Result<Result<(), wasi::sockets::network::ErrorCode>> {
        Ok(Ok(()))
    }
    async fn hop_limit(
        &mut self,
        self_: Resource<wasi::sockets::tcp::TcpSocket>,
    ) -> Result<Result<u8, wasi::sockets::network::ErrorCode>> {
        Ok(Ok(self.table.get(&self_)?.hop_limit))
    }
    async fn set_hop_limit(
        &mut self,
        self_: Resource<wasi::sockets::tcp::TcpSocket>,
        value: u8,
    ) -> Result<Result<(), wasi::sockets::network::ErrorCode>> {
        if value == 0 {
            return Ok(Err(wasi::sockets::network::ErrorCode::InvalidArgument));
        }
        self.table.get_mut(&self_)?.hop_limit = value;
        Ok(Ok(()))
    }
    async fn receive_buffer_size(
        &mut self,
        self_: Resource<wasi::sockets::tcp::TcpSocket>,
    ) -> Result<Result<u64, wasi::sockets::network::ErrorCode>> {
        Ok(Ok(self.table.get(&self_)?.receive_buffer_size))
    }
    async fn set_receive_buffer_size(
        &mut self,
        self_: Resource<wasi::sockets::tcp::TcpSocket>,
        value: u64,
    ) -> Result<Result<(), wasi::sockets::network::ErrorCode>> {
        if value == 0 {
            return Ok(Err(wasi::sockets::network::ErrorCode::InvalidArgument));
        }
        self.table.get_mut(&self_)?.receive_buffer_size = value;
        Ok(Ok(()))
    }
    async fn send_buffer_size(
        &mut self,
        self_: Resource<wasi::sockets::tcp::TcpSocket>,
    ) -> Result<Result<u64, wasi::sockets::network::ErrorCode>> {
        Ok(Ok(self.table.get(&self_)?.send_buffer_size))
    }
    async fn set_send_buffer_size(
        &mut self,
        self_: Resource<wasi::sockets::tcp::TcpSocket>,
        value: u64,
    ) -> Result<Result<(), wasi::sockets::network::ErrorCode>> {
        if value == 0 {
            return Ok(Err(wasi::sockets::network::ErrorCode::InvalidArgument));
        }
        self.table.get_mut(&self_)?.send_buffer_size = value;
        Ok(Ok(()))
    }
    async fn subscribe(
        &mut self,
        self_: Resource<wasi::sockets::tcp::TcpSocket>,
    ) -> Result<Resource<PollableEntry>> {
        let entry = match &self.table.get(&self_)?.state {
            TcpSocketState::Connected { socket } => PollableEntry::TcpWritable {
                socket: socket.clone_socket(),
            },
            TcpSocketState::Listening { listener } => PollableEntry::TcpAcceptable {
                listener: listener.clone_listener(),
            },
            _ => PollableEntry::AlwaysReady,
        };
        Ok(self.table.push(entry)?)
    }
    async fn shutdown(
        &mut self,
        self_: Resource<wasi::sockets::tcp::TcpSocket>,
        shutdown_type: wasi::sockets::tcp::ShutdownType,
    ) -> Result<Result<(), wasi::sockets::network::ErrorCode>> {
        let entry = self.table.get(&self_)?;
        let socket = match &entry.state {
            TcpSocketState::Connected { socket } => socket,
            _ => return Ok(Err(wasi::sockets::network::ErrorCode::InvalidState)),
        };
        let how = match shutdown_type {
            wasi::sockets::tcp::ShutdownType::Receive => net::NetShutdown::Read,
            wasi::sockets::tcp::ShutdownType::Send => net::NetShutdown::Write,
            wasi::sockets::tcp::ShutdownType::Both => net::NetShutdown::Both,
        };
        match socket.shutdown(how) {
            Ok(()) => Ok(Ok(())),
            Err(e) => Ok(Err(net_err_to_wasi(e))),
        }
    }
    async fn drop(
        &mut self,
        rep: Resource<wasi::sockets::tcp::TcpSocket>,
    ) -> Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

impl wasi::sockets::udp_create_socket::Host for WasiCtx {
    async fn create_udp_socket(
        &mut self,
        address_family: wasi::sockets::network::IpAddressFamily,
    ) -> Result<Result<Resource<wasi::sockets::udp::UdpSocket>, wasi::sockets::network::ErrorCode>>
    {
        if matches!(address_family, wasi::sockets::network::IpAddressFamily::Ipv6) {
            return Ok(Err(wasi::sockets::network::ErrorCode::NotSupported));
        }
        let entry = UdpSocketEntry {
            state: UdpSocketState::Unbound,
            family: address_family,
            remote_address: None,
            hop_limit: 64,
            receive_buffer_size: 65536,
            send_buffer_size: 65536,
        };
        Ok(Ok(self.table.push(entry)?))
    }
}

impl wasi::sockets::udp::Host for WasiCtx {}

impl wasi::sockets::udp::HostUdpSocket for WasiCtx {
    async fn start_bind(
        &mut self,
        self_: Resource<wasi::sockets::udp::UdpSocket>,
        _network: Resource<wasi::sockets::network::Network>,
        local_address: wasi::sockets::network::IpSocketAddress,
    ) -> Result<Result<(), wasi::sockets::network::ErrorCode>> {
        let entry = self.table.get_mut(&self_)?;
        if !matches!(entry.state, UdpSocketState::Unbound) {
            return Ok(Err(wasi::sockets::network::ErrorCode::InvalidState));
        }
        let addr = match wasi_addr_to_net(&local_address) {
            Ok(a) => a,
            Err(e) => return Ok(Err(e)),
        };
        let socket = match net::NetUdpSocket::bind(addr) {
            Ok(s) => s,
            Err(e) => return Ok(Err(net_err_to_wasi(e))),
        };
        entry.state = UdpSocketState::Bound { socket };
        Ok(Ok(()))
    }
    async fn finish_bind(
        &mut self,
        self_: Resource<wasi::sockets::udp::UdpSocket>,
    ) -> Result<Result<(), wasi::sockets::network::ErrorCode>> {
        let entry = self.table.get(&self_)?;
        if matches!(entry.state, UdpSocketState::Bound { .. }) {
            Ok(Ok(()))
        } else {
            Ok(Err(wasi::sockets::network::ErrorCode::NotInProgress))
        }
    }
    async fn stream(
        &mut self,
        self_: Resource<wasi::sockets::udp::UdpSocket>,
        remote_address: Option<wasi::sockets::network::IpSocketAddress>,
    ) -> Result<
        Result<
            (
                Resource<wasi::sockets::udp::IncomingDatagramStream>,
                Resource<wasi::sockets::udp::OutgoingDatagramStream>,
            ),
            wasi::sockets::network::ErrorCode,
        >,
    > {
        let remote = match remote_address {
            Some(ref addr) => match wasi_addr_to_net(addr) {
                Ok(a) => Some(a),
                Err(e) => return Ok(Err(e)),
            },
            None => None,
        };

        let socket = {
            let entry = self.table.get_mut(&self_)?;
            match &entry.state {
                UdpSocketState::Bound { socket } => {
                    entry.remote_address = remote;
                    socket.clone_socket()
                }
                _ => return Ok(Err(wasi::sockets::network::ErrorCode::InvalidState)),
            }
        };

        let in_stream = self.table.push(IncomingDatagramStreamEntry {
            socket: socket.clone_socket(),
            remote_address: remote,
        })?;
        let out_stream = self.table.push(OutgoingDatagramStreamEntry {
            socket,
            remote_address: remote,
        })?;
        Ok(Ok((in_stream, out_stream)))
    }
    async fn local_address(
        &mut self,
        self_: Resource<wasi::sockets::udp::UdpSocket>,
    ) -> Result<Result<wasi::sockets::network::IpSocketAddress, wasi::sockets::network::ErrorCode>>
    {
        let entry = self.table.get(&self_)?;
        match &entry.state {
            UdpSocketState::Bound { socket } => match socket.local_addr() {
                Ok(a) => Ok(Ok(net_addr_to_wasi(&a))),
                Err(e) => Ok(Err(net_err_to_wasi(e))),
            },
            _ => Ok(Err(wasi::sockets::network::ErrorCode::InvalidState)),
        }
    }
    async fn remote_address(
        &mut self,
        self_: Resource<wasi::sockets::udp::UdpSocket>,
    ) -> Result<Result<wasi::sockets::network::IpSocketAddress, wasi::sockets::network::ErrorCode>>
    {
        let entry = self.table.get(&self_)?;
        match entry.remote_address {
            Some(ref addr) => Ok(Ok(net_addr_to_wasi(addr))),
            None => Ok(Err(wasi::sockets::network::ErrorCode::InvalidState)),
        }
    }
    async fn address_family(
        &mut self,
        self_: Resource<wasi::sockets::udp::UdpSocket>,
    ) -> Result<wasi::sockets::network::IpAddressFamily> {
        let entry = self.table.get(&self_)?;
        Ok(entry.family)
    }
    async fn unicast_hop_limit(
        &mut self,
        self_: Resource<wasi::sockets::udp::UdpSocket>,
    ) -> Result<Result<u8, wasi::sockets::network::ErrorCode>> {
        Ok(Ok(self.table.get(&self_)?.hop_limit))
    }
    async fn set_unicast_hop_limit(
        &mut self,
        self_: Resource<wasi::sockets::udp::UdpSocket>,
        value: u8,
    ) -> Result<Result<(), wasi::sockets::network::ErrorCode>> {
        if value == 0 {
            return Ok(Err(wasi::sockets::network::ErrorCode::InvalidArgument));
        }
        self.table.get_mut(&self_)?.hop_limit = value;
        Ok(Ok(()))
    }
    async fn receive_buffer_size(
        &mut self,
        self_: Resource<wasi::sockets::udp::UdpSocket>,
    ) -> Result<Result<u64, wasi::sockets::network::ErrorCode>> {
        Ok(Ok(self.table.get(&self_)?.receive_buffer_size))
    }
    async fn set_receive_buffer_size(
        &mut self,
        self_: Resource<wasi::sockets::udp::UdpSocket>,
        value: u64,
    ) -> Result<Result<(), wasi::sockets::network::ErrorCode>> {
        if value == 0 {
            return Ok(Err(wasi::sockets::network::ErrorCode::InvalidArgument));
        }
        self.table.get_mut(&self_)?.receive_buffer_size = value;
        Ok(Ok(()))
    }
    async fn send_buffer_size(
        &mut self,
        self_: Resource<wasi::sockets::udp::UdpSocket>,
    ) -> Result<Result<u64, wasi::sockets::network::ErrorCode>> {
        Ok(Ok(self.table.get(&self_)?.send_buffer_size))
    }
    async fn set_send_buffer_size(
        &mut self,
        self_: Resource<wasi::sockets::udp::UdpSocket>,
        value: u64,
    ) -> Result<Result<(), wasi::sockets::network::ErrorCode>> {
        if value == 0 {
            return Ok(Err(wasi::sockets::network::ErrorCode::InvalidArgument));
        }
        self.table.get_mut(&self_)?.send_buffer_size = value;
        Ok(Ok(()))
    }
    async fn subscribe(
        &mut self,
        self_: Resource<wasi::sockets::udp::UdpSocket>,
    ) -> Result<Resource<PollableEntry>> {
        let entry = match &self.table.get(&self_)?.state {
            UdpSocketState::Bound { socket } => PollableEntry::UdpWritable {
                socket: socket.clone_socket(),
            },
            _ => PollableEntry::AlwaysReady,
        };
        Ok(self.table.push(entry)?)
    }
    async fn drop(
        &mut self,
        rep: Resource<wasi::sockets::udp::UdpSocket>,
    ) -> Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

impl wasi::sockets::udp::HostIncomingDatagramStream for WasiCtx {
    async fn receive(
        &mut self,
        self_: Resource<wasi::sockets::udp::IncomingDatagramStream>,
        max_results: u64,
    ) -> Result<
        Result<Vec<wasi::sockets::udp::IncomingDatagram>, wasi::sockets::network::ErrorCode>,
    > {
        if max_results == 0 {
            return Ok(Ok(Vec::new()));
        }

        // Clone socket to avoid holding table borrow across blocking I/O.
        let (socket, remote_filter) = {
            let entry = self.table.get(&self_)?;
            (entry.socket.clone_socket(), entry.remote_address)
        };

        // Do a blocking receive for the first datagram.
        let mut buf = vec![0u8; 65536];
        match socket.recv_from(&mut buf) {
            Ok((len, remote)) => {
                // If in connected mode, verify the source matches.
                if let Some(filter) = remote_filter {
                    if remote.ip != filter.ip || remote.port != filter.port {
                        return Ok(Ok(Vec::new()));
                    }
                }
                buf.truncate(len);
                Ok(Ok(vec![wasi::sockets::udp::IncomingDatagram {
                    data: buf,
                    remote_address: net_addr_to_wasi(&remote),
                }]))
            }
            Err(e) => Ok(Err(net_err_to_wasi(e))),
        }
    }
    async fn subscribe(
        &mut self,
        self_: Resource<wasi::sockets::udp::IncomingDatagramStream>,
    ) -> Result<Resource<PollableEntry>> {
        let socket = self.table.get(&self_)?.socket.clone_socket();
        Ok(self.table.push(PollableEntry::UdpReadable { socket })?)
    }
    async fn drop(
        &mut self,
        rep: Resource<wasi::sockets::udp::IncomingDatagramStream>,
    ) -> Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

impl wasi::sockets::udp::HostOutgoingDatagramStream for WasiCtx {
    async fn check_send(
        &mut self,
        _self_: Resource<wasi::sockets::udp::OutgoingDatagramStream>,
    ) -> Result<Result<u64, wasi::sockets::network::ErrorCode>> {
        Ok(Ok(64)) // Always ready.
    }
    async fn send(
        &mut self,
        self_: Resource<wasi::sockets::udp::OutgoingDatagramStream>,
        datagrams: Vec<wasi::sockets::udp::OutgoingDatagram>,
    ) -> Result<Result<u64, wasi::sockets::network::ErrorCode>> {
        if datagrams.is_empty() {
            return Ok(Ok(0));
        }

        let (socket, default_remote) = {
            let entry = self.table.get(&self_)?;
            (entry.socket.clone_socket(), entry.remote_address)
        };

        let mut sent = 0u64;
        for dg in &datagrams {
            let remote = match &dg.remote_address {
                Some(addr) => match wasi_addr_to_net(addr) {
                    Ok(a) => a,
                    Err(e) => {
                        if sent > 0 {
                            return Ok(Ok(sent));
                        }
                        return Ok(Err(e));
                    }
                },
                None => match default_remote {
                    Some(r) => r,
                    None => {
                        if sent > 0 {
                            return Ok(Ok(sent));
                        }
                        return Ok(Err(wasi::sockets::network::ErrorCode::InvalidArgument));
                    }
                },
            };
            match socket.send_to(&dg.data, remote) {
                Ok(_) => sent += 1,
                Err(e) => {
                    if sent > 0 {
                        return Ok(Ok(sent));
                    }
                    return Ok(Err(net_err_to_wasi(e)));
                }
            }
        }
        Ok(Ok(sent))
    }
    async fn subscribe(
        &mut self,
        self_: Resource<wasi::sockets::udp::OutgoingDatagramStream>,
    ) -> Result<Resource<PollableEntry>> {
        let socket = self.table.get(&self_)?.socket.clone_socket();
        Ok(self.table.push(PollableEntry::UdpWritable { socket })?)
    }
    async fn drop(
        &mut self,
        rep: Resource<wasi::sockets::udp::OutgoingDatagramStream>,
    ) -> Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

impl wasi::sockets::ip_name_lookup::Host for WasiCtx {
    async fn resolve_addresses(
        &mut self,
        _network: Resource<wasi::sockets::network::Network>,
        name: String,
    ) -> Result<
        Result<
            Resource<wasi::sockets::ip_name_lookup::ResolveAddressStream>,
            wasi::sockets::network::ErrorCode,
        >,
    > {
        use std::str::FromStr;

        // If the name is already an IP address, just parse it.
        let addresses = if let Ok(ip) = smoltcp::wire::Ipv4Address::from_str(&name) {
            vec![smoltcp::wire::IpAddress::Ipv4(ip)]
        } else {
            // Perform DNS resolution (blocking).
            match net::resolve_dns(&name) {
                Ok(addrs) => addrs,
                Err(e) => return Ok(Err(net_err_to_wasi(e))),
            }
        };

        let entry = ResolveAddressStreamEntry {
            addresses,
            index: 0,
        };
        Ok(Ok(self.table.push(entry)?))
    }
}

impl wasi::sockets::ip_name_lookup::HostResolveAddressStream for WasiCtx {
    async fn resolve_next_address(
        &mut self,
        self_: Resource<wasi::sockets::ip_name_lookup::ResolveAddressStream>,
    ) -> Result<
        Result<Option<wasi::sockets::network::IpAddress>, wasi::sockets::network::ErrorCode>,
    > {
        let entry = self.table.get_mut(&self_)?;
        if entry.index >= entry.addresses.len() {
            return Ok(Ok(None));
        }
        let addr = entry.addresses[entry.index];
        entry.index += 1;
        match addr {
            smoltcp::wire::IpAddress::Ipv4(v4) => {
                Ok(Ok(Some(wasi::sockets::network::IpAddress::Ipv4((
                    v4.0[0], v4.0[1], v4.0[2], v4.0[3],
                )))))
            }
            _ => {
                // Skip IPv6 addresses (not supported).
                Ok(Ok(None))
            }
        }
    }
    async fn subscribe(
        &mut self,
        _self_: Resource<wasi::sockets::ip_name_lookup::ResolveAddressStream>,
    ) -> Result<Resource<PollableEntry>> {
        Ok(self.table.push(PollableEntry::AlwaysReady)?)
    }
    async fn drop(
        &mut self,
        rep: Resource<wasi::sockets::ip_name_lookup::ResolveAddressStream>,
    ) -> Result<()> {
        self.table.delete(rep)?;
        Ok(())
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

// ── Socket address conversion helpers ────────────────────────────────

fn wasi_addr_to_net(
    addr: &wasi::sockets::network::IpSocketAddress,
) -> Result<net::NetAddr, wasi::sockets::network::ErrorCode> {
    match addr {
        wasi::sockets::network::IpSocketAddress::Ipv4(a) => {
            let (a0, a1, a2, a3) = a.address;
            Ok(net::NetAddr {
                ip: smoltcp::wire::Ipv4Address::new(a0, a1, a2, a3).into(),
                port: a.port,
            })
        }
        wasi::sockets::network::IpSocketAddress::Ipv6(_) => {
            Err(wasi::sockets::network::ErrorCode::NotSupported)
        }
    }
}

fn net_addr_to_wasi(addr: &net::NetAddr) -> wasi::sockets::network::IpSocketAddress {
    match addr.ip {
        smoltcp::wire::IpAddress::Ipv4(v4) => {
            wasi::sockets::network::IpSocketAddress::Ipv4(
                wasi::sockets::network::Ipv4SocketAddress {
                    port: addr.port,
                    address: (v4.0[0], v4.0[1], v4.0[2], v4.0[3]),
                },
            )
        }
        _ => unreachable!("IPv6 not supported"),
    }
}

fn net_err_to_wasi(e: net::NetError) -> wasi::sockets::network::ErrorCode {
    match e {
        net::NetError::WouldBlock => wasi::sockets::network::ErrorCode::WouldBlock,
        net::NetError::ConnectionRefused => wasi::sockets::network::ErrorCode::ConnectionRefused,
        net::NetError::ConnectionReset => wasi::sockets::network::ErrorCode::ConnectionReset,
        net::NetError::NotConnected => wasi::sockets::network::ErrorCode::InvalidState,
        net::NetError::AddrInUse => wasi::sockets::network::ErrorCode::AddressInUse,
        net::NetError::AddrNotAvailable => wasi::sockets::network::ErrorCode::AddressNotBindable,
        net::NetError::InvalidArgument => wasi::sockets::network::ErrorCode::InvalidArgument,
        net::NetError::NotSupported => wasi::sockets::network::ErrorCode::NotSupported,
        net::NetError::PortExhaustion => wasi::sockets::network::ErrorCode::AddressInUse,
        net::NetError::Other(_) => wasi::sockets::network::ErrorCode::Unknown,
    }
}

// ── wasi:graphics-context ───────────────────────────────────────────

impl wasi::graphics_context::graphics_context::Host for WasiCtx {}

impl wasi::graphics_context::graphics_context::HostContext for WasiCtx {
    async fn new(&mut self) -> Result<Resource<GfxContext>> {
        let ctx = GfxContext { has_surface: false };
        Ok(self.table.push(ctx)?)
    }

    async fn get_current_buffer(
        &mut self,
        _self_: Resource<GfxContext>,
    ) -> Result<Resource<AbstractBuffer>> {
        Ok(self.table.push(AbstractBuffer)?)
    }

    async fn present(&mut self, self_: Resource<GfxContext>) -> Result<()> {
        let ctx = self.table.get(&self_)?;
        if !ctx.has_surface {
            bail!("no surface connected to graphics context");
        }
        if let Some(window) = &self.display_window {
            let pixels = &self.display_pixels;
            let w = self.display_width;
            let h = self.display_height;
            window.window_buffer.update_buffer(|mut buf, _bw, _bh| {
                let num_pixels = (w * h) as usize;
                for i in 0..num_pixels.min(buf.len()) {
                    let base = i * 4;
                    if base + 3 < pixels.len() {
                        let r = pixels[base] as u32;
                        let g = pixels[base + 1] as u32;
                        let b = pixels[base + 2] as u32;
                        buf[i] = 0xFF000000 | (r << 16) | (g << 8) | b;
                    }
                }
                buf.damage(twizzler_display::Rect::full());
            });
            window.window_buffer.flip();
        }
        Ok(())
    }

    async fn drop(&mut self, rep: Resource<GfxContext>) -> Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

impl wasi::graphics_context::graphics_context::HostAbstractBuffer for WasiCtx {
    async fn drop(&mut self, rep: Resource<AbstractBuffer>) -> Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

// ── wasi:frame-buffer ──────────────────────────────────────────────

impl wasi::frame_buffer::frame_buffer::Host for WasiCtx {}

impl wasi::frame_buffer::frame_buffer::HostDevice for WasiCtx {
    async fn new(&mut self) -> Result<Resource<FrameBufferDevice>> {
        Ok(self.table.push(FrameBufferDevice)?)
    }

    async fn connect_graphics_context(
        &mut self,
        _self_: Resource<FrameBufferDevice>,
        _context: Resource<GfxContext>,
    ) -> Result<()> {
        // In our simplified implementation, the device is always connected
        // to the single global display context.
        Ok(())
    }

    async fn drop(&mut self, rep: Resource<FrameBufferDevice>) -> Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

impl wasi::frame_buffer::frame_buffer::HostBuffer for WasiCtx {
    async fn from_graphics_buffer(
        &mut self,
        buffer: Resource<AbstractBuffer>,
    ) -> Result<Resource<FrameBuffer>> {
        self.table.delete(buffer)?;
        let fb = FrameBuffer {
            data: Vec::new(),
        };
        Ok(self.table.push(fb)?)
    }

    async fn get(&mut self, self_: Resource<FrameBuffer>) -> Result<Vec<u8>> {
        let fb = self.table.get(&self_)?;
        Ok(fb.data.clone())
    }

    async fn set(&mut self, self_: Resource<FrameBuffer>, val: Vec<u8>) -> Result<()> {
        let fb = self.table.get_mut(&self_)?;
        fb.data = val.clone();
        // Also copy to the shared display pixel buffer for present().
        self.display_pixels = val;
        Ok(())
    }

    async fn drop(&mut self, rep: Resource<FrameBuffer>) -> Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

// ── twizzler:gfx/surface ───────────────────────────────────────────

impl twizzler::gfx::surface::Host for WasiCtx {
    async fn create(
        &mut self,
        context: Resource<GfxContext>,
        width: u32,
        height: u32,
    ) -> Result<Result<(), String>> {
        use secgate::util::Handle;
        let ctx = self.table.get_mut(&context)?;
        match twizzler_display::WindowHandle::open(twizzler_display::WindowConfig {
            w: width,
            h: height,
            x: 0,
            y: 0,
            z: 0,
        }) {
            Ok(window) => {
                self.display_window = Some(window);
                self.display_width = width;
                self.display_height = height;
                self.display_pixels = vec![0u8; (width * height * 4) as usize];
                ctx.has_surface = true;
                Ok(Ok(()))
            }
            Err(e) => Ok(Err(format!("failed to open window: {e:?}"))),
        }
    }
}

// ── twizzler:input/input ────────────────────────────────────────────

impl twizzler::input::input::Host for WasiCtx {
    async fn poll_events(&mut self) -> Result<Vec<twizzler::input::input::InputEvent>> {
        let events = crate::input::poll_events();
        Ok(events
            .into_iter()
            .map(|e| twizzler::input::input::InputEvent {
                event_type: e.event_type,
                code: e.code,
                value: e.value,
            })
            .collect())
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
    // WASI-GFX interfaces
    wasi::graphics_context::graphics_context::add_to_linker::<_, D>(linker, |t| t)?;
    wasi::frame_buffer::frame_buffer::add_to_linker::<_, D>(linker, |t| t)?;
    twizzler::gfx::surface::add_to_linker::<_, D>(linker, |t| t)?;
    // Input events
    twizzler::input::input::add_to_linker::<_, D>(linker, |t| t)?;
    Ok(())
}

// ── Minimal executor ────────────────────────────────────────────────

/// Minimal single-threaded executor for driving wasmtime async futures.
///
/// Wasmtime's fiber support handles the actual suspension/resumption —
/// this just polls the top-level future until completion.
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

    fn raw_waker() -> RawWaker {
        fn no_op(_: *const ()) {}
        fn clone(_: *const ()) -> RawWaker {
            raw_waker()
        }
        let vtable = &RawWakerVTable::new(clone, no_op, no_op, no_op);
        RawWaker::new(std::ptr::null(), vtable)
    }

    let waker = unsafe { Waker::from_raw(raw_waker()) };
    let mut cx = Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);

    loop {
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(output) => return output,
            Poll::Pending => std::thread::yield_now(),
        }
    }
}

// ── Public API ──────────────────────────────────────────────────────

/// Run a WASI P2 component (wasi:cli/command world).
///
/// Uses wasmtime's async support with Twizzler fiber stacks, enabling
/// host functions to yield the fiber during I/O waits.
pub fn run_wasi_component(component_bytes: &[u8]) -> Result<()> {
    // Try to initialize input device (non-fatal if not present).
    crate::input::init();

    let config = crate::wasmtime_config();
    let engine = Engine::new(&config)?;
    let component = Component::new(&engine, component_bytes)?;

    let mut linker = Linker::new(&engine);
    add_wasi_to_linker(&mut linker)?;

    let ctx = WasiCtx {
        table: ResourceTable::new(),
        display_window: None,
        display_width: 0,
        display_height: 0,
        display_pixels: Vec::new(),
    };
    let mut store = Store::new(&engine, ctx);

    block_on(async {
        let command = Command::instantiate_async(&mut store, &component, &linker).await?;
        match command.wasi_cli_run().call_run(&mut store).await? {
            Ok(()) => Ok(()),
            Err(()) => bail!("WASI component returned error"),
        }
    })
}
