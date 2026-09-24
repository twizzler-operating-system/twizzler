use std::{sync::Arc, time::Duration};

use libc::S_IFSOCK;
use twizzler_io::pipe::Pipe;
use twizzler_rt_abi::{
    bindings::{wait_kind, WAIT_WRITE},
    fd::{FdFlags, FdInfo, FdKind},
    io::IoFlags,
    Result,
};

use crate::runtime::file::{Fd, FdImpl, WaitpointResult};

/// One end of a socketpair: two pipes cross-wired, so each end reads what the other writes.
/// Backs `socketpair(AF_UNIX, SOCK_STREAM)`, which is all the unix-socket surface portable
/// code (signal-hook's wakeup channel, `UnixStream::pair`) actually exercises here.
pub struct SocketPairEnd {
    rx: Pipe,
    tx: Pipe,
}

impl SocketPairEnd {
    /// Create both pipes and return the first end. The peer comes from [`Self::peer`].
    pub fn create() -> Result<Self> {
        let rx = Pipe::create_object(Default::default())?;
        let tx = Pipe::create_object(Default::default())?;
        Ok(Self::assemble(rx, tx))
    }

    /// The other end of `self`'s pair. Opening claims fresh reader/writer roles on the shared
    /// pipe objects, so EOF and broken-pipe accounting works the same as for a plain pipe.
    pub fn peer(&self) -> Result<Self> {
        let rx = Pipe::open_object(self.tx.id())?;
        let tx = Pipe::open_object(self.rx.id())?;
        Ok(Self::assemble(rx, tx))
    }

    // Both constructors claim reader and writer; keep only the role each side plays.
    fn assemble(rx: Pipe, tx: Pipe) -> Self {
        rx.close_writer();
        tx.close_reader();
        Self { rx, tx }
    }
}

impl Fd for SocketPairEnd {
    fn read(
        &self,
        buf: &mut [u8],
        flags: IoFlags,
        _offset: Option<u64>,
        _ep: Option<&mut twizzler_rt_abi::io::Endpoint>,
    ) -> Result<usize> {
        self.rx
            .read(buf, flags.contains(IoFlags::NONBLOCKING))
            .map_err(Into::into)
    }

    fn write(
        &self,
        buf: &[u8],
        flags: IoFlags,
        _offset: Option<u64>,
        _to: Option<&twizzler_rt_abi::io::Endpoint>,
    ) -> Result<usize> {
        self.tx
            .write(buf, flags.contains(IoFlags::NONBLOCKING))
            .map_err(Into::into)
    }

    fn stat(&self) -> Result<FdInfo> {
        Ok(FdInfo {
            size: 0,
            kind: FdKind::Pipe,
            flags: FdFlags::empty(),
            id: self.rx.id().raw(),
            created: Duration::ZERO,
            accessed: Duration::ZERO,
            modified: Duration::ZERO,
            unix_mode: S_IFSOCK | 0o666,
            nlink: 1,
        })
    }

    fn seek(&self, _pos: std::io::SeekFrom) -> Result<usize> {
        Ok(0)
    }

    fn flush(&self) -> Result<()> {
        Ok(())
    }

    fn fd_cmd(&self, _cmd: u32, _arg: *const u8, _ret: *mut u8) -> Result<()> {
        Ok(())
    }

    fn waitpoint(&self, kind: wait_kind) -> Result<WaitpointResult> {
        if kind == WAIT_WRITE {
            self.tx.waitpoint(kind)
        } else {
            self.rx.waitpoint(kind)
        }
    }

    fn shutdown(&self, sh: std::net::Shutdown) -> Result<()> {
        if matches!(sh, std::net::Shutdown::Read | std::net::Shutdown::Both) {
            self.rx.close_reader();
        }
        if matches!(sh, std::net::Shutdown::Write | std::net::Shutdown::Both) {
            self.tx.close_writer();
        }
        Ok(())
    }

    fn as_socketpair(&self) -> Option<&SocketPairEnd> {
        Some(self)
    }

    fn dup(&self) -> Option<FdImpl> {
        Some(Arc::new(SocketPairEnd {
            rx: self.rx.clone(),
            tx: self.tx.clone(),
        }))
    }
}
