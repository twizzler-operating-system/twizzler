use std::{
    io::{ErrorKind, Read, Write},
    sync::atomic::{AtomicU64, Ordering},
};

use twizzler::{
    BaseType, Invariant,
    object::{MapFlags, ObjID, Object, ObjectBuilder, TypedObject},
};
use twizzler_abi::syscall::{
    ObjectCreate, ThreadSync, ThreadSyncFlags, ThreadSyncOp, ThreadSyncReference, ThreadSyncSleep,
    sys_thread_sync,
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
    pipe: Object<PipeBase>,
    reader: bool,
    writer: bool,
}

impl Pipe {
    pub fn create_object(spec: ObjectCreate) -> std::io::Result<Self> {
        let obj = ObjectBuilder::new(spec).build(PipeBase::new())?;
        Ok(Self {
            pipe: obj,
            reader: true,
            writer: true,
        })
    }

    pub fn open_object(id: ObjID) -> std::io::Result<Self> {
        let obj =
            unsafe { Object::<PipeBase>::map_unchecked(id, MapFlags::READ | MapFlags::WRITE) }?;
        obj.base().readers.fetch_add(1, Ordering::SeqCst);
        obj.base().writers.fetch_add(1, Ordering::SeqCst);
        Ok(Self {
            pipe: obj,
            reader: true,
            writer: true,
        })
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

    pub fn close_reader(&mut self) {
        self.reader = false;
        if self.readers() == 0 {
            return;
        }
        self.pipe.base().readers.fetch_sub(1, Ordering::SeqCst);
    }

    pub fn close_writer(&mut self) {
        self.writer = false;
        if self.writers() == 0 {
            return;
        }
        self.pipe.base().writers.fetch_sub(1, Ordering::SeqCst);
    }

    fn do_sleep(&self, sync: ThreadSyncSleep) -> std::io::Result<()> {
        let readers = self.readers();
        let reader_sync = ThreadSync::new_sleep(ThreadSyncSleep::new(
            ThreadSyncReference::Virtual(&self.pipe.base().readers),
            readers,
            ThreadSyncOp::Equal,
            ThreadSyncFlags::empty(),
        ));
        let writer_sync = ThreadSync::new_sleep(ThreadSyncSleep::new(
            ThreadSyncReference::Virtual(&self.pipe.base().readers),
            readers,
            ThreadSyncOp::Equal,
            ThreadSyncFlags::empty(),
        ));
        sys_thread_sync(
            &mut [ThreadSync::new_sleep(sync), reader_sync, writer_sync],
            None,
        )?;
        Ok(())
    }
}

impl Read for Pipe {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let count = self.pipe.base().buffer.read_bytes(buf)?;
        if count == 0 && buf.len() > 0 && self.writers() > 0 {
            self.do_sleep(self.pipe.base().buffer.sync_for_pending_data())?;
            return self.read(buf);
        }
        Ok(count)
    }
}

impl Write for Pipe {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.readers() == 0 {
            return Err(ErrorKind::BrokenPipe.into());
        }
        let count = self.pipe.base().buffer.write_bytes(buf)?;
        if count == 0 && buf.len() > 0 && self.readers() > 0 {
            self.do_sleep(self.pipe.base().buffer.sync_for_avail_space())?;
            return self.write(buf);
        }
        Ok(count)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Clone for Pipe {
    fn clone(&self) -> Self {
        if self.reader {
            self.pipe.base().readers.fetch_add(1, Ordering::SeqCst);
        }
        if self.writer {
            self.pipe.base().writers.fetch_add(1, Ordering::SeqCst);
        }
        Self {
            pipe: self.pipe.clone(),
            reader: self.reader,
            writer: self.writer,
        }
    }
}

impl Drop for Pipe {
    fn drop(&mut self) {
        if self.reader {
            self.close_reader();
        }
        if self.writer {
            self.close_writer();
        }
    }
}
