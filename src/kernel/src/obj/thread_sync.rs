use alloc::collections::BTreeMap;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use twizzler_abi::{
    device::NUM_DEVICE_INTERRUPTS,
    syscall::{ThreadSyncFlags, ThreadSyncOp},
};

use super::{Object, OBJ_HAS_INTERRUPTS};
use crate::{
    interrupt::wait_for_device_interrupt,
    syscall::sync::add_to_requeue,
    thread::{current_thread_ref, ThreadRef},
};

struct SleepEntry {
    threads: BTreeMap<u64, ThreadRef>,
}

impl Drop for SleepEntry {
    fn drop(&mut self) {
        while let Some(t) = self.threads.pop_first() {
            if t.1.reset_sync_sleep() {
                add_to_requeue(t.1);
            }
        }
    }
}

pub struct SleepInfo {
    words: BTreeMap<usize, SleepEntry>,
}

impl SleepEntry {
    pub fn new(thread: ThreadRef) -> Self {
        let mut map = BTreeMap::new();
        map.insert(thread.id(), thread);
        Self { threads: map }
    }
}

impl SleepInfo {
    pub fn new() -> Self {
        SleepInfo {
            words: BTreeMap::new(),
        }
    }

    pub fn insert(&mut self, offset: usize, thread: ThreadRef) {
        if let Some(se) = self.words.get_mut(&offset) {
            se.threads.insert(thread.id(), thread);
            //   logln!("inserted {}", se.threads.len());
        } else {
            self.words.insert(offset, SleepEntry::new(thread));
            // logln!("inserted 1");
        }
    }

    pub fn remove(&mut self, offset: usize, thread_id: u64) {
        if let Some(se) = self.words.get_mut(&offset) {
            se.threads.remove(&thread_id);
        }
    }

    pub fn wake_n(&mut self, offset: usize, max_count: usize) -> usize {
        let mut count = 0;
        if let Some(se) = self.words.get_mut(&offset) {
            //logln!("wake up {}/{} threads", max_count, se.threads.len());
            if max_count == 1 {
                /* This is fairly common, so we can have a fast path */
                let mut remove = None;
                for (id, t) in &se.threads {
                    if t.reset_sync_sleep() {
                        remove = Some(*id);
                        break;
                    }
                }
                if let Some(ref id) = remove {
                    crate::syscall::sync::add_to_requeue(se.threads.remove(id).unwrap());
                    return 1;
                }
                return 0;
            }
            for (_, t) in se.threads.extract_if(|_, v| {
                let p = count < max_count && v.reset_sync_sleep();
                if p {
                    count += 1;
                }
                p
            }) {
                /* TODO (opt): if sync_sleep_done is also set, maybe we can just immeditately
                 * reschedule this thread. */
                crate::syscall::sync::add_to_requeue(t);
            }
        }
        count
    }
}

impl Object {
    pub fn wakeup_word(&self, offset: usize, count: usize) -> usize {
        let mut sleep_info = self.sleep_info.lock();
        sleep_info.wake_n(offset, count)
    }

    pub fn add_device_interrupt(&self, vector: u32, num: usize, offset: usize) {
        self.device_interrupt_info[num]
            .0
            .store(vector as u64, Ordering::Release);
        self.device_interrupt_info[num]
            .1
            .store(offset as u64, Ordering::Release);
        self.flags.fetch_or(OBJ_HAS_INTERRUPTS, Ordering::Release);
    }

    pub fn setup_sleep_word(
        &self,
        offset: usize,
        op: ThreadSyncOp,
        val: u64,
        first_sleep: bool,
        flags: ThreadSyncFlags,
        vaddr: Option<&AtomicU64>,
    ) -> bool {
        let thread = current_thread_ref().unwrap();

        if let Some(vaddr) = vaddr {
            if self.flags.load(Ordering::Acquire) & OBJ_HAS_INTERRUPTS != 0 {
                for i in 0..NUM_DEVICE_INTERRUPTS {
                    let di_offset = self.device_interrupt_info[i].1.load(Ordering::Acquire);
                    let di_vector = self.device_interrupt_info[i].0.load(Ordering::Acquire);
                    if di_offset as usize == offset {
                        return wait_for_device_interrupt(
                            thread,
                            di_vector as u32,
                            first_sleep,
                            vaddr,
                        );
                    }
                }
            }
        }

        let mut sleep_info = self.sleep_info.lock();

        let cur = vaddr
            .map(|vaddr| vaddr.load(Ordering::SeqCst))
            .unwrap_or_else(|| unsafe { self.read_atomic_u64(offset) });
        let res = op.check(cur, val, flags);
        if res {
            if first_sleep {
                thread.set_sync_sleep();
            }
            sleep_info.insert(offset, thread.clone());
        }
        res
    }

    pub fn setup_sleep_word32(
        &self,
        offset: usize,
        op: ThreadSyncOp,
        val: u32,
        first_sleep: bool,
        flags: ThreadSyncFlags,
        vaddr: Option<&AtomicU32>,
    ) -> bool {
        let thread = current_thread_ref().unwrap();
        let mut sleep_info = self.sleep_info.lock();

        let cur = vaddr
            .map(|vaddr| vaddr.load(Ordering::SeqCst))
            .unwrap_or_else(|| unsafe { self.read_atomic_u32(offset) });
        let res = op.check(cur, val, flags);
        if res {
            if first_sleep {
                thread.set_sync_sleep();
            }
            sleep_info.insert(offset, thread.clone());
        }
        res
    }

    pub fn remove_from_sleep_word(&self, offset: usize) {
        let thread = current_thread_ref().unwrap();
        let mut sleep_info = self.sleep_info.lock();
        sleep_info.remove(offset, thread.id());
    }
}
