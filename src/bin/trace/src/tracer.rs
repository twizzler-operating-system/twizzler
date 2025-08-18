use std::{
    fmt::Debug,
    sync::{
        Condvar, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread::Builder,
    time::Instant,
    usize,
};

use miette::IntoDiagnostic;
use monitor_api::{CompartmentFlags, CompartmentHandle};
use twizzler::{
    BaseType, Invariant,
    object::{MapFlags, ObjID, Object, ObjectBuilder, RawObject, TypedObject},
};
use twizzler_abi::{
    syscall::{
        ObjectCreate, PERTHREAD_TRACE_GEN_SAMPLE, ThreadSync, ThreadSyncFlags, ThreadSyncOp,
        ThreadSyncReference, ThreadSyncSleep, ThreadSyncWake, TraceSpec, sys_ktrace,
        sys_thread_change_state, sys_thread_set_trace_events, sys_thread_sync,
    },
    thread::ExecutionState,
    trace::{TraceBase, TraceData, TraceEntryFlags, TraceEntryHead},
};

use crate::Cli;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Setup,
    Ready,
    Running,
    Done,
}

pub struct TracingState {
    objects: Vec<Object<BaseWrap>>,
    end_point: u64,
    pub total: u64,
    state: State,
    pub start_time: Instant,
    pub end_time: Instant,
    pub name: String,
}

impl Debug for TracingState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "TracingState {{ {} objects, end_point: {}, total: {}, state: {:?} }}",
            self.objects.len(),
            self.end_point,
            self.total,
            self.state
        )
    }
}

#[derive(BaseType, Invariant)]
#[repr(transparent)]
struct BaseWrap(TraceBase);

impl TracingState {
    fn new(name: String, specs: &[TraceSpec]) -> miette::Result<Self> {
        let prime = ObjectBuilder::new(ObjectCreate::default())
            .build(BaseWrap(TraceBase {
                start: 0,
                end: AtomicU64::new(0),
            }))
            .into_diagnostic()?;

        for spec in specs {
            sys_ktrace(prime.id(), Some(spec)).into_diagnostic()?;
        }

        Ok(Self {
            objects: vec![prime],
            end_point: 0,
            total: 0,
            state: State::Setup,
            start_time: Instant::now(),
            end_time: Instant::now(),
            name,
        })
    }

    fn collect(&mut self) -> miette::Result<ThreadSyncSleep> {
        let mut current = self.objects.last().unwrap();
        let posted_end = current.base().0.end.load(Ordering::SeqCst);
        let start_point = self.end_point.max(current.base().0.start);
        tracing::trace!(
            "collect {:x}: {:x} {:x}: {}",
            self.end_point,
            posted_end,
            start_point,
            self.objects.len()
        );
        if self.end_point != posted_end {
            let amount = posted_end.saturating_sub(start_point);

            if amount > 0 {
                self.total += amount;

                // scan for next object directives
                let mut offset = 0usize;
                while offset < amount as usize {
                    let header = current
                        .lea(start_point as usize + offset, size_of::<TraceEntryHead>())
                        .unwrap()
                        .cast::<TraceEntryHead>();
                    let header = unsafe { &*header };
                    if header.flags.contains(TraceEntryFlags::NEXT_OBJECT) {
                        tracing::debug!("got next tracing object: {}", header.extra_or_next);
                        let next = unsafe {
                            Object::<BaseWrap>::map_unchecked(header.extra_or_next, MapFlags::READ)
                                .into_diagnostic()
                        }?;
                        self.objects.push(next);
                        current = self.objects.last().unwrap();
                        self.end_point = current.base().0.start;
                        return self.collect();
                    } else {
                        offset += size_of::<TraceEntryHead>();
                        if header.flags.contains(TraceEntryFlags::HAS_DATA) {
                            let data_header = current
                                .lea(start_point as usize + offset, size_of::<TraceData<()>>())
                                .unwrap()
                                .cast::<TraceData<()>>();
                            offset += (unsafe { *data_header }).len as usize;
                        }
                    }
                }
                if offset == amount as usize {
                    self.end_point += amount;
                }
            }
        }

        Ok(ThreadSyncSleep::new(
            ThreadSyncReference::Virtual(&current.base().0.end),
            start_point,
            ThreadSyncOp::Equal,
            ThreadSyncFlags::empty(),
        ))
    }

    pub fn data(&self) -> TraceDataIter<'_> {
        TraceDataIter {
            state: self,
            pos: 0,
            inner_pos: 0,
        }
    }
}

pub struct TraceDataIter<'a> {
    state: &'a TracingState,
    pos: usize,
    inner_pos: u64,
}

#[allow(dead_code)]
struct Tracer {
    state: Mutex<TracingState>,
    specs: Vec<TraceSpec>,
    state_cv: Condvar,
    notifier: AtomicU64,
}

impl<'a> Iterator for TraceDataIter<'a> {
    type Item = (&'a TraceEntryHead, Option<&'a TraceData<()>>);

    fn next(&mut self) -> Option<Self::Item> {
        let obj = self.state.objects.get(self.pos)?;
        let start_pos = self.inner_pos.max(obj.base().0.start);
        self.inner_pos = start_pos;
        let end = obj.base().0.end.load(Ordering::SeqCst);
        if start_pos + size_of::<TraceEntryHead>() as u64 > end {
            self.pos += 1;
            self.inner_pos = 0;
            return self.next();
        }
        let mut len = size_of::<TraceEntryHead>();
        let header = obj
            .lea(start_pos as usize, len)
            .unwrap()
            .cast::<TraceEntryHead>();
        let header = unsafe { header.as_ref().unwrap() };
        let data = if header.flags.contains(TraceEntryFlags::HAS_DATA) {
            let data_header = obj
                .lea(
                    start_pos as usize + size_of::<TraceEntryHead>(),
                    size_of::<TraceData<()>>(),
                )
                .unwrap()
                .cast::<TraceData<()>>();
            let data_header = unsafe { data_header.as_ref().unwrap() };
            let data = obj
                .lea(
                    start_pos as usize + size_of::<TraceEntryHead>(),
                    data_header.len as usize,
                )
                .unwrap()
                .cast::<TraceData<()>>();
            let data = unsafe { data.as_ref().unwrap() };
            len += data.len as usize;
            Some(data)
        } else {
            None
        };

        self.inner_pos += len as u64;

        Some((header, data))
    }
}

impl Tracer {
    fn set_state(&self, new_state: State) {
        tracing::trace!("setting tracing state: {:?}", new_state);
        let mut guard = self.state.lock().unwrap();
        guard.state = new_state;
        self.state_cv.notify_all();
    }

    fn wait_for(&self, target_state: State) {
        tracing::trace!("wait for tracing state: {:?}", target_state);
        let mut guard = self.state.lock().unwrap();
        while guard.state != target_state {
            guard = self.state_cv.wait(guard).unwrap();
        }
    }

    fn notify_exit(&self) {
        let wake = ThreadSyncWake::new(ThreadSyncReference::Virtual(&self.notifier), usize::MAX);
        self.notifier.store(1, Ordering::SeqCst);
        let _ = sys_thread_sync(&mut [ThreadSync::new_wake(wake)], None).inspect_err(|e| {
            tracing::warn!("failed to notify exit: {}", e);
        });
    }
}

fn collector(tracer: &Tracer) {
    tracer.set_state(State::Ready);
    loop {
        let mut guard = tracer.state.lock().unwrap();
        let Ok(waiter) = guard.collect().inspect_err(|e| {
            tracing::error!("failed to collect trace data: {}", e);
        }) else {
            continue;
        };

        if tracer.notifier.load(Ordering::SeqCst) == 0 {
            drop(guard);
            let mut waiters = [
                ThreadSync::new_sleep(waiter),
                ThreadSync::new_sleep(ThreadSyncSleep::new(
                    ThreadSyncReference::Virtual(&tracer.notifier),
                    0,
                    ThreadSyncOp::Equal,
                    ThreadSyncFlags::empty(),
                )),
            ];
            tracing::trace!(
                "collector is waiting for data: {} {}",
                waiters[0].ready(),
                waiters[1].ready()
            );
            if waiters.iter().all(|w| !w.ready()) {
                let _ = sys_thread_sync(&mut waiters, None).inspect_err(|e| {
                    tracing::warn!("failed to thread sync: {}", e);
                });
            }
        } else {
            tracing::trace!("collector was notified of exit");

            let _ = sys_ktrace(guard.objects.first().unwrap().id(), None).inspect_err(|e| {
                tracing::error!("failed to disable tracing: {}", e);
            });
            let _ = guard.collect().inspect_err(|e| {
                tracing::error!("failed to collect trace data: {}", e);
            });
            if guard.state == State::Done {
                break;
            }
            drop(tracer.state_cv.wait(guard).unwrap());
        }
    }
}

pub fn start(
    cli: &Cli,
    comp: CompartmentHandle,
    specs: Vec<TraceSpec>,
) -> miette::Result<TracingState> {
    let tracer = Tracer {
        state: Mutex::new(TracingState::new(comp.info().name, specs.as_slice())?),
        specs,
        state_cv: Condvar::new(),
        notifier: AtomicU64::new(0),
    };
    std::thread::scope(|scope| {
        let th_collector = Builder::new()
            .name("trace-collector".to_owned())
            .spawn_scoped(scope, || collector(&tracer))
            .into_diagnostic()?;

        tracer.wait_for(State::Ready);

        let start = Instant::now();
        for thread in comp.threads() {
            let id: ObjID = thread.repr_id;
            tracing::debug!("resuming compartment thread {}", id);
            sys_thread_change_state(id, ExecutionState::Running).into_diagnostic()?;
            if cli.prog.sample {
                tracing::debug!("setting per-thread sampling for {}", id);
                sys_thread_set_trace_events(id, PERTHREAD_TRACE_GEN_SAMPLE).into_diagnostic()?;
            }
        }
        tracer.set_state(State::Running);

        let mut flags = comp.info().flags;
        while !flags.contains(CompartmentFlags::EXITED) {
            flags = comp.wait(flags);
        }
        let end = Instant::now();
        tracing::debug!(
            "compartment exited after {:2.2}s",
            (end - start).as_secs_f32()
        );
        tracer.state.lock().unwrap().start_time = start;
        tracer.state.lock().unwrap().end_time = end;

        tracer.set_state(State::Done);
        tracer.notify_exit();

        th_collector.join().unwrap();

        std::io::Result::Ok(()).into_diagnostic()
    })?;
    tracer.state.into_inner().into_diagnostic()
}
