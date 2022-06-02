use std::{
    mem::{size_of, MaybeUninit},
    sync::Mutex,
};

use twizzler_object::Object;

use crate::req::PacketData;

#[repr(C)]
pub struct BufferBase {
    counter: u32,
    pos: usize,
    reuse: [u32; 4096],
}

impl twizzler_abi::marker::BaseType for BufferBase {
    fn init<T>(_t: T) -> Self {
        todo!()
    }

    fn tags() -> &'static [(
        twizzler_abi::marker::BaseVersion,
        twizzler_abi::marker::BaseTag,
    )] {
        todo!()
    }
}

pub struct BufferController {
    #[allow(dead_code)]
    mgr: bool,
    #[allow(dead_code)]
    tx: bool,
    #[allow(dead_code)]
    obj: Mutex<Object<BufferBase>>,
}

const BUFFER_SIZE: usize = 8 * 1024; //TODO

pub struct ManagedBuffer<'a> {
    controller: &'a BufferController,
    idx: u32,
    len: usize,
    owned: bool,
}

impl<'a> ManagedBuffer<'a> {
    fn new(controller: &'a BufferController, idx: u32, len: usize) -> Self {
        Self {
            controller,
            idx,
            owned: true,
            len,
        }
    }

    pub fn new_unowned(controller: &'a BufferController, idx: u32, len: usize) -> Self {
        Self {
            controller,
            idx,
            owned: false,
            len,
        }
    }

    pub fn buffer_len(&self) -> usize {
        self.len
    }

    pub fn set_len(&mut self, len: usize) {
        if len > BUFFER_SIZE {
            panic!("cannot set buffer size above maximum value");
        }
        self.len = len;
    }

    pub fn as_bytes(&self) -> &[u8] {
        self.controller.get_slice(self.idx, self.len)
    }

    #[allow(clippy::mut_from_ref)]
    pub fn as_bytes_mut(&self) -> &mut [u8] {
        self.controller.get_slice_mut(self.idx, self.len)
    }

    pub fn copy_in(&mut self, data: &[u8]) {
        self.len = data.len();
        self.as_bytes_mut().copy_from_slice(data);
    }

    pub fn as_packet_data(&self) -> PacketData {
        PacketData {
            buffer_idx: self.idx,
            buffer_len: self.len as u32,
        }
    }

    pub fn get_data_mut<T>(&mut self, offset: usize) -> &mut MaybeUninit<T> {
        if offset + size_of::<T>() >= BUFFER_SIZE {
            panic!("tried to access buffer data out of bounds");
        }
        self.len = std::cmp::max(self.len, offset + size_of::<T>());
        let bytes = self.as_bytes_mut();
        unsafe {
            ((&mut bytes[offset..(offset + size_of::<T>())]).as_mut_ptr() as *mut MaybeUninit<T>)
                .as_mut()
                .unwrap()
        }
    }
}

impl BufferController {
    pub(crate) fn new(mgr: bool, tx: bool, obj: Object<BufferBase>) -> Self {
        Self {
            mgr,
            tx,
            obj: Mutex::new(obj),
        }
    }

    pub async fn allocate(&self) -> ManagedBuffer<'_> {
        let obj = self.obj.lock().unwrap();
        // TODO: unsafe
        let base = unsafe { obj.base_mut_unchecked() };
        let b = if base.pos == 0 {
            let b = ManagedBuffer::new(self, base.counter, 0);
            base.counter += 1;
            b
        } else {
            base.pos -= 1;
            ManagedBuffer::new(self, base.reuse[base.pos], 0)
        };
        println!(
            "allocated buffer {} from {} {}",
            b.idx,
            if self.mgr { "mgr" } else { "client" },
            if self.tx { "tx" } else { "rx" }
        );
        b
    }

    pub fn release(&self, idx: u32) {
        println!(
            "releasing buffer {} to {} {}",
            idx,
            if self.mgr { "mgr" } else { "client" },
            if self.tx { "tx" } else { "rx" }
        );
        let obj = self.obj.lock().unwrap();
        // TODO: unsafe
        let base = unsafe { obj.base_mut_unchecked() };
        if base.counter == idx + 1 {
            base.counter -= 1;
        } else {
            base.reuse[base.pos] = idx;
            base.pos += 1;
        }
    }

    pub fn get_slice(&self, idx: u32, len: usize) -> &[u8] {
        let obj = self.obj.lock().unwrap();
        let ptr = obj.raw_lea(idx as usize * BUFFER_SIZE + 0x2000);
        unsafe { core::slice::from_raw_parts(ptr, len) }
    }

    #[allow(clippy::mut_from_ref)]
    pub fn get_slice_mut(&self, idx: u32, len: usize) -> &mut [u8] {
        let obj = self.obj.lock().unwrap();
        let ptr = obj.raw_lea_mut(idx as usize * BUFFER_SIZE + 0x2000);
        unsafe { core::slice::from_raw_parts_mut(ptr, len) }
    }
}

impl<'a> Drop for ManagedBuffer<'a> {
    fn drop(&mut self) {
        if self.owned {
            self.controller.release(self.idx);
        }
    }
}
