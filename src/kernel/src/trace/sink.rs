use alloc::vec::Vec;
use core::{ptr::addr_of, sync::atomic::AtomicU64, usize};

use twizzler_abi::{
    object::{MAX_SIZE, NULLPAGE_SIZE, ObjID, Protections},
    syscall::{BackingType, LifetimeType, ObjectCreate, ObjectCreateFlags, TraceSpec},
    trace::{TraceBase, TraceData, TraceEntryFlags, TraceEntryHead},
};
use twizzler_rt_abi::error::{ObjectError, TwzError};

use super::{buffered_trace_data::BufferedTraceData, mgr::signalstats};
use crate::{
    obj::{LookupFlags, ObjectRef, lookup_object},
    syscall::object::sys_object_create,
};

pub struct TraceSink {
    prime_object: ObjectRef,
    current_object: ObjectRef,
    offset: u64,
    specs: Vec<TraceSpec>,
    buffer: Vec<(TraceEntryHead, BufferedTraceData)>,
    /// Entries written into the object but not yet announced. See [`Self::SIGNAL_WATERMARK`].
    unsignalled: usize,
}

/// Object-write calls the sink makes per record, so the collapse from three to one is checked
/// rather than assumed. Wall clock cannot see it: the writer runs on cpus a `-j1` build leaves
/// idle, so traced and untraced builds both land at ~31s regardless.
pub mod resolvestats {
    use core::sync::atomic::{AtomicU64, Ordering::Relaxed};

    static RECORDS: AtomicU64 = AtomicU64::new(0);
    static WRITES: AtomicU64 = AtomicU64::new(0);

    pub fn record(writes: u64) {
        RECORDS.fetch_add(1, Relaxed);
        WRITES.fetch_add(writes, Relaxed);
    }

    pub fn print() {
        let records = RECORDS.load(Relaxed);
        if records == 0 {
            return;
        }
        logln!(
            "== trace sink writes: {} records, {} object writes ({:.2} per record) ==",
            records,
            WRITES.load(Relaxed),
            WRITES.load(Relaxed) as f64 / records as f64,
        );
    }
}

const TRACE_DATA_START: u64 = NULLPAGE_SIZE as u64 * 2;
impl TraceSink {
    pub fn new(id: ObjID, specs: Vec<TraceSpec>) -> Result<Self, TwzError> {
        let obj = lookup_object(id, LookupFlags::empty()).ok_or(ObjectError::NoSuchObject)?;
        obj.write_base(&TraceBase {
            start: TRACE_DATA_START,
            end: AtomicU64::new(TRACE_DATA_START),
        })
        .unwrap();
        Ok(Self {
            prime_object: obj.clone(),
            current_object: obj,
            offset: TRACE_DATA_START,
            specs,
            buffer: Vec::new(),
            unsignalled: 0,
        })
    }

    pub fn pending(&self) -> usize {
        self.buffer.len()
    }

    pub fn modify(&mut self, spec: TraceSpec) {
        self.specs.push(spec);
    }

    pub fn accepts(&self, event: &TraceEntryHead) -> bool {
        self.specs.iter().any(|s| s.accepts(event))
    }

    pub fn enqueue(&mut self, entry: (TraceEntryHead, BufferedTraceData)) {
        self.buffer.push(entry);
    }

    /// Append one record, in a single object write.
    ///
    /// The three pieces used to be three `write_bytes` calls at consecutive offsets, and each one
    /// resolved the frame from scratch -- page-table lock, `ensure_in_core`, page-table walk. See
    /// [`Object::write_pieces`] for the measurement that motivated collapsing them.
    fn write(&self, entry: &(TraceEntryHead, BufferedTraceData)) -> u64 {
        let entry_head_len = size_of::<TraceEntryHead>();
        // SAFETY: a `TraceEntryHead` is `repr(C)` and plain data; this is the same byte image
        // `write_at` would have produced for it.
        let head_bytes =
            unsafe { core::slice::from_raw_parts(addr_of!(entry.0).cast::<u8>(), entry_head_len) };
        if !entry.0.flags.contains(TraceEntryFlags::HAS_DATA) {
            resolvestats::record(1);
            let _ = self
                .current_object
                .write_pieces(&[head_bytes], self.offset as usize);
            return entry_head_len as u64;
        }
        let header_len = size_of::<TraceData<()>>();
        let len = entry.1.len() + header_len;
        let trace_data_header = TraceData::<()> {
            len: len.next_multiple_of(align_of::<TraceEntryHead>().max(32)) as u32,
            flags: 0,
            data: (),
            resv: 0,
        };
        log::trace!(
            "write: {:x} {} {} {} {}",
            self.offset,
            entry_head_len,
            len,
            entry.1.len(),
            trace_data_header.len,
        );
        // SAFETY: both are plain-data reads of live locals/buffers for the duration of the call.
        let hdr_bytes = unsafe {
            core::slice::from_raw_parts(addr_of!(trace_data_header).cast::<u8>(), header_len)
        };
        let data_bytes = unsafe { core::slice::from_raw_parts(entry.1.ptr(), entry.1.len()) };
        resolvestats::record(1);
        let _ = self
            .current_object
            .write_pieces(&[head_bytes, hdr_bytes, data_bytes], self.offset as usize);
        entry_head_len as u64 + trace_data_header.len as u64
    }

    fn check_space(&mut self) -> bool {
        if self.offset > (MAX_SIZE as u64 / 2) {
            let Ok(id) = sys_object_create(
                &ObjectCreate::new(
                    BackingType::Normal,
                    LifetimeType::Volatile,
                    None,
                    ObjectCreateFlags::empty(),
                    Protections::READ,
                ),
                &[],
                &[],
            ) else {
                log::warn!("failed to allocate new tracing data object");
                return false;
            };
            log::debug!("allocating new object for tracing data: {}", id);

            let obj = lookup_object(id, LookupFlags::empty()).unwrap();
            obj.write_base(&TraceBase {
                start: TRACE_DATA_START,
                end: AtomicU64::new(TRACE_DATA_START),
            })
            .unwrap();

            self.offset += self.write(&(
                TraceEntryHead::new_next_object(id),
                BufferedTraceData::default(),
            ));

            // Announced unconditionally: the consumer follows the object chain through this
            // entry, so deferring it would leave it waiting on an object nothing more is written
            // to.
            self.unsignalled = 0;
            let _ = self.current_object.try_write_val_and_signal(
                NULLPAGE_SIZE,
                self.offset,
                usize::MAX,
            );

            self.current_object = obj;
            self.offset = TRACE_DATA_START;
        }
        true
    }

    /// Entries written but not yet announced to the consumer before it is worth a wake.
    ///
    /// The consumer sleeps on the sink's end offset, so announcing every batch woke it once per
    /// writer pass -- and the writer runs a pass per enqueue. Measured over one guest build:
    /// 21,211 events produced 17,703 collector wakes, 0.83 wakes per event, each a
    /// `sys_thread_sync` round trip plus a collect pass. That was the tracer's largest remaining
    /// cost and the reason `sys_thread_sync` was its hottest pc.
    ///
    /// **It barely binds, and the measurement says why.** With it in place: 15,095 announcements
    /// for 19,325 events, collector wakes 17,703 -> 15,070 (-15%), zero events dropped. The writer
    /// drains one or two events per pass, so `did_work` goes false immediately and the
    /// about-to-idle flush announces the batch before 64 ever accumulate. Batching here cannot
    /// help while the thing feeding it is itself woken per event -- the same reason coalescing the
    /// writer's own wake elided only 17%. The fix that would work is to stop waking the *writer*
    /// per enqueue (a watermark on the async buffer, with a timed backstop for the tail); this is
    /// kept because it is free and correct, not because it solved it.
    const SIGNAL_WATERMARK: usize = 64;

    /// Write buffered entries into the sink object without announcing them.
    ///
    /// Returns whether anything was written. The announcement is [`Self::signal`], which the writer
    /// thread runs on the watermark or when it is about to sleep -- so a batch either has more work
    /// coming (and will be announced by a later pass) or ends with an announcement. Nothing can be
    /// stranded.
    pub fn write_all(&mut self) -> bool {
        let old_offset = self.offset;
        for i in 0..self.buffer.len() {
            if !self.check_space() {
                // TODO: this could lead to duplicates
                return false;
            }
            self.offset += self.write(&self.buffer[i]);
        }
        if !self.buffer.is_empty() {
            log::debug!(
                "trace sink write_all: {} entries ({})",
                self.buffer.len(),
                self.offset - old_offset
            );
            self.unsignalled += self.buffer.len();
            self.buffer.clear();
            if self.unsignalled >= Self::SIGNAL_WATERMARK {
                self.signal();
            }
            true
        } else {
            false
        }
    }

    /// Announce everything written so far, if anything is unannounced.
    pub fn signal(&mut self) {
        if self.unsignalled == 0 {
            return;
        }
        self.unsignalled = 0;
        signalstats::sink_signal();
        let _ =
            self.current_object
                .try_write_val_and_signal(NULLPAGE_SIZE, self.offset, usize::MAX);
    }

    pub fn specs(&self) -> &[TraceSpec] {
        &self.specs
    }
}
