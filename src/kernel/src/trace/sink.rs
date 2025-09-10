use alloc::vec::Vec;
use core::{ptr::addr_of, sync::atomic::AtomicU64, usize};

use twizzler_abi::{
    object::{ObjID, Protections, MAX_SIZE, NULLPAGE_SIZE},
    syscall::{BackingType, LifetimeType, ObjectCreate, ObjectCreateFlags, TraceSpec},
    trace::{TraceBase, TraceData, TraceEntryFlags, TraceEntryHead},
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
    specs: Vec<TraceSpec>,
    buffer: Vec<(TraceEntryHead, BufferedTraceData)>,
}

const TRACE_DATA_START: u64 = NULLPAGE_SIZE as u64 * 2;
impl TraceSink {
    pub fn new(id: ObjID, specs: Vec<TraceSpec>) -> Result<Self, TwzError> {
        let obj = lookup_object(id, LookupFlags::empty()).ok_or(ObjectError::NoSuchObject)?;
        obj.write_base(&TraceBase {
            start: TRACE_DATA_START,
            end: AtomicU64::new(TRACE_DATA_START),
        });
        Ok(Self {
            prime_object: obj.clone(),
            current_object: obj,
            offset: TRACE_DATA_START,
            specs,
            buffer: Vec::new(),
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

    fn write(&self, entry: &(TraceEntryHead, BufferedTraceData)) -> u64 {
        self.current_object.write_at(&entry.0, self.offset as usize);
        let entry_head_len = size_of::<TraceEntryHead>();
        if entry.0.flags.contains(TraceEntryFlags::HAS_DATA) {
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
            let header_ptr = addr_of!(trace_data_header);
            self.current_object.write_bytes(
                header_ptr.cast(),
                header_len,
                self.offset as usize + entry_head_len,
            );
            self.current_object.write_bytes(
                entry.1.ptr(),
                entry.1.len(),
                self.offset as usize + header_len + entry_head_len,
            );
            entry_head_len as u64 + trace_data_header.len as u64
        } else {
            entry_head_len as u64
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
            log::debug!("allocating new object for tracing data: {}", id);

            let obj = lookup_object(id, LookupFlags::empty()).unwrap();
            obj.write_base(&TraceBase {
                start: TRACE_DATA_START,
                end: AtomicU64::new(TRACE_DATA_START),
            });

            self.offset += self.write(&(
                TraceEntryHead::new_next_object(id),
                BufferedTraceData::default(),
            ));

            unsafe {
                self.current_object
                    .try_write_val_and_signal(NULLPAGE_SIZE, self.offset, usize::MAX)
            }

            self.current_object = obj;
            self.offset = TRACE_DATA_START;
        }
        true
    }

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
            unsafe {
                self.current_object
                    .try_write_val_and_signal(NULLPAGE_SIZE, self.offset, usize::MAX)
            };
            self.buffer.clear();
            true
        } else {
            false
        }
    }

    pub fn specs(&self) -> &[TraceSpec] {
        &self.specs
    }
}
