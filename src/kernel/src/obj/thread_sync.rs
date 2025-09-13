use alloc::collections::BTreeMap;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use heapless::index_map::FnvIndexMap;
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
    threads: FnvIndexMap<u64, ThreadRef, 16>,
}

impl SleepEntry {
    pub fn new(thread: ThreadRef) -> Self {
        let mut threads = FnvIndexMap::new();
        let _ = threads.insert(thread.id(), thread);
        Self { threads }
    }

    pub fn add_thread(&mut self, thread: ThreadRef) {
        if let Err((_, thread)) = self.threads.insert(thread.id(), thread) {
            log::warn!("overflowed thread sleep list");
            self.wake_n(2);
            return self.add_thread(thread);
        }
    }

    pub fn remove_thread(&mut self, id: u64) {
        self.threads.remove(&id);
    }

    pub fn wake_n(&mut self, max_count: usize) -> usize {
        let mut count = 0;
        for idx in 0..self.threads.capacity() {
            if count >= max_count {
                break;
            }
            if let Some((id, thread)) = self.threads.get_index(idx) {
                if thread.reset_sync_sleep() {
                    let nid = *id;
                    add_to_requeue(self.threads.remove(&nid).unwrap());
                    count += 1;
                }
            }
        }
        return count;
    }
}

impl Drop for SleepEntry {
    fn drop(&mut self) {
        for idx in 0..self.threads.capacity() {
            if let Some((_, thread)) = self.threads.get_index(idx) {
                if thread.reset_sync_sleep() {
                    add_to_requeue(thread.clone());
                }
            }
        }
        self.threads.clear();
    }
}

pub struct SleepInfo {
    some_words: FnvIndexMap<usize, SleepEntry, 16>,
    more_words: Option<BTreeMap<usize, SleepEntry>>,
}

impl SleepInfo {
    pub fn new() -> Self {
        SleepInfo {
            some_words: FnvIndexMap::new(),
            more_words: None,
        }
    }

    fn word(&mut self, offset: usize) -> Option<&mut SleepEntry> {
        if let Some(words) = self.more_words.as_mut() {
            words.get_mut(&offset)
        } else {
            self.some_words.get_mut(&offset)
        }
    }

    pub fn insert(&mut self, offset: usize, thread: ThreadRef) {
        if let Some(se) = self.word(offset) {
            se.add_thread(thread);
        } else {
            if let Some(words) = self.more_words.as_mut() {
                words.insert(offset, SleepEntry::new(thread));
            } else {
                match self.some_words.insert(offset, SleepEntry::new(thread)) {
                    Ok(_) => {}
                    Err((_, se)) => {
                        log::info!("overflowing sleep entries");
                        // Clear the old words, wake up all those threads.
                        self.some_words.clear();
                        let mw = self.more_words.get_or_insert(BTreeMap::new());
                        mw.insert(offset, se);
                    }
                }
            }
        }
    }

    pub fn remove(&mut self, offset: usize, thread_id: u64) {
        if let Some(se) = self.word(offset) {
            se.remove_thread(thread_id);
        }
    }

    pub fn wake_n(&mut self, offset: usize, max_count: usize) -> usize {
        if let Some(se) = self.word(offset) {
            se.wake_n(max_count)
        } else {
            0
        }
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
            let cur = vaddr.load(Ordering::SeqCst);
            if !op.check(cur, val, flags) {
                return false;
            }
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
        if let Some(vaddr) = vaddr {
            let cur = vaddr.load(Ordering::SeqCst);
            if !op.check(cur, val, flags) {
                return false;
            }
        }
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
