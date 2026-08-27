use std::{
    collections::HashMap,
    io::ErrorKind,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
};

use twizzler_abi::syscall::ThreadSyncSleep;
use twizzler_io::packet::PacketObject;
use twizzler_queue::{Queue, ReceiveFlags, SubmissionFlags};

use crate::{PacketNum, PacketSet};

pub struct Pair<S: Copy, C: Copy> {
    buf: PacketObject,
    queue: Queue<S, C>,
    inner: Mutex<PairInner>,
    /// Set when `check_completions` actually reclaims packets.
    ///
    /// Freeing a slot is progress a blocked sender is waiting on, and it is invisible to smoltcp's
    /// `PollResult` -- which is what made the original missed-wakeup bug. Recording it here lets
    /// the poll loop report it as progress *without* notifying unconditionally, which livelocked:
    /// a notify on every poll woke a blocked sender, whose retry failed and which then woke the
    /// poll thread again, forever.
    progress: AtomicBool,
}

impl<S: Copy, C: Copy> Pair<S, C> {
    pub fn new(buf: PacketObject, queue: Queue<S, C>) -> Self {
        Self {
            buf,
            queue,
            inner: Mutex::new(PairInner::default()),
            progress: AtomicBool::new(false),
        }
    }

    pub fn rx_waiter(&self) -> ThreadSyncSleep {
        self.queue.setup_read_sub_sleep()
    }

    pub fn comp_waiters(&self) -> ThreadSyncSleep {
        self.queue.setup_read_com_sleep()
    }

    pub fn has_pending_msg(&self) -> bool {
        self.queue.has_pending_submission()
    }

    pub fn packet_object(&self) -> &PacketObject {
        &self.buf
    }
}

#[derive(Default)]
struct PairInner {
    borrowed_packets: HashMap<u32, PacketSet>,
    id_list: Vec<u32>,
    next_id: u32,
}

impl PairInner {
    pub fn next_id(&mut self) -> u32 {
        if let Some(id) = self.id_list.pop() {
            return id;
        }
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub fn release_id(&mut self, id: u32) {
        if self.next_id == id + 1 {
            self.next_id -= 1;
            return;
        }
        self.id_list.push(id);
    }

    pub fn register_set(&mut self, id: u32, set: PacketSet) {
        self.borrowed_packets.insert(id, set);
    }

    pub fn take_set(&mut self, id: u32) -> Option<PacketSet> {
        self.borrowed_packets.remove(&id)
    }
}

impl<S: Copy, C: Copy> Pair<S, C> {
    pub fn packet_size(&self) -> usize {
        self.buf.packet_size()
    }

    /// Slots in this endpoint's pool -- the count of network buffers behind it.
    pub fn nr_packets(&self) -> usize {
        self.buf.nr_packets()
    }

    pub fn set_packet_len(&self, id: PacketNum, len: usize) {
        self.buf.set_packet_len(id, len)
    }

    pub fn packet_len(&self, id: PacketNum) -> usize {
        self.buf.packet_len(id)
    }

    pub fn allocate_packet(&self) -> Option<PacketNum> {
        self.check_completions();
        let r = self.buf.allocate_packet();
        r
    }

    pub fn release_packet(&self, id: PacketNum) {
        self.buf.release_packet(id);
    }

    #[allow(dead_code)]
    pub fn packet_mem(&self, id: PacketNum) -> &[u8] {
        self.buf.packet_mem(id)
    }

    pub fn packet_mem_mut(&self, id: PacketNum) -> &mut [u8] {
        self.buf.packet_mem_mut(id)
    }

    #[allow(dead_code)]
    pub fn try_send_packets(
        &self,
        packets: &[PacketNum],
        f: impl FnOnce(PacketSet) -> S,
    ) -> std::io::Result<usize> {
        let (set, count) = PacketSet::from_slice(packets);
        let mut inner = self.inner.lock().unwrap();
        let id = inner.next_id();
        inner.register_set(id, set);
        drop(inner);
        let msg = f(set);
        let r = self
            .queue
            .submit(id, msg, SubmissionFlags::NON_BLOCK)
            .map_err(|_| ErrorKind::WouldBlock);
        if r.is_err() {
            let mut inner = self.inner.lock().unwrap();
            if let Some(set) = inner.take_set(id) {
                self.release_packets(set);
            }
            inner.release_id(id);
            r?;
        }
        Ok(count)
    }

    pub fn send_packets(
        &self,
        packets: &[PacketNum],
        f: impl FnOnce(PacketSet) -> S,
    ) -> std::io::Result<usize> {
        let (set, count) = PacketSet::from_slice(packets);
        let mut inner = self.inner.lock().unwrap();
        let id = inner.next_id();
        inner.register_set(id, set);
        drop(inner);
        let msg = f(set);
        let r = self
            .queue
            .submit(id, msg, SubmissionFlags::empty())
            .map_err(|_| ErrorKind::Other);
        if r.is_err() {
            let mut inner = self.inner.lock().unwrap();
            if let Some(set) = inner.take_set(id) {
                self.release_packets(set);
            }
            inner.release_id(id);
            r?;
        }
        Ok(count)
    }

    fn release_packets(&self, set: PacketSet) {
        for packet in set.into_iter() {
            self.release_packet(packet);
        }
    }

    pub fn check_completions(&self) {
        let mut inner = self.inner.lock().unwrap();
        while let Ok(comp) = self.queue.get_completion(ReceiveFlags::NON_BLOCK) {
            if let Some(set) = inner.take_set(comp.0) {
                self.release_packets(set);
            }
            inner.release_id(comp.0);
            self.progress.store(true, Ordering::Relaxed);
        }
    }

    /// Whether packets were reclaimed since the last call, clearing the flag.
    pub fn take_progress(&self) -> bool {
        self.progress.swap(false, Ordering::Relaxed)
    }

    pub fn recv_msg(&self) -> Option<(u32, S)> {
        self.queue.receive(ReceiveFlags::NON_BLOCK).ok()
    }

    pub fn complete(&self, id: u32, msg: C) {
        self.queue
            .complete(id, msg, SubmissionFlags::empty())
            .unwrap();
    }
}
