use core::sync::atomic::{AtomicU64, Ordering};

use twizzler_queue_raw::{QueueEntry, RawQueue, RawQueueHdr, ReceiveFlags, SubmissionFlags};

use crate::{condvar::CondVar, spinlock::Spinlock};

struct Queue<T> {
    raw: RawQueue<T>,
    cv: CondVar,
    lock: Spinlock<()>,
}

impl<T: Copy> Queue<T> {
    unsafe fn new(hdr: *const RawQueueHdr, buf: *mut QueueEntry<T>) -> Self {
        Self {
            raw: RawQueue::new(hdr, buf),
            cv: CondVar::new(),
            lock: Spinlock::new(()),
        }
    }

    fn send(&self, item: T, info: u32) {
        self.raw
            .submit(
                QueueEntry::new(info, item),
                |word, val| {
                    let guard = self.lock.lock();
                    if word.load(Ordering::SeqCst) == val {
                        self.cv.wait(guard);
                    }
                },
                |_word| self.cv.signal(),
                SubmissionFlags::empty(),
            )
            .unwrap();
    }

    fn recv(&mut self) -> (u32, T) {
        let item = self
            .raw
            .receive(
                |word, val| {
                    let guard = self.lock.lock();
                    if word.load(Ordering::SeqCst) == val {
                        self.cv.wait(guard);
                    }
                },
                |_word| self.cv.signal(),
                ReceiveFlags::empty(),
            )
            .unwrap();
        (item.info(), item.item())
    }
}

pub struct QueueSender<T> {
    inner: Queue<T>,
}

impl<T: Copy> QueueSender<T> {
    fn send(&self, item: T, info: u32) {
        self.inner.send(item, info)
    }
}

pub struct QueueReceiver<T> {
    inner: Queue<T>,
}

impl<T: Copy> QueueReceiver<T> {
    fn recv(&mut self) -> (u32, T) {
        self.inner.recv()
    }
}
