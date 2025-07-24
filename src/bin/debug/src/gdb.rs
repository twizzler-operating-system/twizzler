#![allow(unused)]

use core::slice;
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
        Target, TargetError,
        ext::{
            base::{
                BaseOps,
                multithread::{MultiThreadBase, MultiThreadResume, MultiThreadResumeOps},
            },
            breakpoints::{Breakpoints, SwBreakpoint},
        },
    },
};
use gdbstub_arch::x86::reg::{X86SegmentRegs, X87FpuInternalRegs};
use miette::IntoDiagnostic;
use monitor_api::{CompartmentFlags, CompartmentHandle};
use twizzler_abi::{
    arch::{ArchRegisters, XSAVE_LEN},
    object::{MAX_SIZE, NULLPAGE_SIZE, ObjID, Protections},
    syscall::{KernelConsoleReadFlags, KernelConsoleWriteFlags, sys_object_read_map},
    thread::ExecutionState,
    upcall::UpcallFrame,
};
use twizzler_rt_abi::error::{GenericError, ObjectError, ResourceError, SecurityError, TwzError};

pub struct TwizzlerGdb {}

type ChanMsg = Event<MultiThreadStopReason<u64>>;

struct TwzRegs(ArchRegisters);

impl From<TwzRegs> for gdbstub_arch::x86::reg::X86_64CoreRegs {
    fn from(value: TwzRegs) -> Self {
        gdbstub_arch::x86::reg::X86_64CoreRegs {
            regs: [
                value.0.frame.rax,
                value.0.frame.rbx,
                value.0.frame.rcx,
                value.0.frame.rdx,
                value.0.frame.rsi,
                value.0.frame.rdi,
                value.0.frame.rsp,
                value.0.frame.rbp,
                value.0.frame.r8,
                value.0.frame.r9,
                value.0.frame.r10,
                value.0.frame.r11,
                value.0.frame.r12,
                value.0.frame.r13,
                value.0.frame.r14,
                value.0.frame.r15,
            ],
            eflags: value.0.frame.rflags as u32,
            rip: value.0.frame.rip,
            segments: X86SegmentRegs {
                cs: value.0.cs,
                ss: value.0.ss,
                ds: value.0.ds,
                es: value.0.es,
                fs: value.0.fs,
                gs: value.0.gs,
            },
            st: [[0; 10]; 8],
            fpu: X87FpuInternalRegs::default(),
            xmm: [0; 16],
            mxcsr: 0,
        }
    }
}

impl From<&gdbstub_arch::x86::reg::X86_64CoreRegs> for TwzRegs {
    fn from(value: &gdbstub_arch::x86::reg::X86_64CoreRegs) -> Self {
        Self(ArchRegisters {
            frame: UpcallFrame {
                rax: value.regs[0],
                rbx: value.regs[1],
                rcx: value.regs[2],
                rdx: value.regs[3],
                rsi: value.regs[4],
                rdi: value.regs[5],
                rbp: value.regs[6],
                rsp: value.regs[7],
                r8: value.regs[8],
                r9: value.regs[9],
                r10: value.regs[10],
                r11: value.regs[11],
                r12: value.regs[12],
                r13: value.regs[13],
                r14: value.regs[14],
                r15: value.regs[15],
                xsave_region: [0; XSAVE_LEN],
                rip: value.rip,
                rflags: value.eflags as u64,
                thread_ptr: 0,
                prior_ctx: 0.into(),
            },
            fs: value.segments.fs,
            gs: value.segments.gs,
            es: value.segments.es,
            ds: value.segments.ds,
            ss: value.segments.ss,
            cs: value.segments.cs,
        })
    }
}

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
    thread_repr_id: ObjID,
}

impl Drop for TwizzlerTarget {
    fn drop(&mut self) {
        self.inner.done.store(true, Ordering::SeqCst);
    }
}

impl TwizzlerTarget {
    pub fn new(comp: CompartmentHandle, conn: Sender<u8>) -> Self {
        let (send, recv) = std::sync::mpsc::channel();
        let thread_repr_id = comp
            .threads()
            .nth(0)
            .map(|t| t.repr_id)
            .unwrap_or(ObjID::new(0));
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
            thread_repr_id,
        }
    }

    fn mem_access(
        &mut self,
        addr: <<TwizzlerTarget as gdbstub::target::Target>::Arch as gdbstub::arch::Arch>::Usize,
        len: usize,
        prot: Protections,
    ) -> gdbstub::target::TargetResult<usize, Self> {
        let slot = addr as usize / MAX_SIZE;
        let info = sys_object_read_map(None, slot).map_err(|e| TargetError::Io(e.into()))?;
        if prot & info.prot != prot {
            return Err(TargetError::Io(
                TwzError::Generic(GenericError::AccessDenied).into(),
            ));
        }
        if (addr as usize % MAX_SIZE) < NULLPAGE_SIZE {
            return Err(TargetError::Io(
                TwzError::Generic(GenericError::AccessDenied).into(),
            ));
        }
        let max_len = (MAX_SIZE - NULLPAGE_SIZE) - (addr as usize % MAX_SIZE);
        Ok(len.min(max_len))
    }

    fn mem_slice(
        &mut self,
        addr: <<TwizzlerTarget as gdbstub::target::Target>::Arch as gdbstub::arch::Arch>::Usize,
        len: usize,
    ) -> gdbstub::target::TargetResult<&[u8], Self> {
        let slice = unsafe { slice::from_raw_parts(addr as *const u8, len) };
        Ok(slice)
    }

    fn mem_slice_mut(
        &mut self,
        addr: <<TwizzlerTarget as gdbstub::target::Target>::Arch as gdbstub::arch::Arch>::Usize,
        len: usize,
    ) -> gdbstub::target::TargetResult<&mut [u8], Self> {
        let slice = unsafe { slice::from_raw_parts_mut(addr as *mut u8, len) };
        Ok(slice)
    }

    fn get_thread_repr_id(&self, _tid: gdbstub::common::Tid) -> ObjID {
        self.thread_repr_id
    }
}

impl MultiThreadBase for TwizzlerTarget {
    fn read_registers(
        &mut self,
        regs: &mut <Self::Arch as gdbstub::arch::Arch>::Registers,
        tid: gdbstub::common::Tid,
    ) -> gdbstub::target::TargetResult<(), Self> {
        tracing::debug!("reading regs from {}", tid);
        let old_state = twizzler_abi::syscall::sys_thread_change_state(
            self.get_thread_repr_id(tid),
            ExecutionState::Suspended,
        )
        .map_err(|e| TargetError::Io(e.into()))?;
        let twzregs =
            twizzler_abi::syscall::sys_thread_read_registers(self.get_thread_repr_id(tid))
                .map_err(|e| TargetError::Io(e.into()))?;
        *regs = TwzRegs(twzregs).into();

        tracing::debug!("{}: old state = {:?}", tid, old_state);
        if old_state == ExecutionState::Running {
            twizzler_abi::syscall::sys_thread_change_state(
                self.get_thread_repr_id(tid),
                ExecutionState::Running,
            )
            .map_err(|e| TargetError::Io(e.into()))?;
        }
        Ok(())
    }

    fn write_registers(
        &mut self,
        regs: &<Self::Arch as gdbstub::arch::Arch>::Registers,
        tid: gdbstub::common::Tid,
    ) -> gdbstub::target::TargetResult<(), Self> {
        let twzregs = TwzRegs::from(regs);
        twizzler_abi::syscall::sys_thread_write_registers(self.get_thread_repr_id(tid), &twzregs.0)
            .map_err(|e| TargetError::Io(e.into()))?;
        Ok(())
    }

    fn read_addrs(
        &mut self,
        start_addr: <Self::Arch as gdbstub::arch::Arch>::Usize,
        data: &mut [u8],
        tid: gdbstub::common::Tid,
    ) -> gdbstub::target::TargetResult<usize, Self> {
        tracing::debug!("read addrs: {:x} {}", start_addr, data.len());
        let len = self.mem_access(start_addr, data.len(), Protections::READ)?;
        let slice = self.mem_slice(start_addr, len)?;
        (&mut data[0..len]).copy_from_slice(slice);
        Ok(len)
    }

    fn write_addrs(
        &mut self,
        start_addr: <Self::Arch as gdbstub::arch::Arch>::Usize,
        data: &[u8],
        tid: gdbstub::common::Tid,
    ) -> gdbstub::target::TargetResult<(), Self> {
        tracing::debug!("write addrs: {:x} {}", start_addr, data.len());
        let len = self.mem_access(start_addr, data.len(), Protections::WRITE)?;
        if len < data.len() {
            return Err(TargetError::Io(
                TwzError::Generic(GenericError::AccessDenied).into(),
            ));
        }
        let slice = self.mem_slice_mut(start_addr, len)?;
        slice.copy_from_slice(data);
        Ok(())
    }

    fn list_active_threads(
        &mut self,
        thread_is_active: &mut dyn FnMut(gdbstub::common::Tid),
    ) -> Result<(), Self::Error> {
        // TODO: support multithreading
        thread_is_active(NonZero::new(1).unwrap());
        Ok(())
    }

    fn support_resume(&mut self) -> Option<MultiThreadResumeOps<'_, Self>> {
        Some(self)
    }
}

impl MultiThreadResume for TwizzlerTarget {
    fn resume(&mut self) -> Result<(), Self::Error> {
        twizzler_abi::syscall::sys_thread_change_state(
            self.get_thread_repr_id(NonZero::new(1).unwrap()),
            ExecutionState::Running,
        )?;
        Ok(())
    }

    fn clear_resume_actions(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }

    fn set_resume_action_continue(
        &mut self,
        tid: gdbstub::common::Tid,
        signal: Option<gdbstub::common::Signal>,
    ) -> Result<(), Self::Error> {
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
