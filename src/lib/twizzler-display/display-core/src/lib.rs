use std::{
    cell::UnsafeCell,
    ops::{Index, IndexMut},
    sync::atomic::{AtomicU32, AtomicU64, Ordering},
    u32,
};

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

#[derive(Invariant, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl From<WindowConfig> for Rect {
    fn from(value: WindowConfig) -> Self {
        Self {
            x: value.x,
            y: value.y,
            w: value.w,
            h: value.h,
        }
    }
}

impl Rect {
    pub const fn new(x: u32, y: u32, w: u32, h: u32) -> Self {
        Self { x, y, w, h }
    }

    pub const fn full() -> Self {
        Self {
            x: 0,
            y: 0,
            w: u32::MAX,
            h: u32::MAX,
        }
    }

    pub fn is_covered_by_any(&self, rects: &[Rect]) -> bool {
        for rect in rects {
            if rect.x <= self.x
                && rect.y <= self.y
                && rect.x + rect.w >= self.x + self.w
                && rect.y + rect.h >= self.y + self.h
            {
                return true;
            }
        }
        false
    }

    pub fn extent_of(rects: &[Rect]) -> Self {
        let x = rects.iter().min_by_key(|r| r.x).map(|r| r.x).unwrap_or(0);
        let y = rects.iter().min_by_key(|r| r.y).map(|r| r.y).unwrap_or(0);
        Rect {
            x,
            y,
            w: rects
                .iter()
                .max_by_key(|r| r.x + r.w)
                .map(|r| r.x + r.w)
                .unwrap_or(0)
                - x,
            h: rects
                .iter()
                .max_by_key(|r| r.y + r.h)
                .map(|r| r.y + r.h)
                .unwrap_or(0)
                - y,
        }
    }
}

const NUM_DAMAGE: usize = 8;
const FULL_DAMAGE: u64 = 0xFFFFFFFFFFFFFFFF;

#[derive(Invariant)]
pub struct DisplayBuffer {
    pub comp_width: AtomicU32,
    pub comp_height: AtomicU32,
    pub width: AtomicU32,
    pub height: AtomicU32,
    pub byte_len: u64,
    pub ptr: InvPtr<u32>,
    pub damage: UnsafeCell<[Rect; NUM_DAMAGE]>,
    pub damage_count: AtomicU64,
}

pub struct Buffer<'a> {
    buffer: &'a [u32],
    db: &'a DisplayBuffer,
}

impl<'a> AsRef<[u32]> for Buffer<'a> {
    fn as_ref(&self) -> &[u32] {
        self.buffer
    }
}

impl<'a> Index<usize> for Buffer<'a> {
    type Output = u32;

    fn index(&self, index: usize) -> &Self::Output {
        &self.buffer[index]
    }
}

impl<'a> Buffer<'a> {
    fn new(s: &mut RefSlice<'a, u32>, db: &'a DisplayBuffer) -> Self {
        Self {
            buffer: s.as_slice(),
            db,
        }
    }

    pub fn damage_rects(&self) -> &[Rect] {
        unsafe { self.db.damage() }
    }

    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    pub fn as_slice(&self) -> &[u32] {
        self.buffer
    }
}

pub struct BufferMut<'a> {
    buffer: &'a mut [u32],
    db: &'a DisplayBuffer,
}

impl<'a> AsRef<[u32]> for BufferMut<'a> {
    fn as_ref(&self) -> &[u32] {
        self.buffer
    }
}

impl<'a> AsMut<[u32]> for BufferMut<'a> {
    fn as_mut(&mut self) -> &mut [u32] {
        self.buffer
    }
}

impl<'a> Index<usize> for BufferMut<'a> {
    type Output = u32;

    fn index(&self, index: usize) -> &Self::Output {
        &self.buffer[index]
    }
}

impl<'a> IndexMut<usize> for BufferMut<'a> {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        &mut self.buffer[index]
    }
}

impl<'a> BufferMut<'a> {
    fn new(s: &mut RefSliceMut<'a, u32>, db: &'a DisplayBuffer) -> Self {
        Self {
            buffer: s.as_slice_mut(),
            db,
        }
    }

    pub fn damage_rects(&self) -> &[Rect] {
        unsafe { self.db.damage() }
    }

    pub fn damage(&self, dmg: Rect) {
        unsafe { self.db.append_damage(dmg) };
    }

    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    pub fn as_slice(&self) -> &[u32] {
        self.buffer
    }

    pub fn as_slice_mut(&mut self) -> &mut [u32] {
        self.buffer
    }
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

    unsafe fn append_damage(&self, dmg: Rect) {
        let current_count = self.damage_count.load(Ordering::SeqCst);
        if current_count == FULL_DAMAGE {
            return;
        }
        let damage = self.damage.get().as_mut().unwrap_unchecked();
        if current_count as usize == NUM_DAMAGE {
            damage[0] = Rect::extent_of(&damage.as_slice()[..(current_count as usize)]);
            self.damage_count.store(1, Ordering::Release);
            return;
        }

        if dmg.is_covered_by_any(&damage[..(current_count as usize)]) {
            return;
        }

        damage[current_count as usize] = dmg;
        self.damage_count.fetch_add(1, Ordering::Release);
    }

    unsafe fn reset_damage(&self) {
        self.damage_count.store(0, Ordering::SeqCst);
    }

    unsafe fn damage(&self) -> &[Rect] {
        const FD: [Rect; 1] = [Rect::full()];
        let count = self.damage_count.load(Ordering::Acquire);
        if count == FULL_DAMAGE {
            return &FD;
        }
        &self.damage.get().as_ref().unwrap_unchecked()[..count as usize]
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
                        damage_count: AtomicU64::new(0),
                        damage: UnsafeCell::new([Rect::full(); NUM_DAMAGE]),
                    },
                    DisplayBuffer {
                        comp_width: AtomicU32::new(w),
                        comp_height: AtomicU32::new(h),
                        width: AtomicU32::new(w),
                        height: AtomicU32::new(h),
                        byte_len: MAX_BUFFER_SIZE,
                        ptr: InvPtr::from_raw_parts(0, buf2.offset()),
                        damage_count: AtomicU64::new(0),
                        damage: UnsafeCell::new([Rect::full(); NUM_DAMAGE]),
                    },
                ],
            };
            obj.write(base)
        })?;

        Ok(BufferObject { obj })
    }

    /// Returns true if the buffers currently need to be read out.
    pub fn has_data_for_read(&self) -> bool {
        self.obj.base().flags.load(Ordering::SeqCst) & DBF_COMP_DONE == 0
    }

    /// Read out the compositor buffer.
    pub fn read_buffer<R>(&self, mut f: impl FnMut(Buffer, u32, u32) -> R) -> R {
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
        let mut buf = buf.slice(0..((cw * ch) as usize));
        let r = f(Buffer::new(&mut buf, buffer), cw, ch);
        r
    }

    /// Mark that the compositor has finished reading the buffer. Provides the new width and height
    /// to use next time this buffer should be filled out. These values may be unchanged.
    pub fn read_done(&self, new_w: u32, new_h: u32) {
        let base = self.obj.base();
        let flags = base.flags.load(Ordering::SeqCst);
        let buffer = if flags & DBF_PHASE != 0 {
            &base.buffers[0]
        } else {
            &base.buffers[1]
        };
        buffer.comp_height.store(new_h, Ordering::Release);
        buffer.comp_width.store(new_w, Ordering::Release);
        unsafe { buffer.reset_damage() };
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
    pub fn update_buffer<R>(&self, mut f: impl FnMut(BufferMut, u32, u32) -> R) -> R {
        let base = self.obj.base();
        let flags = base.flags.load(Ordering::SeqCst);

        let buffer = if flags & DBF_PHASE != 0 {
            &base.buffers[1]
        } else {
            &base.buffers[0]
        };
        let cw = buffer.comp_width.load(Ordering::SeqCst);
        let ch = buffer.comp_height.load(Ordering::SeqCst);

        let buf = unsafe { buffer.buffer_mut() };
        let mut buf = buf.slice(0..((cw * ch) as usize));
        let r = f(BufferMut::new(&mut buf, buffer), cw, ch);
        buffer.height.store(ch, Ordering::Release);
        buffer.width.store(cw, Ordering::Release);
        r
    }

    /// Flip the buffer, indicating that the compositor can now read the buffer.
    pub fn flip(&self) {
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

        let (src_buffer, dst_buffer) = if flags & DBF_PHASE != 0 {
            (&base.buffers[0], &base.buffers[1])
        } else {
            (&base.buffers[1], &base.buffers[0])
        };

        let w = src_buffer.width.load(Ordering::Acquire);
        let h = src_buffer.height.load(Ordering::Acquire);
        for dmg in unsafe { src_buffer.damage() } {
            for y in dmg.y..(dmg.y + dmg.h.min(h - dmg.y)) {
                let start = (y * w + dmg.x) as usize;
                let len = dmg.w.min(w - dmg.x) as usize;
                let src = &unsafe { src_buffer.buffer().as_slice() }[start..(start + len)];
                let dst =
                    &mut unsafe { dst_buffer.buffer_mut().as_slice_mut() }[start..(start + len)];
                dst.copy_from_slice(src);
            }
        }

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
