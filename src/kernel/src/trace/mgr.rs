use alloc::collections::btree_map::BTreeMap;
use core::{
    cell::UnsafeCell,
    hint::unlikely,
    sync::atomic::{
        AtomicBool, AtomicU64, AtomicUsize,
        Ordering::{self, Relaxed, SeqCst},
    },
};

use twizzler_abi::{
    object::ObjID,
    syscall::TraceSpec,
    trace::{TraceEntryFlags, TraceEntryHead, TraceKind},
};
use twizzler_rt_abi::error::{ObjectError, TwzError};

use super::{buffered_trace_data::BufferedTraceData, sink::TraceSink};
use crate::{
    condvar::CondVar,
    mutex::Mutex,
    once::Once,
    spinlock::Spinlock,
    thread::{current_thread_ref, entry::start_new_kernel, priority::Priority, ThreadRef},
};

#[derive(Debug)]
pub struct TraceEvent<T: Copy + core::fmt::Debug = ()> {
    header: TraceEntryHead,
    data: Option<T>,
}

impl TraceEvent<()> {
    pub fn new(mut head: TraceEntryHead) -> Self {
        head.flags.remove(TraceEntryFlags::HAS_DATA);
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

    pub fn new_with_data(mut head: TraceEntryHead, data: T) -> Self {
        head.flags.insert(TraceEntryFlags::HAS_DATA);
        Self {
            header: head,
            data: Some(data),
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
    async_overflow: AtomicBool,
    has_work: Spinlock<bool>,
    cv: CondVar,
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
    has_work: Spinlock::new(false),
    async_overflow: AtomicBool::new(false),
    cv: CondVar::new(),
};

static WRITE_THREAD: Once<ThreadRef> = Once::new();

impl TraceMgr {
    fn signal_work(&self) {
        let mut sig = self.has_work.lock();
        *sig = true;
        self.cv.signal();
    }

    fn update_quick_enabled(&self, kind: TraceKind, events: u64) {
        let idx = kind as usize;
        if unlikely(idx >= MAX_QUICK_ENABLED) {
            return;
        }

        self.quick_enabled[idx].store(events, Relaxed);
    }

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
                    sink.enqueue((head, data.clone()));
                }
            }
        });
        for sink in map.values_mut() {
            if sink.accepts(&event.header) {
                sink.enqueue(event.split());
            }
        }
        drop(map);
        self.signal_work();
    }

    pub fn async_enqueue<T: Copy + core::fmt::Debug>(&self, event: TraceEvent<T>) {
        const MAX_ASYNC_ITER: usize = 1000;
        let mut iter = 0;
        loop {
            iter += 1;
            let idx = self.async_idx.load(SeqCst);
            if idx > MAX_PENDING_ASYNC || iter > MAX_ASYNC_ITER {
                self.async_overflow.store(true, Ordering::SeqCst);
                log::warn!(
                    "dropped async trace event {:?} (overflow={}, timeout={})",
                    event,
                    idx > MAX_PENDING_ASYNC,
                    iter > MAX_ASYNC_ITER
                );
                return;
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
            self.signal_work();
            return;
        }
    }

    pub fn drain_async(&self, mut f: impl FnMut(TraceEntryHead, BufferedTraceData)) {
        const MU: Option<(TraceEntryHead, BufferedTraceData)> = None;
        let mut buf = [MU; MAX_PENDING_ASYNC];
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
                buf[i] = None;
                unsafe {
                    self.async_buffer
                        .get()
                        .cast::<Option<(TraceEntryHead, BufferedTraceData)>>()
                        .add(i)
                        .swap(&mut buf[i]);
                }
            }

            if self
                .async_idx
                .compare_exchange(idx, 0, SeqCst, SeqCst)
                .is_err()
            {
                crate::arch::processor::spin_wait_iteration();
                continue;
            }

            let overflow = self.async_overflow.swap(false, Ordering::SeqCst);
            log::debug!("drained {} async events (overflow={})", idx / 2, overflow);
            for i in 0..(idx / 2) {
                if let Some((mut h, d)) = buf[i].take() {
                    if i + 1 == idx / 2 && overflow {
                        h.flags.insert(TraceEntryFlags::DROPPED);
                    }
                    f(h, d);
                }
            }
            return;
        }
    }

    pub fn add_sink(&self, id: ObjID, spec: TraceSpec) -> Result<(), TwzError> {
        start_write_thread();
        let mut map = self.map.lock();
        TRACE_MGR.drain_async(|head, data| {
            for sink in map.values_mut() {
                if sink.accepts(&head) {
                    sink.enqueue((head, data.clone()));
                }
            }
        });
        if let Some(sink) = map.get_mut(&id) {
            sink.modify(spec);
            drop(map);
        } else {
            drop(map);
            let sink = TraceSink::new(id, [spec].to_vec())?;
            let mut map = self.map.lock();

            if let Some(sink) = map.get_mut(&id) {
                sink.modify(spec);
            } else {
                map.insert(id, sink);
            }
            drop(map);
        }
        self.accum_all_events();
        self.signal_work();
        Ok(())
    }

    pub fn remove_sink(&self, id: ObjID) -> Result<(), TwzError> {
        let mut map = self.map.lock();
        TRACE_MGR.drain_async(|head, data| {
            for sink in map.values_mut() {
                if sink.accepts(&head) {
                    sink.enqueue((head, data.clone()));
                }
            }
        });
        if let Some(sink) = map.get_mut(&id) {
            sink.write_all();
            map.remove(&id);
            drop(map);
            self.accum_all_events();
            Ok(())
        } else {
            Err(ObjectError::NoSuchObject.into())
        }
    }

    pub fn accum_all_events(&self) {
        let mut map = self.map.lock();
        let mut quicks = BTreeMap::<TraceKind, u64>::new();
        quicks.insert(TraceKind::Context, 0);
        quicks.insert(TraceKind::Kernel, 0);
        quicks.insert(TraceKind::Object, 0);
        quicks.insert(TraceKind::Pager, 0);
        quicks.insert(TraceKind::Security, 0);
        quicks.insert(TraceKind::Thread, 0);
        for sink in map.values_mut() {
            for spec in sink.specs() {
                let entry = quicks.entry(spec.kind).or_default();
                let events = spec.enable_events & !spec.disable_events;
                *entry |= events;
            }
        }
        for (k, e) in quicks {
            log::trace!("accum quick update: {:?}: {:x}", k, e);
            self.update_quick_enabled(k, e);
        }
    }
}

extern "C" fn kthread_trace_writer() {
    loop {
        let mut did_work = false;
        let mut map = TRACE_MGR.map.lock();
        TRACE_MGR.drain_async(|head, data| {
            did_work = true;
            for sink in map.values_mut() {
                if sink.accepts(&head) {
                    sink.enqueue((head, data.clone()));
                }
            }
        });
        for sink in map.values_mut() {
            if sink.write_all() {
                did_work = true;
            }
        }
        drop(map);
        let mut sig = TRACE_MGR.has_work.lock();
        log::trace!("ktrace thread: {} {}", did_work, *sig);
        if !*sig && !did_work {
            TRACE_MGR.cv.wait(sig);
        } else {
            *sig = false;
        }
    }
}

fn start_write_thread() {
    if current_thread_ref().is_some() {
        WRITE_THREAD.call_once(|| start_new_kernel(Priority::BACKGROUND, kthread_trace_writer, 0));
    }
}
