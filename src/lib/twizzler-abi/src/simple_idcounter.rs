use core::{alloc::Layout, ptr};

pub(crate) struct IdCounter {
    hold: *mut u32,
    hold_len: usize,
    hold_top: usize,
    counter: u32,
}

impl IdCounter {
    pub(crate) const fn new(initial_value: u32) -> Self {
        Self {
            hold: ptr::null_mut(),
            hold_len: 0,
            hold_top: 0,
            counter: initial_value,
        }
    }

    pub(crate) fn next(&mut self) -> u32 {
        if self.hold_len == 0 {
            let n = self.counter;
            self.counter += 1;
            return n;
        }
        let slice = unsafe { core::slice::from_raw_parts(self.hold, self.hold_top) };
        self.hold_len -= 1;
        slice[self.hold_len]
    }

    pub(crate) fn release(&mut self, id: u32) {
        if self.counter == id + 1 {
            self.counter -= 1;
            return;
        }

        if self.hold.is_null() || self.hold_len == self.hold_top {
            self.reserve_more();
        }

        let slice = unsafe { core::slice::from_raw_parts_mut(self.hold, self.hold_top) };
        assert!(slice.len() > self.hold_len);

        slice[self.hold_len] = id;
        self.hold_len += 1;
    }

    fn reserve_more(&mut self) {
        let size = self.hold_top * core::mem::size_of::<u32>();
        let new_top = core::cmp::max(self.hold_top * 2, 16);
        let new_size = new_top * core::mem::size_of::<u32>();
        unsafe {
            self.hold = crate::alloc::global_realloc(
                self.hold as *mut u8,
                Layout::from_size_align(size, core::mem::align_of::<u32>()).unwrap(),
                new_size,
            ) as *mut u32;
        }
        self.hold_top = new_top;
    }
}
