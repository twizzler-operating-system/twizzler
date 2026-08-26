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
    /// Monotonic "the read side may have become ready" counter: bumped when data is written and
    /// when the writer count moves. `write_events` is its mirror, for space and the reader count.
    ///
    /// One word per direction, and every readiness consumer sleeps on the one matching its
    /// `wait_kind`. Sleeping on the buffer's data word instead -- which is what poll, select and
    /// kqueue all used to inherit -- cannot see a writer closing, because that moves the writer
    /// count and leaves the data word alone. Nor is it fixable per consumer:
    /// `twz_rt_fd_waitpoint` returns a single word across the C ABI with nowhere to put a second,
    /// and that ABI is what every async reader goes through. A word each consumer sleeps on means
    /// no consumer has to enumerate which events matter, so the set cannot drift again.
    ///
    /// Split by direction rather than shared, because a reader draining the buffer has to wake
    /// *writers* -- and on a shared word it would also wake every other reader, with nothing for
    /// them to read. `PollState::wait` does not re-arm, so it would return 0 from a poll that was
    /// given an infinite timeout. Same hazard `Waiters::mark_waiter` documents on the socket side.
    read_events: AtomicU64,
    write_events: AtomicU64,
    buffer: VolatileBuffer<BUF_SZ>,
}

impl PipeBase {
    pub fn new() -> Self {
        Self {
            readers: AtomicU64::new(1),
            writers: AtomicU64::new(1),
            read_events: AtomicU64::new(0),
            write_events: AtomicU64::new(0),
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

    fn event_word(&self, write_side: bool) -> &AtomicU64 {
        let base = self.pipe.base();
        if write_side {
            &base.write_events
        } else {
            &base.read_events
        }
    }

    pub fn events(&self, write_side: bool) -> u64 {
        self.event_word(write_side).load(Ordering::SeqCst)
    }

    /// Sleep until this side's event counter moves off the value the caller sampled. Sample it
    /// *before* testing readiness: a change landing in between moves the word, and the kernel
    /// declines a sleep whose armed value is already stale rather than losing the wakeup.
    pub fn events_waitpoint(&self, write_side: bool, events: u64) -> ThreadSyncSleep {
        ThreadSyncSleep::new(
            ThreadSyncReference::Virtual(self.event_word(write_side)),
            events,
            ThreadSyncOp::Equal,
            ThreadSyncFlags::empty(),
        )
    }

    /// Bump the counter for the side this change could have made ready, and wake it.
    fn bump_events(&self, write_side: bool) {
        let word = self.event_word(write_side);
        word.fetch_add(1, Ordering::SeqCst);
        let _ = sys_thread_sync(
            &mut [ThreadSync::new_wake(ThreadSyncWake::new(
                ThreadSyncReference::Virtual(word),
                usize::MAX,
            ))],
            None,
        )
        .inspect_err(|e| tracing::warn!("failed to wake on events: {e}"));
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
        self.bump_events(true);
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
        self.bump_events(false);
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
        self.bump_events(true);
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
        self.bump_events(false);
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
        if count > 0 {
            self.bump_events(true);
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
        if count > 0 {
            self.bump_events(false);
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
