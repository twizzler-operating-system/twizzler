//! Defines a control object caching mechanism, useful for control objects whose base type
//! is updated frequently. Since these objects tend to also be small and use only one page
//! for the base, we optimize a bit by avoiding creating a kernel object handle if the base
//! type fits in one page.

use alloc::{collections::btree_map::BTreeMap, vec::Vec};
use core::{ptr::NonNull, sync::atomic::AtomicU64};

use twizzler_abi::{device::CacheType, object::Protections, syscall::MapFlags};

use crate::{
    memory::{
        context::{
            KernelMemoryContext, KernelObject, KernelObjectHandle, ObjectContextInfo,
            kernel_context,
        },
        frame::FrameRef,
        tracker::{FrameAllocFlags, alloc_frame},
    },
    mutex::Mutex,
    obj::{Object, ObjectRef, PageNumber},
    userinit::create_blank_object,
};

struct QuickBase<Base> {
    base_ptr: NonNull<Base>,
    base_frame: FrameRef,
}

enum QuickOrKernel<Base> {
    Quick(QuickBase<Base>),
    Kernel(KernelObject<Base>),
}

/// Manages a kernel control object, allowing access to the base type, while accelerating
/// that access for the common case.
pub struct ControlObjectCacher<Base> {
    object: ObjectRef,
    quick_or_kernel: QuickOrKernel<Base>,
}

unsafe impl<Base> Send for ControlObjectCacher<Base> {}
unsafe impl<Base> Sync for ControlObjectCacher<Base> {}

impl<Base> ControlObjectCacher<Base> {
    /// Create a new control object cacher, making a new, blank object for it. Initialize the base
    /// with the provided initial data.
    pub fn new(base: Base) -> Self {
        let object = create_blank_object();
        let qok = if core::mem::size_of::<Base>() > PageNumber::PAGE_SIZE {
            object.write_base(&base).unwrap();
            let kobj = kernel_context().insert_kernel_object(ObjectContextInfo::new(
                object.clone(),
                Protections::READ | Protections::WRITE,
                CacheType::WriteBack,
                MapFlags::empty(),
            ));
            QuickOrKernel::Kernel(kobj)
        } else {
            let frame = alloc_frame(
                FrameAllocFlags::ZEROED | FrameAllocFlags::WAIT_OK | FrameAllocFlags::KERNEL,
            );
            frame.set_wired(true);
            object.add_frame(PageNumber::base_page(), frame);
            let base_ptr = frame.virtaddr().as_mut_ptr::<Base>();
            unsafe { base_ptr.write(base) };
            QuickOrKernel::Quick(QuickBase {
                base_ptr: NonNull::new(base_ptr).unwrap(),
                base_frame: frame,
            })
        };
        Self {
            object,
            quick_or_kernel: qok,
        }
    }

    /// Get a reference to the base of this object.
    ///
    /// # Safety
    /// The caller must ensure that the base type is not aliased in a way that leads to unsoundness
    /// for this type.
    pub fn base(&self) -> &Base {
        match &self.quick_or_kernel {
            QuickOrKernel::Quick(quick) => unsafe { quick.base_ptr.as_ref() },
            QuickOrKernel::Kernel(kobj) => kobj.base(),
        }
    }

    /// Get the handle to the underlying object.
    pub fn object(&self) -> &ObjectRef {
        &self.object
    }
}

impl<Base> Drop for ControlObjectCacher<Base> {
    fn drop(&mut self) {
        match &self.quick_or_kernel {
            QuickOrKernel::Quick(quick) => {
                quick.base_frame.set_wired(false);
            }
            QuickOrKernel::Kernel(_) => {}
        }
    }
}

pub struct VNotes {
    notes: Mutex<BTreeMap<u64, Vec<u8>>>,
    next_key: AtomicU64,
}

impl VNotes {
    pub fn new() -> Self {
        Self {
            notes: Mutex::new(BTreeMap::new()),
            next_key: AtomicU64::new(0),
        }
    }

    pub fn find(&self, value: &[u8]) -> Option<u64> {
        let notes = self.notes.lock();
        for (key, note) in notes.iter() {
            if note.as_slice() == value {
                return Some(*key);
            }
        }
        None
    }

    pub fn with_note<R>(&self, key: u64, f: impl FnOnce(&mut Vec<u8>) -> R) -> Option<R> {
        let mut notes = self.notes.lock();
        notes.get_mut(&key).map(f)
    }

    pub fn set(&self, key: u64, value: Vec<u8>) {
        let mut notes = self.notes.lock();
        notes.insert(key, value);
    }

    pub fn remove(&self, key: u64) {
        let mut notes = self.notes.lock();
        notes.remove(&key);
    }

    pub fn reset(&self) {
        let mut notes = self.notes.lock();
        notes.clear();
    }

    pub fn next_key(&self) -> u64 {
        self.next_key
            .fetch_add(1, core::sync::atomic::Ordering::SeqCst)
            + 1
    }
}

impl Object {
    pub fn add_note(&self, value: &[u8]) -> u64 {
        let notes = self.get_notes();
        if let Some(k) = notes.find(value) {
            return k;
        }
        let key = notes.next_key();
        notes.set(key, Vec::from(value));
        key
    }

    pub fn get_note(&self, key: u64, buf: &mut [u8]) -> Option<usize> {
        let notes = self.get_notes();
        notes.with_note(key, |note| {
            let len = core::cmp::min(buf.len(), note.len());
            buf[..len].copy_from_slice(&note[..len]);
            len
        })
    }

    pub fn remove_note(&self, key: u64) {
        let notes = self.get_notes();
        notes.remove(key);
    }

    pub fn enumerate_notes(&self, offset: usize, max: usize) -> Vec<u64> {
        let notes = self.get_notes();
        let notes = notes.notes.lock();
        notes.keys().skip(offset).take(max).copied().collect()
    }

    pub fn print_notes(&self) {
        let notes = self.get_notes();
        let notes = notes.notes.lock();
        for (key, value) in notes.iter() {
            logln!("   {}: {:?}", key, str::from_utf8(value));
        }
    }
}
