use crate::MemSize;

use std::mem::{self, MaybeUninit};
use std::ptr;

/// Gets the memory an entry with the given key and value would occupy in an
/// LRU cache, in bytes. This is also the function used internally, thus if the
/// returned number of bytes fits inside the cache (as can be determined using
/// [LruCache::current_size](crate::LruCache::current_size) and
/// [LruCache::max_size](crate::LruCache::max_size)), it is guaranteed not to
/// eject an element.
///
/// # Arguments
///
/// * `key`: A reference to the key of the entry whose size to determine.
/// * `value`: A reference to the value of the entry whose size to determine.
///
/// # Example
///
/// ```
/// let key_1 = 0u64;
/// let value_1 = vec![0u8; 10];
/// let size_1 = lru_mem::entry_size(&key_1, &value_1);
///
/// let key_2 = 1u64;
/// let value_2 = vec![0u8; 1000];
/// let size_2 = lru_mem::entry_size(&key_2, &value_2);
///
/// assert!(size_1 < size_2);
/// ```
pub fn entry_size<K: MemSize, V: MemSize>(key: &K, value: &V) -> usize {
    let key_heap_size = key.heap_size();
    let value_heap_size = value.heap_size();
    let value_size = mem::size_of::<Entry<K, V>>();
    key_heap_size + value_heap_size + value_size
}

pub(crate) struct UnhingedEntry<K, V> {
    size: usize,
    key: K,
    value: V
}

impl<K: MemSize, V: MemSize> UnhingedEntry<K, V> {
    pub(crate) fn new(key: K, value: V) -> UnhingedEntry<K, V> {
        let size = entry_size(&key, &value);

        UnhingedEntry {
            size,
            key,
            value
        }
    }
}

impl<K, V> UnhingedEntry<K, V> {
    pub(crate) fn size(&self) -> usize {
        self.size
    }

    pub(crate) fn key(&self) -> &K {
        &self.key
    }

    pub(crate) fn into_key_value(self) -> (K, V) {
        (self.key, self.value)
    }
}

pub(crate) struct Entry<K, V> {
    pub(crate) size: usize,
    pub(crate) prev: EntryPtr<K, V>,
    pub(crate) next: EntryPtr<K, V>,
    key: MaybeUninit<K>,
    value: MaybeUninit<V>
}

impl<K, V> Entry<K, V> {

    /// Safety: Requires key to be initialized.
    pub(crate) unsafe fn key(&self) -> &K {
        self.key.assume_init_ref()
    }

    /// Safety: Requires value to be initialized.
    pub(crate) unsafe fn value(&self) -> &V {
        self.value.assume_init_ref()
    }

    /// Safety: Requires value to be initialized.
    pub(crate) unsafe fn value_mut(&mut self) -> &mut V {
        self.value.assume_init_mut()
    }

    /// Safety: Requires key and value to be initialized.
    pub(crate) unsafe fn into_key_value(self) -> (K, V) {
        (self.key.assume_init(), self.value.assume_init())
    }

    // Safety: Requires key and value to be initialized.
    pub(crate) unsafe fn drop(mut self) {
        ptr::drop_in_place(self.key.as_mut_ptr());
        ptr::drop_in_place(self.value.as_mut_ptr());
    }

    /// Safety: Key and value must be initialized.
    pub(crate) unsafe fn unhinge(mut self) -> UnhingedEntry<K, V> {
        self.prev.get_mut().next = self.next;
        self.next.get_mut().prev = self.prev;

        UnhingedEntry {
            size: self.size,
            key: self.key.assume_init(),
            value: self.value.assume_init()
        }
    }
}

impl<K: Clone, V: Clone> Entry<K, V> {

    /// Safety: Requires key and value to be initialized.
    pub(crate) unsafe fn clone(&self) -> Entry<K, V> {
        Entry {
            size: self.size,
            prev: self.prev,
            next: self.next,
            key: MaybeUninit::new(self.key().clone()),
            value: MaybeUninit::new(self.value().clone())
        }
    }
}

impl<K, V> Entry<K, V> {
    pub(crate) fn new(entry: UnhingedEntry<K, V>, prev: EntryPtr<K, V>,
            next: EntryPtr<K, V>) -> Entry<K, V> {
        Entry {
            size: entry.size,
            prev,
            next,
            key: MaybeUninit::new(entry.key),
            value: MaybeUninit::new(entry.value)
        }
    }
}

pub(crate) struct EntryPtr<K, V> {
    ptr: *mut Entry<K, V>
}

impl<K, V> PartialEq for EntryPtr<K, V> {
    fn eq(&self, other: &Self) -> bool {
        self.ptr == other.ptr
    }
}

impl<K, V> Clone for EntryPtr<K, V> {
    fn clone(&self) -> Self {
        EntryPtr {
            ptr: self.ptr
        }
    }
}

impl<K, V> Copy for EntryPtr<K, V> { }

impl<K, V> EntryPtr<K, V> {

    pub(crate) fn new(ptr: *mut Entry<K, V>) -> EntryPtr<K, V> {
        EntryPtr { ptr }
    }

    pub(crate) fn new_seal() -> EntryPtr<K, V> {
        let entry = Entry {
            size: 0,
            prev: EntryPtr {
                ptr: ptr::null_mut()
            },
            next: EntryPtr {
                ptr: ptr::null_mut()
            },
            key: MaybeUninit::uninit(),
            value: MaybeUninit::uninit()
        };
        let mut ptr = EntryPtr {
            ptr: Box::into_raw(Box::new(entry))
        };
        let ptr_clone = ptr;
        let entry = ptr.get_mut();
        entry.prev = ptr_clone;
        entry.next = ptr_clone;

        ptr
    }

    /// Safety: May never be dereferenced in any way (get, get_mut, move_to,
    /// read, drop_seal).
    pub(crate) unsafe fn null() -> EntryPtr<K, V> {
        EntryPtr {
            ptr: ptr::null_mut()
        }
    }

    pub(crate) fn is_null(&self) -> bool {
        self.ptr.is_null()
    }

    pub(crate) fn get(&self) -> &Entry<K, V> {
        unsafe { &*self.ptr }
    }

    pub(crate) fn get_mut(&mut self) -> &mut Entry<K, V> {
        unsafe { &mut *self.ptr }
    }

    /// Safety: Must ensure the pointer is valid for the given lifetime.
    pub(crate) unsafe fn get_extended<'a>(self) -> &'a Entry<K, V> {
        &*self.ptr
    }

    /// Safety: Must ensure the entry is re-inserted at the appropriate
    /// location.
    pub(crate) unsafe fn unhinge(self) {
        let mut prev = self.get().prev;
        let mut next = self.get().next;

        prev.get_mut().next = next;
        next.get_mut().prev = prev;
    }

    pub(crate) fn insert(&mut self, mut prev: EntryPtr<K, V>,
            mut next: EntryPtr<K, V>) {
        let self_ptr = *self;
        let entry_mut = self.get_mut();

        prev.get_mut().next = self_ptr;
        next.get_mut().prev = self_ptr;
        entry_mut.next = next;
        entry_mut.prev = prev;
    }

    /// Safety: This pointer and all its copies must never be used again.
    pub(crate) unsafe fn read(self) -> Entry<K, V> {
        ptr::read(self.ptr)
    }

    /// Safety: This pointer and all its copies must never be used again.
    pub(crate) unsafe fn drop_seal(self) {
        let _ = Box::from_raw(self.ptr);
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn entry_correctly_computes_size() {
        let entry =
            UnhingedEntry::new("hello".to_owned(), "world!".to_owned());

        let key_str_bytes = 5;
        let value_str_bytes = 6;
        let str_meta_bytes = mem::size_of::<String>();
        let usize_bytes = mem::size_of::<usize>();
        let ptr_bytes = mem::size_of::<*mut Entry<String, String>>();

        // We require key + value (key_str_bytes + value_str_bytes +
        // 2 * str_meta_bytes), 1 usize (size of entry), and 2 pointers (next
        // and prev).

        let expected_bytes = key_str_bytes
            + value_str_bytes
            + 2 * str_meta_bytes
            + usize_bytes
            + 2 * ptr_bytes;

        assert_eq!(expected_bytes, entry.size());
    }
}
