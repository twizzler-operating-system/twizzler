use alloc::collections::BTreeMap;

use twizzler_abi::syscall::{ThreadSyncFlags, ThreadSyncOp};

use super::Object;
use crate::thread::{current_thread_ref, ThreadRef};

struct SleepEntry {
    threads: BTreeMap<u64, ThreadRef>,
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
                count += 1;
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

    pub fn setup_sleep_word(
        &self,
        offset: usize,
        op: ThreadSyncOp,
        val: u64,
        first_sleep: bool,
        flags: ThreadSyncFlags,
    ) -> bool {
        let thread = current_thread_ref().unwrap();
        let mut sleep_info = self.sleep_info.lock();

        let cur = unsafe { self.read_atomic_u64(offset) };
        let res = op.check(cur, val, flags);
        if res {
            if first_sleep {
                thread.set_sync_sleep();
            }
            sleep_info.insert(offset, thread);
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
    ) -> bool {
        let thread = current_thread_ref().unwrap();
        let mut sleep_info = self.sleep_info.lock();

        let cur = unsafe { self.read_atomic_u32(offset) };
        let res = op.check(cur, val, flags);
        if res {
            if first_sleep {
                thread.set_sync_sleep();
            }
            sleep_info.insert(offset, thread);
        }
        res
    }

    pub fn remove_from_sleep_word(&self, offset: usize) {
        let thread = current_thread_ref().unwrap();
        let mut sleep_info = self.sleep_info.lock();
        sleep_info.remove(offset, thread.id());
    }
}
