#![allow(unused)]

use std::{
    fs::File,
    io::{Read, Write},
    num::NonZero,
    os::fd::{AsFd, FromRawFd},
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
            <Self::Target as gdbstub::target::Target>::Error,
            <Self::Connection as gdbstub::conn::Connection>::Error,
        >,
    > {
        tracing::debug!("waiting for stop reason or data");
        loop {
            if conn
                .peek()
                .map_err(|e| WaitForStopReasonError::Connection(e))?
                .is_some()
            {
                let byte = conn
                    .read()
                    .map_err(|e| WaitForStopReasonError::Connection(e))?;
                return Ok(Event::IncomingData(byte));
            } else if target.comp.info().flags.contains(CompartmentFlags::EXITED) {
                return Ok(Event::TargetStopped(MultiThreadStopReason::Exited(0)));
            } else {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    fn on_interrupt(
        target: &mut Self::Target,
    ) -> Result<Option<Self::StopReason>, <Self::Target as gdbstub::target::Target>::Error> {
        // TODO
        Ok(None)
    }
}

pub struct TwizzlerTarget {
    comp: CompartmentHandle,
}

impl TwizzlerTarget {
    pub fn new(comp: CompartmentHandle) -> Self {
        Self { comp }
    }
}

impl MultiThreadBase for TwizzlerTarget {
    fn read_registers(
        &mut self,
        regs: &mut <Self::Arch as gdbstub::arch::Arch>::Registers,
        tid: gdbstub::common::Tid,
    ) -> gdbstub::target::TargetResult<(), Self> {
        todo!()
    }

    fn write_registers(
        &mut self,
        regs: &<Self::Arch as gdbstub::arch::Arch>::Registers,
        tid: gdbstub::common::Tid,
    ) -> gdbstub::target::TargetResult<(), Self> {
        todo!()
    }

    fn read_addrs(
        &mut self,
        start_addr: <Self::Arch as gdbstub::arch::Arch>::Usize,
        data: &mut [u8],
        tid: gdbstub::common::Tid,
    ) -> gdbstub::target::TargetResult<usize, Self> {
        todo!()
    }

    fn write_addrs(
        &mut self,
        start_addr: <Self::Arch as gdbstub::arch::Arch>::Usize,
        data: &[u8],
        tid: gdbstub::common::Tid,
    ) -> gdbstub::target::TargetResult<(), Self> {
        todo!()
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
        todo!()
    }

    fn remove_sw_breakpoint(
        &mut self,
        addr: <Self::Arch as gdbstub::arch::Arch>::Usize,
        kind: <Self::Arch as gdbstub::arch::Arch>::BreakpointKind,
    ) -> gdbstub::target::TargetResult<bool, Self> {
        todo!()
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
}

impl TwizzlerConn {
    pub fn new() -> Self {
        Self { peek: None }
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
        let mut bytes = [0];
        let r = twizzler_abi::syscall::sys_kernel_console_read_debug(
            &mut bytes,
            KernelConsoleReadFlags::empty(),
        )?;
        if r == 0 {
            std::thread::sleep(Duration::from_millis(10));
            return self.read();
        }
        Ok(bytes[0])
    }

    fn peek(&mut self) -> Result<Option<u8>, Self::Error> {
        if let Some(byte) = &self.peek {
            return Ok(Some(*byte));
        }
        let mut bytes = [0];
        let r = twizzler_abi::syscall::sys_kernel_console_read_debug(
            &mut bytes,
            KernelConsoleReadFlags::NONBLOCKING,
        );
        if matches!(r, Err(TwzError::WOULD_BLOCK)) {
            return Ok(None);
        }
        if r? == 0 {
            return Ok(None);
        }
        self.peek = Some(bytes[0]);
        Ok(Some(bytes[0]))
    }
}
