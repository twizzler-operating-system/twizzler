use twizzler_abi::{
    syscall::TimeSpan,
    trace::{TraceEntryFlags, TraceEntryHead, TraceKind},
};

use crate::{
    instant::Instant,
    processor::current_processor,
    thread::{current_memory_context, current_thread_ref},
};

pub mod buffered_trace_data;
pub mod mgr;
pub mod sink;

pub fn new_trace_entry(kind: TraceKind, event: u64, flags: TraceEntryFlags) -> TraceEntryHead {
    let now = Instant::now();
    TraceEntryHead {
        thread: current_thread_ref()
            .map(|ct| ct.objid())
            .unwrap_or_default(),
        sctx: current_thread_ref()
            .map(|ct| ct.secctx.active_id())
            .unwrap_or_default(),
        mctx: 0.into(), // TODO
        cpuid: current_processor().id as u64,
        time: now.into_time_span(),
        event,
        kind,
        extra_or_next: 0.into(),
        flags,
    }
}
