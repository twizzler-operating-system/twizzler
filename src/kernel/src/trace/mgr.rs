use alloc::collections::btree_map::BTreeMap;
use core::{
    cell::UnsafeCell,
    hint::unlikely,
    mem::MaybeUninit,
    sync::atomic::{
        AtomicU64, AtomicUsize,
        Ordering::{Relaxed, SeqCst},
    },
};

use twizzler_abi::{
    object::ObjID,
    syscall::TraceSpec,
    trace::{TraceData, TraceEntryHead, TraceKind},
};
use twizzler_rt_abi::error::{ObjectError, TwzError};

use super::{buffered_trace_data::BufferedTraceData, sink::TraceSink};
use crate::mutex::Mutex;

#[derive(Debug)]
pub struct TraceEvent<T: Copy + core::fmt::Debug = ()> {
    header: TraceEntryHead,
    data: Option<TraceData<T>>,
}

impl TraceEvent<()> {
    pub fn new(head: TraceEntryHead) -> Self {
        Self {
            header: head,
            data: None,
        }
    }
}

impl<T: Copy + core::fmt::Debug> TraceEvent<T> {
    fn split(&self) -> (TraceEntryHead, BufferedTraceData) {
        (
            self.header,
            self.data
                .map(|data| BufferedTraceData::new(data))
                .unwrap_or_default(),
        )
    }

    fn split_async(self) -> (TraceEntryHead, BufferedTraceData) {
        (
            self.header,
            self.data
                .map(|data| BufferedTraceData::new_inline(data))
                .flatten()
                .unwrap_or_default(),
        )
    }

    pub fn new_with_data(head: TraceEntryHead, data: T) -> Self {
        Self {
            header: head,
            data: Some(TraceData {
                len: size_of::<TraceData<T>>() as u32,
                data,
            }),
        }
    }
}

const MAX_QUICK_ENABLED: usize = 10;
const MAX_PENDING_ASYNC: usize = 64;

pub struct TraceMgr {
    map: Mutex<BTreeMap<ObjID, TraceSink>>,
    quick_enabled: [AtomicU64; MAX_QUICK_ENABLED],
    async_buffer: UnsafeCell<[Option<(TraceEntryHead, BufferedTraceData)>; MAX_PENDING_ASYNC]>,
    async_idx: AtomicUsize,
}

unsafe impl Sync for TraceMgr {}
unsafe impl Send for TraceMgr {}

const _Z: AtomicU64 = AtomicU64::new(0);
const __Z: Option<(TraceEntryHead, BufferedTraceData)> = None;
pub static TRACE_MGR: TraceMgr = TraceMgr {
    map: Mutex::new(BTreeMap::new()),
    quick_enabled: [_Z; MAX_QUICK_ENABLED],
    async_buffer: UnsafeCell::new([__Z; MAX_PENDING_ASYNC]),
    async_idx: AtomicUsize::new(0),
};

impl TraceMgr {
    #[inline]
    pub fn any_enabled(&self, kind: TraceKind, events: u64) -> bool {
        let idx = kind as usize;
        if unlikely(idx >= MAX_QUICK_ENABLED) {
            return true;
        }

        self.quick_enabled[idx].load(Relaxed) & events != 0
    }

    pub fn enqueue<T: Copy + core::fmt::Debug>(&self, event: TraceEvent<T>) {
        let mut map = self.map.lock();
        self.drain_async(|head, data| {
            for sink in map.values_mut() {
                if sink.accepts(&head) {
                    sink.enqueue((head, data));
                }
            }
        });
        for sink in map.values_mut() {
            if sink.accepts(&event.header) {
                sink.enqueue(event.split());
            }
        }
    }

    pub fn async_enqueue<T: Copy + core::fmt::Debug>(&self, event: TraceEvent<T>) {
        let mut iter = 0;
        loop {
            iter += 1;
            let idx = self.async_idx.load(SeqCst);
            if idx > MAX_PENDING_ASYNC || iter > 1000 {
                log::warn!("dropped async trace event {:?}", event);
            }

            if idx & 1 == 1 {
                crate::arch::processor::spin_wait_iteration();
                continue;
            }

            if self
                .async_idx
                .compare_exchange(idx, idx + 1, SeqCst, SeqCst)
                .is_err()
            {
                crate::arch::processor::spin_wait_iteration();
                continue;
            }

            unsafe {
                self.async_buffer
                    .get()
                    .cast::<(TraceEntryHead, BufferedTraceData)>()
                    .add(idx / 2)
                    .write(event.split_async());
            };
            self.async_idx.fetch_add(1, SeqCst);
            return;
        }
    }

    pub fn drain_async(&self, mut f: impl FnMut(TraceEntryHead, BufferedTraceData)) {
        let mut buf = [MaybeUninit::uninit(); MAX_PENDING_ASYNC];
        loop {
            let idx = self.async_idx.load(SeqCst);
            if idx == 0 {
                return;
            }
            if idx & 1 == 1 {
                crate::arch::processor::spin_wait_iteration();
                continue;
            }

            for i in 0..(idx / 2) {
                buf[i] = MaybeUninit::new(unsafe {
                    self.async_buffer
                        .get()
                        .cast::<(TraceEntryHead, BufferedTraceData)>()
                        .add(i)
                        .read()
                });
            }

            if self
                .async_idx
                .compare_exchange(idx, 0, SeqCst, SeqCst)
                .is_err()
            {
                crate::arch::processor::spin_wait_iteration();
                continue;
            }

            log::info!("drained {} async events", idx / 2);
            for i in 0..(idx / 2) {
                let (h, d) = unsafe { buf[i].assume_init() };
                f(h, d);
            }
            return;
        }
    }

    pub fn add_sink(&self, id: ObjID, spec: TraceSpec) -> Result<(), TwzError> {
        let mut map = self.map.lock();
        if let Some(sink) = map.get_mut(&id) {
            sink.modify(spec);
        } else {
            drop(map);
            let sink = TraceSink::new(id, spec)?;
            let mut map = self.map.lock();

            if let Some(sink) = map.get_mut(&id) {
                sink.modify(spec);
            } else {
                map.insert(id, sink);
            }
        }
        Ok(())
    }

    pub fn remove_sink(&self, id: ObjID) -> Result<(), TwzError> {
        let mut map = self.map.lock();
        if let Some(sink) = map.get_mut(&id) {
            sink.write_all();
            map.remove(&id);
            Ok(())
        } else {
            Err(ObjectError::NoSuchObject.into())
        }
    }
}
