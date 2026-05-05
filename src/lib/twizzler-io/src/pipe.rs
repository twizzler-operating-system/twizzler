use std::{
    io::ErrorKind,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

use twizzler::{
    BaseType, Invariant,
    object::{MapFlags, ObjID, Object, ObjectBuilder, TypedObject},
};
use twizzler_abi::syscall::{
    ObjectCreate, ThreadSync, ThreadSyncFlags, ThreadSyncOp, ThreadSyncReference, ThreadSyncSleep,
    ThreadSyncWake, sys_thread_sync,
};

use crate::buffer::VolatileBuffer;

pub const BUF_SZ: usize = 4096;

#[derive(Invariant, BaseType)]
pub struct PipeBase {
    readers: AtomicU64,
    writers: AtomicU64,
    buffer: VolatileBuffer<BUF_SZ>,
}

impl PipeBase {
    pub fn new() -> Self {
        Self {
            readers: AtomicU64::new(1),
            writers: AtomicU64::new(1),
            buffer: VolatileBuffer::new(),
        }
    }
}

pub struct Pipe {
    pub pipe: Object<PipeBase>,
    reader: AtomicBool,
    writer: AtomicBool,
}

impl Pipe {
    pub fn create_object(spec: ObjectCreate) -> std::io::Result<Self> {
        let obj = ObjectBuilder::new(spec).build(PipeBase::new())?;
        Ok(Self {
            pipe: obj,
            reader: AtomicBool::new(true),
            writer: AtomicBool::new(true),
        })
    }

    pub fn open_object(id: ObjID) -> std::io::Result<Self> {
        let obj =
            unsafe { Object::<PipeBase>::map_unchecked(id, MapFlags::READ | MapFlags::WRITE) }?;
        let this = Self {
            pipe: obj,
            reader: AtomicBool::new(true),
            writer: AtomicBool::new(true),
        };
        this.increment_reader();
        this.increment_writer();
        Ok(this)
    }

    pub fn id(&self) -> ObjID {
        self.pipe.id()
    }

    pub fn readers(&self) -> u64 {
        self.pipe.base().readers.load(Ordering::SeqCst)
    }

    pub fn writers(&self) -> u64 {
        self.pipe.base().writers.load(Ordering::SeqCst)
    }

    pub fn read_waitpoint(&self) -> ThreadSyncSleep {
        self.pipe.base().buffer.sync_for_pending_data()
    }

    pub fn write_waitpoint(&self) -> ThreadSyncSleep {
        self.pipe.base().buffer.sync_for_avail_space()
    }

    pub fn is_reader(&self) -> bool {
        self.reader.load(Ordering::SeqCst)
    }

    pub fn is_writer(&self) -> bool {
        self.writer.load(Ordering::SeqCst)
    }

    pub fn enable_reader(&self) {
        if !self.reader.swap(true, Ordering::SeqCst) {
            self.increment_reader();
        }
    }

    pub fn increment_reader(&self) {
        self.pipe.base().readers.fetch_add(1, Ordering::SeqCst);
        let _ = sys_thread_sync(
            &mut [ThreadSync::new_wake(ThreadSyncWake::new(
                ThreadSyncReference::Virtual(&self.pipe.base().readers),
                usize::MAX,
            ))],
            None,
        )
        .inspect_err(|e| tracing::warn!("failed to wake on readers: {e}"));
    }

    pub fn enable_writer(&self) {
        if !self.writer.swap(true, Ordering::SeqCst) {
            self.increment_writer();
        }
    }

    pub fn increment_writer(&self) {
        self.pipe.base().writers.fetch_add(1, Ordering::SeqCst);
        let _ = sys_thread_sync(
            &mut [ThreadSync::new_wake(ThreadSyncWake::new(
                ThreadSyncReference::Virtual(&self.pipe.base().writers),
                usize::MAX,
            ))],
            None,
        )
        .inspect_err(|e| tracing::warn!("failed to wake on writers: {e}"));
    }

    pub fn close_reader(&self) {
        if !self.reader.swap(false, Ordering::SeqCst) {
            return;
        }
        if self.readers() == 0 {
            return;
        }

        self.pipe.base().readers.fetch_sub(1, Ordering::SeqCst);

        let _ = sys_thread_sync(
            &mut [ThreadSync::new_wake(ThreadSyncWake::new(
                ThreadSyncReference::Virtual(&self.pipe.base().readers),
                usize::MAX,
            ))],
            None,
        )
        .inspect_err(|e| tracing::warn!("failed to wake on readers: {e}"));
    }

    pub fn close_writer(&self) {
        if !self.writer.swap(false, Ordering::SeqCst) {
            return;
        }
        if self.writers() == 0 {
            return;
        }
        self.pipe.base().writers.fetch_sub(1, Ordering::SeqCst);

        let _ = sys_thread_sync(
            &mut [ThreadSync::new_wake(ThreadSyncWake::new(
                ThreadSyncReference::Virtual(&self.pipe.base().writers),
                usize::MAX,
            ))],
            None,
        )
        .inspect_err(|e| tracing::warn!("failed to wake on writers: {e}"));
    }

    fn do_sleep(&self, sync: ThreadSyncSleep) -> std::io::Result<()> {
        let readers = self.readers();
        let reader_sync = ThreadSync::new_sleep(ThreadSyncSleep::new(
            ThreadSyncReference::Virtual(&self.pipe.base().readers),
            readers,
            ThreadSyncOp::Equal,
            ThreadSyncFlags::empty(),
        ));
        let writers = self.writers();
        let writer_sync = ThreadSync::new_sleep(ThreadSyncSleep::new(
            ThreadSyncReference::Virtual(&self.pipe.base().writers),
            writers,
            ThreadSyncOp::Equal,
            ThreadSyncFlags::empty(),
        ));
        sys_thread_sync(
            &mut [ThreadSync::new_sleep(sync), reader_sync, writer_sync],
            None,
        )?;
        Ok(())
    }

    pub fn has_pending_data(&self) -> bool {
        !self.pipe.base().buffer.is_empty()
    }

    pub fn has_avail_space(&self) -> bool {
        self.pipe.base().buffer.avail_space() > 0
    }
}

impl Pipe {
    pub fn read(&self, buf: &mut [u8], nb: bool) -> std::io::Result<usize> {
        let writers = self.writers();
        let sync = self.pipe.base().buffer.sync_for_pending_data();
        let count = self.pipe.base().buffer.read_bytes(buf)?;
        if count == 0 && buf.len() > 0 && writers > 0 {
            if nb {
                return Err(ErrorKind::WouldBlock.into());
            }
            self.do_sleep(sync)?;
            return self.read(buf, nb);
        }
        Ok(count)
    }
}

impl Pipe {
    pub fn write(&self, buf: &[u8], nb: bool) -> std::io::Result<usize> {
        let readers = self.readers();
        let sync = self.pipe.base().buffer.sync_for_avail_space();
        if readers == 0 {
            return Err(ErrorKind::BrokenPipe.into());
        }
        let count = self.pipe.base().buffer.write_bytes(buf)?;
        if count == 0 && buf.len() > 0 && readers > 0 {
            if nb {
                return Err(ErrorKind::WouldBlock.into());
            }
            self.do_sleep(sync)?;
            return self.write(buf, nb);
        }
        Ok(count)
    }

    pub fn flush(&self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Clone for Pipe {
    fn clone(&self) -> Self {
        let reader = self.reader.load(Ordering::SeqCst);
        let writer = self.writer.load(Ordering::SeqCst);
        if reader {
            self.increment_reader();
        }
        if writer {
            self.increment_writer();
        }
        Self {
            pipe: self.pipe.clone(),
            reader: AtomicBool::new(reader),
            writer: AtomicBool::new(writer),
        }
    }
}

impl Drop for Pipe {
    fn drop(&mut self) {
        if self.reader.load(Ordering::SeqCst) {
            self.close_reader();
        }
        if self.writer.load(Ordering::SeqCst) {
            self.close_writer();
        }
    }
}
