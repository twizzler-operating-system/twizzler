use alloc::vec::Vec;
use core::{sync::atomic::AtomicU64, usize};

use twizzler_abi::{
    object::{ObjID, Protections, MAX_SIZE, NULLPAGE_SIZE},
    syscall::{BackingType, LifetimeType, ObjectCreate, ObjectCreateFlags, TraceSpec},
    trace::{TraceBase, TraceEntryFlags, TraceEntryHead},
};
use twizzler_rt_abi::error::{ObjectError, TwzError};

use super::buffered_trace_data::BufferedTraceData;
use crate::{
    obj::{lookup_object, LookupFlags, ObjectRef},
    syscall::object::sys_object_create,
};

pub struct TraceSink {
    prime_object: ObjectRef,
    current_object: ObjectRef,
    offset: u64,
    spec: TraceSpec,
    buffer: Vec<(TraceEntryHead, BufferedTraceData)>,
}

const TRACE_DATA_START: u64 = NULLPAGE_SIZE as u64 * 2;
impl TraceSink {
    pub fn new(id: ObjID, spec: TraceSpec) -> Result<Self, TwzError> {
        let obj = lookup_object(id, LookupFlags::empty()).ok_or(ObjectError::NoSuchObject)?;
        obj.write_base(&TraceBase {
            start: TRACE_DATA_START,
            end: AtomicU64::new(TRACE_DATA_START),
        });
        Ok(Self {
            prime_object: obj.clone(),
            current_object: obj,
            offset: TRACE_DATA_START,
            spec,
            buffer: Vec::new(),
        })
    }

    pub fn modify(&mut self, spec: TraceSpec) {
        self.spec = spec;
    }

    pub fn accepts(&self, event: &TraceEntryHead) -> bool {
        self.spec.accepts(event)
    }

    pub fn enqueue(&mut self, entry: (TraceEntryHead, BufferedTraceData)) {
        self.buffer.push(entry);
    }

    fn write(&mut self, entry: (TraceEntryHead, BufferedTraceData)) {
        self.current_object.write_at(&entry.0, self.offset as usize);
        self.offset += size_of::<TraceEntryHead>() as u64;
        if entry.0.flags.contains(TraceEntryFlags::HAS_DATA) {
            self.current_object
                .write_bytes(entry.1.ptr(), entry.1.len(), self.offset as usize);
            self.offset += entry.1.len() as u64;
        }
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

            let obj = lookup_object(id, LookupFlags::empty()).unwrap();
            obj.write_base(&TraceBase {
                start: TRACE_DATA_START,
                end: AtomicU64::new(TRACE_DATA_START),
            });

            self.write((
                TraceEntryHead::new_next_object(id),
                BufferedTraceData::default(),
            ));

            self.current_object = obj;
            self.offset = NULLPAGE_SIZE as u64 * 2;
        }
        true
    }

    pub fn write_all(&mut self) {
        for i in 0..self.buffer.len() {
            if !self.check_space() {
                // TODO: this could lead to duplicates
                return;
            }
            self.write(self.buffer[i]);
        }
        unsafe {
            self.current_object
                .try_write_val_and_signal(NULLPAGE_SIZE, self.offset, usize::MAX)
        };
    }

    pub fn spec(&self) -> &TraceSpec {
        &self.spec
    }
}
