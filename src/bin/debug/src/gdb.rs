#![allow(unused)]

use std::{
    fs::File,
    io::{Read, Write},
    num::NonZero,
    os::fd::{AsFd, FromRawFd},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, Sender},
    },
    thread::JoinHandle,
    time::Duration,
};

use gdbstub::{
    conn::{Connection, ConnectionExt},
    stub::{
        MultiThreadStopReason,
        run_blocking::{Event, WaitForStopReasonError},
    },
    target::{
        Target,
        ext::{
            base::{BaseOps, multithread::MultiThreadBase},
            breakpoints::{Breakpoints, SwBreakpoint},
        },
    },
};
use miette::IntoDiagnostic;
use monitor_api::{CompartmentFlags, CompartmentHandle};
use twizzler_abi::syscall::{KernelConsoleReadFlags, KernelConsoleWriteFlags};
use twizzler_rt_abi::error::TwzError;

pub struct TwizzlerGdb {}

type ChanMsg = Event<MultiThreadStopReason<u64>>;

impl TwizzlerGdb {
    fn mon_main(inner: Arc<TargetInner>) {
        let mut flags: CompartmentFlags = inner.comp.info().flags;
        while !inner.done.load(Ordering::SeqCst) {
            if flags.contains(CompartmentFlags::EXITED) {
                let _ = inner
                    .send
                    .send(Event::TargetStopped(MultiThreadStopReason::Exited(
                        0, //inner.comp.info().code,
                    )));
                break;
            }
            flags = inner.comp.wait(flags);
        }
    }

    fn chan_main(inner: Arc<TargetInner>) {
        while !inner.done.load(Ordering::SeqCst) {
            let mut bytes = [0];
            let r = twizzler_abi::syscall::sys_kernel_console_read_debug(
                &mut bytes,
                KernelConsoleReadFlags::empty(),
            );
            if matches!(r, Ok(1)) {
                inner.conn.send(bytes[0]);
            }
            inner.send.send(Event::IncomingData(0));
        }
    }
}

impl gdbstub::stub::run_blocking::BlockingEventLoop for TwizzlerGdb {
    type Target = TwizzlerTarget;

    type Connection = TwizzlerConn;

    type StopReason = MultiThreadStopReason<u64>;

    fn wait_for_stop_reason(
        target: &mut Self::Target,
        conn: &mut Self::Connection,
    ) -> Result<
        Event<Self::StopReason>,
        WaitForStopReasonError<
            <Self::Target as Target>::Error,
            <Self::Connection as Connection>::Error,
        >,
    > {
        loop {
            let event = target.recv.recv().unwrap();
            match event {
                Event::IncomingData(_) => {
                    if conn
                        .peek()
                        .map_err(|e| WaitForStopReasonError::Connection(e))?
                        .is_some()
                    {
                        let byte = conn
                            .read()
                            .map_err(|e| WaitForStopReasonError::Connection(e))?;
                        return Ok(Event::IncomingData(byte));
                    }
                }
                Event::TargetStopped(_) => {
                    return Ok(event);
                }
            }
        }
    }

    fn on_interrupt(
        target: &mut Self::Target,
    ) -> Result<Option<Self::StopReason>, <Self::Target as Target>::Error> {
        // TODO
        Ok(None)
    }
}

pub struct TargetInner {
    done: AtomicBool,
    comp: CompartmentHandle,
    send: Sender<ChanMsg>,
    conn: Sender<u8>,
}

pub struct TwizzlerTarget {
    recv: Receiver<ChanMsg>,
    inner: Arc<TargetInner>,
    mon_t: JoinHandle<()>,
    chan_t: JoinHandle<()>,
}

impl Drop for TwizzlerTarget {
    fn drop(&mut self) {
        self.inner.done.store(true, Ordering::SeqCst);
    }
}

impl TwizzlerTarget {
    pub fn new(comp: CompartmentHandle, conn: Sender<u8>) -> Self {
        let (send, recv) = std::sync::mpsc::channel();
        let inner = Arc::new(TargetInner {
            done: AtomicBool::new(false),
            comp,
            send,
            conn,
        });
        let inner_t = inner.clone();
        let chan_t = std::thread::spawn(|| {
            TwizzlerGdb::chan_main(inner_t);
        });
        let inner_t = inner.clone();
        let mon_t = std::thread::spawn(|| {
            TwizzlerGdb::mon_main(inner_t);
        });
        Self {
            recv,
            inner,
            mon_t,
            chan_t,
        }
    }
}

impl MultiThreadBase for TwizzlerTarget {
    fn read_registers(
        &mut self,
        regs: &mut <Self::Arch as gdbstub::arch::Arch>::Registers,
        tid: gdbstub::common::Tid,
    ) -> gdbstub::target::TargetResult<(), Self> {
        Ok(())
    }

    fn write_registers(
        &mut self,
        regs: &<Self::Arch as gdbstub::arch::Arch>::Registers,
        tid: gdbstub::common::Tid,
    ) -> gdbstub::target::TargetResult<(), Self> {
        Ok(())
    }

    fn read_addrs(
        &mut self,
        start_addr: <Self::Arch as gdbstub::arch::Arch>::Usize,
        data: &mut [u8],
        tid: gdbstub::common::Tid,
    ) -> gdbstub::target::TargetResult<usize, Self> {
        Ok(0)
    }

    fn write_addrs(
        &mut self,
        start_addr: <Self::Arch as gdbstub::arch::Arch>::Usize,
        data: &[u8],
        tid: gdbstub::common::Tid,
    ) -> gdbstub::target::TargetResult<(), Self> {
        Ok(())
    }

    fn list_active_threads(
        &mut self,
        thread_is_active: &mut dyn FnMut(gdbstub::common::Tid),
    ) -> Result<(), Self::Error> {
        thread_is_active(NonZero::new(1).unwrap());
        Ok(())
    }
}

impl Breakpoints for TwizzlerTarget {
    fn support_sw_breakpoint(
        &mut self,
    ) -> Option<gdbstub::target::ext::breakpoints::SwBreakpointOps<'_, Self>> {
        Some(self)
    }

    fn support_hw_breakpoint(
        &mut self,
    ) -> Option<gdbstub::target::ext::breakpoints::HwBreakpointOps<'_, Self>> {
        None
    }

    fn support_hw_watchpoint(
        &mut self,
    ) -> Option<gdbstub::target::ext::breakpoints::HwWatchpointOps<'_, Self>> {
        None
    }
}

impl SwBreakpoint for TwizzlerTarget {
    fn add_sw_breakpoint(
        &mut self,
        addr: <Self::Arch as gdbstub::arch::Arch>::Usize,
        kind: <Self::Arch as gdbstub::arch::Arch>::BreakpointKind,
    ) -> gdbstub::target::TargetResult<bool, Self> {
        Ok(false)
    }

    fn remove_sw_breakpoint(
        &mut self,
        addr: <Self::Arch as gdbstub::arch::Arch>::Usize,
        kind: <Self::Arch as gdbstub::arch::Arch>::BreakpointKind,
    ) -> gdbstub::target::TargetResult<bool, Self> {
        Ok(false)
    }
}

impl Target for TwizzlerTarget {
    type Arch = gdbstub_arch::x86::X86_64_SSE;

    type Error = TwzError;

    fn base_ops(&mut self) -> BaseOps<'_, Self::Arch, Self::Error> {
        BaseOps::MultiThread(self)
    }

    fn guard_rail_implicit_sw_breakpoints(&self) -> bool {
        true
    }
}

pub struct TwizzlerConn {
    peek: Option<u8>,
    recv: Receiver<u8>,
}

impl TwizzlerConn {
    pub fn new(recv: Receiver<u8>) -> Self {
        Self { peek: None, recv }
    }
}

impl Connection for TwizzlerConn {
    type Error = TwzError;

    fn write(&mut self, byte: u8) -> Result<(), Self::Error> {
        twizzler_abi::syscall::sys_kernel_console_write(
            &[byte],
            KernelConsoleWriteFlags::DEBUG_CONSOLE,
        );
        Ok(())
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl ConnectionExt for TwizzlerConn {
    fn read(&mut self) -> Result<u8, Self::Error> {
        if let Some(byte) = self.peek.take() {
            return Ok(byte);
        }
        Ok(self.recv.recv().unwrap())
    }

    fn peek(&mut self) -> Result<Option<u8>, Self::Error> {
        if let Some(byte) = &self.peek {
            return Ok(Some(*byte));
        }
        if let Ok(byte) = self.recv.try_recv() {
            self.peek = Some(byte);
            Ok(Some(byte))
        } else {
            Ok(None)
        }
    }
}
