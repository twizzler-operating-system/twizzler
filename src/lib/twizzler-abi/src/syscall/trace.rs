use twizzler_rt_abi::{error::TwzError, object::ObjID};

use super::{convert_codes_to_result, twzerr, Syscall};
use crate::{
    arch::syscall::raw_syscall,
    trace::{TraceEntryHead, TraceFlags, TraceKind},
};

/// Tracing specification. Note that events can be disabled and enabled in one spec. It is
/// unspecified if events are disabled or enabled first.
#[derive(Debug, Clone, Copy)]
pub struct TraceSpec {
    /// The event kind to track.
    pub kind: TraceKind,
    /// Flags for this specification.
    pub flags: TraceFlags,
    /// Events to enable.
    pub enable_events: u64,
    /// Events to disable.
    pub disable_events: u64,
    /// Optionally restrict events to given security context.
    pub sctx: Option<ObjID>,
    /// Optionally restrict events to given memory context.
    pub mctx: Option<ObjID>,
    /// Optionally restrict events to given thread.
    pub thread: Option<ObjID>,
    /// Optionally restrict events to given CPU.
    pub cpuid: Option<u64>,
    /// Extra data passed in trace events that match this spec.
    pub extra: ObjID,
}

impl TraceSpec {
    pub fn accepts(&self, header: &TraceEntryHead) -> bool {
        if header.kind != self.kind {
            return false;
        }

        let events_match = (self.enable_events & (header.event & !self.disable_events)) != 0;

        let cpu_ok = self.cpuid.is_none_or(|x| x == header.cpuid);
        let sctx_ok = self.sctx.is_none_or(|x| x == header.sctx);
        let mctx_ok = self.mctx.is_none_or(|x| x == header.mctx);
        let thread_ok = self.thread.is_none_or(|x| x == header.thread);

        cpu_ok && sctx_ok && mctx_ok && thread_ok && events_match
    }
}

/// Trace events in the kernel, storing them in the provided object.
///
/// If spec is Some(_), then that TraceSpec will be used to modify the traced events
/// that will be placed into the object. Note that events can be disabled or enabled
/// with fine grained control.
///
/// If spec is None, all associated tracing events to the supplied object are disabled.
/// Any buffered trace events are flushed to the object.
pub fn sys_ktrace(object: ObjID, spec: Option<&TraceSpec>) -> Result<(), TwzError> {
    let [hi, lo] = object.parts();
    let (code, val) = unsafe {
        raw_syscall(
            Syscall::Ktrace,
            &[
                hi,
                lo,
                spec.map(|spec| spec as *const _ as usize as u64)
                    .unwrap_or(0),
            ],
        )
    };
    convert_codes_to_result(code, val, |c, _| c != 0, |_, _| (), twzerr)
}
