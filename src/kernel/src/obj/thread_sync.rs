use alloc::{boxed::Box, collections::BTreeMap, sync::Arc, vec::Vec};
use core::{
    ptr::NonNull,
    sync::atomic::{AtomicPtr, AtomicU32, AtomicU64, AtomicUsize, Ordering},
};

use heapless::index_map::FnvIndexMap;
use intrusive_collections::{Adapter, KeyAdapter, RBTree, RBTreeAtomicLink, container_of, rbtree};
use twizzler_abi::{
    device::NUM_DEVICE_INTERRUPTS,
    object::ObjID,
    syscall::{ThreadSyncFlags, ThreadSyncOp},
};
use twizzler_rt_abi::error::TwzError;

use super::{OBJ_HAS_INTERRUPTS, Object};
use crate::{
    interrupt::{remove_from_device_wait, wait_for_device_interrupt},
    obj::ObjectRef,
    syscall::sync::add_to_requeue,
    thread::{Thread, ThreadRef, current_thread_ref},
};

struct SleepLinkNode {
    link: RBTreeAtomicLink,
    owner: Arc<Thread>,
}

pub struct ThreadSleepLinker {
    links: AtomicPtr<Box<[SleepLinkNode]>>,
    next: AtomicUsize,
}

impl Drop for ThreadSleepLinker {
    fn drop(&mut self) {
        let links = self.links.swap(core::ptr::null_mut(), Ordering::SeqCst);
        if links.is_null() {
            return;
        }
        let links = unsafe { Box::from_raw(links) };
        drop(links);
    }
}

impl ThreadSleepLinker {
    pub fn new() -> Self {
        Self {
            links: AtomicPtr::new(Box::into_raw(Box::new(Box::new([])))),
            next: AtomicUsize::new(0),
        }
    }

    fn get_links(&self) -> &Box<[SleepLinkNode]> {
        if self.links.load(Ordering::SeqCst).is_null() {
            panic!("ThreadSleepLinker: get_links called after clear_all_references");
        }
        unsafe { &*self.links.load(Ordering::SeqCst) }
    }

    pub fn len(&self) -> usize {
        self.get_links().len()
    }

    pub fn reserve(&self, count: usize, thread: &ThreadRef) {
        assert!(
            self.next.load(Ordering::SeqCst) == 0,
            "ThreadSleepLinker::reserve called mid-round"
        );
        assert!(&thread.sync_links as *const _ == self as *const _);
        let old = self.links.load(Ordering::SeqCst);
        if self.len() < count {
            let mut new_links = Vec::with_capacity(count);
            for _ in 0..count {
                new_links.push(SleepLinkNode {
                    link: RBTreeAtomicLink::new(),
                    owner: thread.clone(),
                });
            }
            let new_links = new_links.into_boxed_slice();
            let new_links_ptr = Box::into_raw(Box::new(new_links));

            if self
                .links
                .compare_exchange(old, new_links_ptr, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
            {
                drop(unsafe { Box::from_raw(new_links_ptr) });
                return self.reserve(count, thread);
            }

            drop(unsafe { Box::from_raw(old) });
        }
    }

    /// Returns the next available link slot, stamping it with `owner` so
    /// that [ThreadSleepAdapter::get_value] can later find its way back to
    /// the owning thread. Only ever called from `ThreadSleepAdapter`.
    fn get_link(&self) -> (&RBTreeAtomicLink, usize) {
        let links = self.get_links();
        let idx = self.next.fetch_add(1, Ordering::SeqCst);
        if idx >= links.len() {
            panic!("ThreadSleepLinker: not enough reserved capacity, call reserve() first");
        }
        (&links[idx].link, idx)
    }

    pub fn reset(&self) {
        let n = self.next.load(Ordering::SeqCst);
        for i in 0..n {
            let links = self.get_links();
            let link = &links[i].link;
            if link.is_linked() {
                log::warn!("ThreadSleepLinker::reset: link {} is still linked", i);
            }
        }
        self.next.store(0, Ordering::SeqCst);
    }

    /// True if any slot is currently linked into some `RBTree`.
    pub fn is_linked(&self) -> bool {
        self.next.load(Ordering::SeqCst) > 0
    }

    pub fn clear_all_references(&self) {
        let links = self.links.swap(core::ptr::null_mut(), Ordering::SeqCst);
        if links.is_null() {
            return;
        }
        let links = unsafe { Box::from_raw(links) };
        drop(links);
    }
}

#[derive(Clone)]
pub struct ThreadSleepAdapter {
    link_ops: rbtree::AtomicLinkOps,
    pointer_ops: intrusive_collections::DefaultPointerOps<ThreadRef>,
}

impl ThreadSleepAdapter {
    pub const NEW: Self = Self {
        link_ops: rbtree::AtomicLinkOps,
        pointer_ops: intrusive_collections::DefaultPointerOps::new(),
    };

    pub fn new() -> Self {
        Self::NEW
    }
}

impl Default for ThreadSleepAdapter {
    fn default() -> Self {
        Self::NEW
    }
}

unsafe impl Adapter for ThreadSleepAdapter {
    type LinkOps = rbtree::AtomicLinkOps;
    type PointerOps = intrusive_collections::DefaultPointerOps<ThreadRef>;

    unsafe fn get_value(&self, link: NonNull<RBTreeAtomicLink>) -> *const Thread {
        unsafe {
            let node = container_of!(link.as_ptr(), SleepLinkNode, link);
            let arc = &(*node).owner;
            Arc::as_ptr(arc)
        }
    }

    unsafe fn get_link(&self, value: *const Thread) -> NonNull<RBTreeAtomicLink> {
        unsafe {
            let thread = &*value;
            let (link, _idx) = thread.sync_links.get_link();
            NonNull::from(link)
        }
    }

    fn link_ops(&self) -> &Self::LinkOps {
        &self.link_ops
    }

    fn link_ops_mut(&mut self) -> &mut Self::LinkOps {
        &mut self.link_ops
    }

    fn pointer_ops(&self) -> &Self::PointerOps {
        &self.pointer_ops
    }
}

impl<'a> KeyAdapter<'a> for ThreadSleepAdapter {
    type Key = ObjID;

    fn get_key(&self, t: &'a Thread) -> ObjID {
        t.objid()
    }
}

struct SleepEntry {
    of_obj: ObjID,
    threads: RBTree<ThreadSleepAdapter>,
}

impl SleepEntry {
    pub fn new(thread: ThreadRef, of_obj: ObjID) -> Self {
        let mut threads = RBTree::new(ThreadSleepAdapter::NEW);
        threads.insert(thread);
        Self { threads, of_obj }
    }

    pub fn add_thread(&mut self, thread: ThreadRef) {
        // If already on this list, skip -- mirrors do_add_to_requeue's
        // find()+insert() guard in syscall/sync.rs; protected by the
        // caller's sleep_info lock, so no TOCTOU race.
        if !self.threads.find(&thread.objid()).is_null() {
            return;
        }
        self.threads.insert(thread);
    }

    pub fn remove_thread(&mut self, id: ObjID) {
        let mut cursor = self.threads.find_mut(&id);
        cursor.remove();
    }

    pub fn wake_n(&mut self, max_count: usize) -> usize {
        let mut count = 0;
        let mut cursor = self.threads.front_mut();
        while !cursor.is_null() && count < max_count {
            let thread = cursor.get().unwrap();
            if thread.reset_sync_sleep() {
                let thread = cursor.remove().unwrap();
                add_to_requeue(thread);
                count += 1;
            } else {
                cursor.move_next();
            }
        }
        return count;
    }
}

impl Drop for SleepEntry {
    fn drop(&mut self) {
        let mut cursor = self.threads.front_mut();
        while !cursor.is_null() {
            let thread = cursor.remove().unwrap();
            if thread.reset_sync_sleep() {
                add_to_requeue(thread);
            }
        }
        self.threads.fast_clear();
    }
}

pub struct SleepInfo {
    of_obj: ObjID,
    some_words: FnvIndexMap<usize, SleepEntry, 32>,
    more_words: Option<BTreeMap<usize, SleepEntry>>,
}

impl SleepInfo {
    pub fn new(of_obj: ObjID) -> Self {
        SleepInfo {
            some_words: FnvIndexMap::new(),
            more_words: None,
            of_obj,
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
                words.insert(offset, SleepEntry::new(thread, self.of_obj));
            } else {
                match self
                    .some_words
                    .insert(offset, SleepEntry::new(thread, self.of_obj))
                {
                    Ok(_) => {}
                    Err((_, se)) => {
                        log::debug!("overflowing sleep entries");
                        // Clear the old words, wake up all those threads.
                        self.some_words.clear();
                        let mw = self.more_words.get_or_insert(BTreeMap::new());
                        mw.insert(offset, se);
                    }
                }
            }
        }
    }

    pub fn remove(&mut self, offset: usize, thread_id: ObjID) {
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
        self: &ObjectRef,
        offset: usize,
        op: ThreadSyncOp,
        val: u64,
        first_sleep: bool,
        flags: ThreadSyncFlags,
        vaddr: Option<&AtomicU64>,
    ) -> Result<bool, TwzError> {
        let thread = current_thread_ref().unwrap();

        if let Some(vaddr) = vaddr {
            let cur = vaddr.load(Ordering::SeqCst);
            if !op.check(cur, val, flags) {
                return Ok(false);
            }
            if self.flags.load(Ordering::Acquire) & OBJ_HAS_INTERRUPTS != 0 {
                for i in 0..NUM_DEVICE_INTERRUPTS {
                    let di_offset = self.device_interrupt_info[i].1.load(Ordering::Acquire);
                    let di_vector = self.device_interrupt_info[i].0.load(Ordering::Acquire);
                    if di_offset as usize == offset {
                        return Ok(wait_for_device_interrupt(
                            thread,
                            di_vector as u32,
                            first_sleep,
                            vaddr,
                        ));
                    }
                }
            }
        }

        let mut sleep_info = self.sleep_info.lock();
        let cur = vaddr
            .map(|vaddr| Ok(vaddr.load(Ordering::SeqCst)))
            .unwrap_or_else(|| self.read_atomic_64(offset))?;
        let res = op.check(cur, val, flags);
        log::trace!(
            "thread {} ({}) setting sleep word on {} (did sleep? {})",
            thread.id(),
            thread.objid(),
            self.id(),
            res,
        );
        if res {
            if first_sleep {
                thread.set_sync_sleep();
            }
            sleep_info.insert(offset, thread.clone());
        }
        Ok(res)
    }

    pub fn setup_sleep_word32(
        self: &ObjectRef,
        offset: usize,
        op: ThreadSyncOp,
        val: u32,
        first_sleep: bool,
        flags: ThreadSyncFlags,
        vaddr: Option<&AtomicU32>,
    ) -> Result<bool, TwzError> {
        let thread = current_thread_ref().unwrap();
        if let Some(vaddr) = vaddr {
            let cur = vaddr.load(Ordering::SeqCst);
            if !op.check(cur, val, flags) {
                return Ok(false);
            }
        }
        let mut sleep_info = self.sleep_info.lock();

        let cur = vaddr
            .map(|vaddr| Ok(vaddr.load(Ordering::SeqCst)))
            .unwrap_or_else(|| self.read_atomic_32(offset))?;
        let res = op.check(cur, val, flags);
        if res {
            if first_sleep {
                thread.set_sync_sleep();
            }
            sleep_info.insert(offset, thread.clone());
        }
        Ok(res)
    }

    pub fn remove_from_sleep_word(&self, offset: usize) {
        let thread = current_thread_ref().unwrap();
        let mut sleep_info = self.sleep_info.lock();
        sleep_info.remove(offset, thread.objid());

        // TODO: I think this only works if the thread waits on one interrupt.
        if self.flags.load(Ordering::Acquire) & OBJ_HAS_INTERRUPTS != 0 {
            for i in 0..NUM_DEVICE_INTERRUPTS {
                let di_offset = self.device_interrupt_info[i].1.load(Ordering::Acquire);
                let di_vector = self.device_interrupt_info[i].0.load(Ordering::Acquire);
                if di_offset as usize == offset {
                    remove_from_device_wait(thread, di_vector as u32);
                    break;
                }
            }
        }
    }
}
