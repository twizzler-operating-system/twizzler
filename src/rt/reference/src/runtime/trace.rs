use std::{
    alloc::Layout,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        OnceLock,
    },
    time::{Duration, Instant},
    usize,
};

use secgate::{get_sctx_id, get_thread_id};
use twizzler_abi::{
    object::{ObjID, MAX_SIZE, NULLPAGE_SIZE},
    simple_mutex::Mutex,
    syscall::{
        sys_object_create, sys_thread_sync, ObjectCreate, ThreadSync, ThreadSyncReference,
        ThreadSyncWake,
    },
    trace::{
        RuntimeAllocationEvent, TraceBase, TraceData, TraceEntryFlags, TraceEntryHead, TraceKind,
        RUNTIME_ALLOC,
    },
};
use twizzler_rt_abi::object::{MapFlags, ObjectHandle};

use super::RuntimeState;
use crate::OUR_RUNTIME;

struct TraceSink {
    _prime: ObjectHandle,
    current: ObjectHandle,
    pos: u64,
    start: Instant,
}

const TRACE_START: u64 = NULLPAGE_SIZE as u64 * 2;
fn write_base(handle: &ObjectHandle, start: u64) {
    unsafe {
        handle
            .start()
            .add(NULLPAGE_SIZE)
            .cast::<TraceBase>()
            .write(TraceBase {
                end: AtomicU64::new(start),
                start,
            });
    };
}

impl TraceSink {
    pub fn new(id: ObjID) -> Option<Self> {
        tracing::info!("initializing runtime trace object {}", id);
        let prime = OUR_RUNTIME
            .map_object(id, MapFlags::READ | MapFlags::WRITE)
            .ok()?;
        let start = TRACE_START;
        write_base(&prime, start);
        let this = Self {
            current: prime.clone(),
            _prime: prime,
            pos: start,
            start: Instant::now(),
        };
        this.write_endpoint(start);
        Some(this)
    }

    fn write_endpoint(&self, new_end: u64) {
        let end = unsafe { &*self.current.start().add(NULLPAGE_SIZE).cast::<AtomicU64>() };
        end.store(new_end, Ordering::SeqCst);
        let _ = sys_thread_sync(
            &mut [ThreadSync::new_wake(ThreadSyncWake::new(
                ThreadSyncReference::Virtual(end),
                usize::MAX,
            ))],
            None,
        )
        .inspect_err(|e| tracing::warn!("failed to wake tracing end point: {}", e));
    }

    pub fn push_record<T: Copy + core::fmt::Debug>(
        &mut self,
        mut record: TraceEntryHead,
        trace_data: Option<T>,
    ) {
        if trace_data.is_some() {
            record.flags.insert(TraceEntryFlags::HAS_DATA);
        }
        let data = (&record) as *const TraceEntryHead as *const u8;
        let len = size_of::<TraceEntryHead>();
        unsafe {
            let start = self.current.start().add(self.pos as usize);
            let slice = core::slice::from_raw_parts_mut(start, len);
            let data = core::slice::from_raw_parts(data, len);
            slice.copy_from_slice(data);
        }
        self.pos += len as u64;

        if let Some(trace_data) = trace_data {
            let data_len = (size_of::<TraceData<()>>() + size_of::<T>())
                .next_multiple_of(align_of::<TraceEntryHead>());
            let data_header = TraceData {
                resv: 0,
                len: data_len as u32,
                flags: 0,
                data: (),
            };
            let data = (&data_header) as *const TraceData<()> as *const u8;
            unsafe {
                let start = self.current.start().add(self.pos as usize);
                let slice = core::slice::from_raw_parts_mut(start, size_of::<TraceData<()>>());
                let data = core::slice::from_raw_parts(data, size_of::<TraceData<()>>());
                slice.copy_from_slice(data);
            }
            self.pos += size_of::<TraceData<()>>() as u64;

            let data = (&trace_data) as *const T as *const u8;
            unsafe {
                let start = self.current.start().add(self.pos as usize);
                let slice = core::slice::from_raw_parts_mut(start, size_of::<T>());
                let data = core::slice::from_raw_parts(data, size_of::<T>());
                slice.copy_from_slice(data);
            }
            self.pos += size_of::<T>() as u64;
        }
        self.pos = self
            .pos
            .next_multiple_of(align_of::<TraceEntryHead>() as u64);
        self.write_endpoint(self.pos);
    }

    pub fn check_space(&mut self) -> bool {
        const TRACE_MAX: u64 = MAX_SIZE as u64 / 2;
        if self.pos >= TRACE_MAX {
            let create = ObjectCreate::default();
            let new_id = match sys_object_create(create, &[], &[]) {
                Err(e) => {
                    tracing::warn!("failed to allocate new runtime tracing object: {}", e);
                    return false;
                }
                Ok(id) => id,
            };
            tracing::info!("initializing continued runtime trace object {}", new_id);
            let new_handle = match OUR_RUNTIME.map_object(new_id, MapFlags::READ | MapFlags::WRITE)
            {
                Err(e) => {
                    tracing::warn!("failed to map new runtime tracing object: {}", e);
                    return false;
                }
                Ok(id) => id,
            };

            let new_start = TRACE_START;
            write_base(&new_handle, new_start);

            self.push_record::<()>(TraceEntryHead::new_next_object(new_id), None);

            self.current = new_handle;
            self.pos = new_start;
            self.write_endpoint(new_start);
        }
        true
    }
}

struct OnceTraceSink {
    key: &'static str,
    val: OnceLock<Option<Mutex<TraceSink>>>,
}

impl OnceTraceSink {
    pub const fn new(key: &'static str) -> Self {
        Self {
            key,
            val: OnceLock::new(),
        }
    }

    pub fn get(&self) -> Option<&Mutex<TraceSink>> {
        self.val
            .get_or_init(|| {
                std::env::var(self.key)
                    .ok()
                    .and_then(|s| u128::from_str_radix(&s, 16).ok())
                    .and_then(|id| TraceSink::new(id.into()).map(|ts| Mutex::new(ts)))
            })
            .as_ref()
    }
}

static ENV_TRACE_OBJECT: OnceTraceSink = OnceTraceSink::new("TWZRT_TRACE_OBJECT");

static DISABLE_ALLOC: AtomicBool = AtomicBool::new(false);

#[allow(dead_code)]
pub fn trace_runtime_alloc(addr: usize, layout: Layout, duration: Duration, is_free: bool) {
    if !OUR_RUNTIME.state().contains(RuntimeState::READY) {
        return;
    }
    if DISABLE_ALLOC.swap(true, Ordering::SeqCst) {
        return;
    }
    if let Some(ts) = ENV_TRACE_OBJECT.get() {
        let mut ts = ts.lock();
        if ts.check_space() {
            let record = TraceEntryHead {
                thread: get_thread_id(),
                sctx: get_sctx_id(),
                mctx: 0.into(),
                cpuid: 0,
                time: (Instant::now() - ts.start).into(),
                event: RUNTIME_ALLOC,
                kind: TraceKind::Runtime,
                extra_or_next: 0.into(),
                flags: TraceEntryFlags::empty(),
            };
            let data = RuntimeAllocationEvent {
                duration: duration.into(),
                layout,
                addr: addr as u64,
                is_free,
            };
            ts.push_record(record, Some(data));
        }
    }
    DISABLE_ALLOC.store(false, Ordering::SeqCst);
}
