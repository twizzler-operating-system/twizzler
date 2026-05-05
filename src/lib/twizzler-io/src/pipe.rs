use std::{
    io::ErrorKind,
    sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};

use libc::Elf32_Addr;
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
    reader: AtomicU64,
    writer: AtomicU64,
}

impl Pipe {
    pub fn create_object(spec: ObjectCreate) -> std::io::Result<Self> {
        let obj = ObjectBuilder::new(spec).build(PipeBase::new())?;
        Ok(Self {
            pipe: obj,
            reader: AtomicU64::new(1),
            writer: AtomicU64::new(1),
        })
    }

    pub fn open_object(id: ObjID) -> std::io::Result<Self> {
        let obj =
            unsafe { Object::<PipeBase>::map_unchecked(id, MapFlags::READ | MapFlags::WRITE) }?;
        obj.base().readers.fetch_add(1, Ordering::SeqCst);
        obj.base().writers.fetch_add(1, Ordering::SeqCst);
        Ok(Self {
            pipe: obj,
            reader: AtomicU64::new(1),
            writer: AtomicU64::new(1),
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

    pub fn read_waitpoint(&self) -> ThreadSyncSleep {
        self.pipe.base().buffer.sync_for_pending_data()
    }

    pub fn write_waitpoint(&self) -> ThreadSyncSleep {
        self.pipe.base().buffer.sync_for_avail_space()
    }

    pub fn is_reader(&self) -> bool {
        self.reader.load(Ordering::SeqCst) > 0
    }

    pub fn is_writer(&self) -> bool {
        self.writer.load(Ordering::SeqCst) > 0
    }

    pub fn increment_reader(&self) {
        self.reader.fetch_add(1, Ordering::SeqCst);
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

    pub fn increment_writer(&self) {
        self.writer.fetch_add(1, Ordering::SeqCst);
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
        twizzler_abi::klog_println!(
            "Pipe::close_reader: reader={}, readers={}",
            self.reader.load(Ordering::SeqCst),
            self.readers()
        );
        let my_readers = self.reader.load(Ordering::SeqCst);
        if my_readers == 0 {
            return;
        }
        while self
            .reader
            .compare_exchange(
                my_readers,
                my_readers - 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_err()
        {
            std::hint::spin_loop();
            let my_readers = self.reader.load(Ordering::SeqCst);
            if my_readers == 0 {
                return;
            }
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
        twizzler_abi::klog_println!(
            "Pipe::close_writer: writer={}, writers={}",
            self.writer.load(Ordering::SeqCst),
            self.writers()
        );
        let my_writers = self.writer.load(Ordering::SeqCst);
        if my_writers == 0 {
            return;
        }
        while self
            .writer
            .compare_exchange(
                my_writers,
                my_writers - 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_err()
        {
            std::hint::spin_loop();
            let my_writers = self.writer.load(Ordering::SeqCst);
            if my_writers == 0 {
                return;
            }
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
}

impl Pipe {
    pub fn read(&self, buf: &mut [u8], nb: bool) -> std::io::Result<usize> {
        twizzler_abi::klog_println!(
            "Pipe::read: readers={}, writers={}",
            self.readers(),
            self.writers()
        );
        let count = self.pipe.base().buffer.read_bytes(buf)?;
        if count == 0 && buf.len() > 0 && self.writers() > 0 {
            if !nb {
                return Err(ErrorKind::WouldBlock.into());
            }
            self.do_sleep(self.pipe.base().buffer.sync_for_pending_data())?;
            return self.read(buf, nb);
        }
        twizzler_abi::klog_println!("Pipe::read: read {} bytes", count);
        Ok(count)
    }
}

impl Pipe {
    pub fn write(&self, buf: &[u8], nb: bool) -> std::io::Result<usize> {
        if self.readers() == 0 {
            twizzler_abi::klog_println!("Pipe::write: no readers, returning BrokenPipe");
            return Err(ErrorKind::BrokenPipe.into());
        }
        let count = self.pipe.base().buffer.write_bytes(buf)?;
        if count == 0 && buf.len() > 0 && self.readers() > 0 {
            if !nb {
                return Err(ErrorKind::WouldBlock.into());
            }
            self.do_sleep(self.pipe.base().buffer.sync_for_avail_space())?;
            return self.write(buf, nb);
        }
        twizzler_abi::klog_println!("Pipe::write: wrote {} bytes", count);
        Ok(count)
    }

    pub fn flush(&self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Clone for Pipe {
    fn clone(&self) -> Self {
        if self.is_reader() {
            self.pipe.base().readers.fetch_add(1, Ordering::SeqCst);
        }
        if self.is_writer() {
            self.pipe.base().writers.fetch_add(1, Ordering::SeqCst);
        }
        Self {
            pipe: self.pipe.clone(),
            reader: AtomicU64::new(if self.is_reader() { 1 } else { 0 }),
            writer: AtomicU64::new(if self.is_writer() { 1 } else { 0 }),
        }
    }
}

impl Drop for Pipe {
    fn drop(&mut self) {
        self.close_reader();
        self.close_writer();
    }
}
