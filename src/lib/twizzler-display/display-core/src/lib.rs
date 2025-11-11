use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use twizzler::{
    error::TwzError,
    object::{ObjID, Object, ObjectBuilder, TypedObject},
    ptr::{InvPtr, RefSlice, RefSliceMut},
    BaseType, Invariant,
};
use twizzler_abi::syscall::{
    sys_thread_sync, ThreadSync, ThreadSyncFlags, ThreadSyncOp, ThreadSyncReference,
    ThreadSyncSleep, ThreadSyncWake,
};

#[derive(Clone)]
/// An object holding a double-buffered compositor surface for a window.
pub struct BufferObject {
    obj: Object<DisplayBufferBase>,
}

impl From<Object<DisplayBufferBase>> for BufferObject {
    fn from(obj: Object<DisplayBufferBase>) -> Self {
        Self { obj }
    }
}

const DBF_PHASE: u64 = 0x1;
const DBF_COMP_DONE: u64 = 0x2;

const MAX_W: u64 = 8192;
const MAX_H: u64 = 8192;
const MAX_BUFFER_SIZE: u64 = MAX_W * MAX_H * 4;

#[derive(Invariant, BaseType)]
pub struct DisplayBufferBase {
    pub flags: AtomicU64,
    pub buffers: [DisplayBuffer; 2],
}

#[derive(Invariant)]
pub struct DisplayBuffer {
    pub comp_width: AtomicU32,
    pub comp_height: AtomicU32,
    pub width: AtomicU32,
    pub height: AtomicU32,
    pub byte_len: u64,
    pub ptr: InvPtr<u32>,
}

impl DisplayBuffer {
    unsafe fn buffer_mut(&self) -> RefSliceMut<u32> {
        let ptr = self.ptr.resolve().as_mut();
        let slice = RefSliceMut::from_ref(ptr, self.byte_len as usize);
        slice
    }

    unsafe fn buffer(&self) -> RefSlice<u32> {
        let ptr = self.ptr.resolve();
        let slice = RefSlice::from_ref(ptr, self.byte_len as usize);
        slice
    }
}

impl BufferObject {
    pub fn id(&self) -> ObjID {
        self.obj.id()
    }

    pub fn create_new(w: u32, h: u32) -> Result<Self, TwzError> {
        let builder = ObjectBuilder::default();
        let obj = builder.build_inplace(|mut obj| {
            let buf1 = obj.static_alloc(0).unwrap();
            let buf2 = obj.static_alloc(0).unwrap();

            let base = DisplayBufferBase {
                flags: AtomicU64::new(0),
                buffers: [
                    DisplayBuffer {
                        comp_width: AtomicU32::new(w),
                        comp_height: AtomicU32::new(h),
                        width: AtomicU32::new(w),
                        height: AtomicU32::new(h),
                        byte_len: MAX_BUFFER_SIZE,
                        ptr: InvPtr::from_raw_parts(0, buf1.offset()),
                    },
                    DisplayBuffer {
                        comp_width: AtomicU32::new(w),
                        comp_height: AtomicU32::new(h),
                        width: AtomicU32::new(w),
                        height: AtomicU32::new(h),
                        byte_len: MAX_BUFFER_SIZE,
                        ptr: InvPtr::from_raw_parts(0, buf2.offset()),
                    },
                ],
            };
            obj.write(base)
        })?;

        Ok(BufferObject { obj })
    }

    /// Returns true if the buffers currently need to be read out.
    pub fn has_data_for_compositor(&self) -> bool {
        self.obj.base().flags.load(Ordering::SeqCst) & DBF_COMP_DONE == 0
    }

    /// Read out the compositor buffer.
    pub fn read_compositor_buffer<R>(&self, f: impl FnOnce(&[u32], u32, u32) -> R) -> R {
        let base = self.obj.base();
        let flags = base.flags.load(Ordering::SeqCst);

        let buffer = if flags & DBF_PHASE != 0 {
            &base.buffers[0]
        } else {
            &base.buffers[1]
        };
        let cw = buffer.width.load(Ordering::SeqCst);
        let ch = buffer.height.load(Ordering::SeqCst);
        let buf = unsafe { buffer.buffer() };
        let buf = buf.slice(0..((cw * ch) as usize));
        let r = f(buf.as_slice(), cw, ch);
        r
    }

    /// Mark that the compositor has finished reading the buffer. Provides the new width and height
    /// to use next time this buffer should be filled out. These values may be unchanged.
    pub fn compositor_done(&self, new_w: u32, new_h: u32) {
        let base = self.obj.base();
        let flags = base.flags.load(Ordering::SeqCst);
        let buffer = if flags & DBF_PHASE != 0 {
            &base.buffers[0]
        } else {
            &base.buffers[1]
        };
        buffer.comp_height.store(new_h, Ordering::Release);
        buffer.comp_width.store(new_w, Ordering::Release);
        base.flags.fetch_or(DBF_COMP_DONE, Ordering::SeqCst);
        let _ = sys_thread_sync(
            &mut [ThreadSync::new_wake(ThreadSyncWake::new(
                ThreadSyncReference::Virtual(&base.flags),
                usize::MAX,
            ))],
            None,
        );
    }

    /// Fill the current application-owned buffer with data within the callback.
    pub fn fill_current_buffer<R>(&self, f: impl FnOnce(&mut [u32], u32, u32) -> R) -> R {
        let base = self.obj.base();
        let mut flags = base.flags.load(Ordering::SeqCst);

        while flags & DBF_COMP_DONE == 0 {
            let _ = sys_thread_sync(
                &mut [ThreadSync::new_sleep(ThreadSyncSleep::new(
                    ThreadSyncReference::Virtual(&base.flags),
                    flags,
                    ThreadSyncOp::Equal,
                    ThreadSyncFlags::empty(),
                ))],
                None,
            );
            flags = base.flags.load(Ordering::SeqCst);
        }

        let buffer = if flags & DBF_PHASE != 0 {
            &base.buffers[1]
        } else {
            &base.buffers[0]
        };
        let cw = buffer.comp_width.load(Ordering::SeqCst);
        let ch = buffer.comp_height.load(Ordering::SeqCst);

        let buf = unsafe { buffer.buffer_mut() };
        let mut buf = buf.slice(0..((cw * ch) as usize));
        let r = f(buf.as_slice_mut(), cw, ch);
        buffer.height.store(ch, Ordering::Release);
        buffer.width.store(cw, Ordering::Release);
        r
    }

    /// Flip the buffer, indicating that the compositor can now read the buffer.
    pub fn flip(&self) {
        let base = self.obj.base();
        let flags = base.flags.load(Ordering::SeqCst);
        let new_flags = (flags ^ DBF_PHASE) & !DBF_COMP_DONE;
        base.flags.store(new_flags, Ordering::SeqCst);

        let _ = sys_thread_sync(
            &mut [ThreadSync::new_wake(ThreadSyncWake::new(
                ThreadSyncReference::Virtual(&base.flags),
                usize::MAX,
            ))],
            None,
        );
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(C)]
pub struct WindowConfig {
    pub w: u32,
    pub h: u32,
    pub x: u32,
    pub y: u32,
    pub z: u32,
}
