//! Input events for a window: a ring in an object shared with the display server.

use std::{
    cell::UnsafeCell,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

use twizzler::{
    error::TwzError,
    object::{MapFlags, ObjID, Object, ObjectBuilder, TypedObject},
    BaseType, Invariant,
};
use twizzler_abi::syscall::{
    sys_thread_sync, ThreadSync, ThreadSyncFlags, ThreadSyncOp, ThreadSyncReference,
    ThreadSyncSleep, ThreadSyncWake,
};

const QUEUE_LEN: usize = 256;

/// Key and button codes are those of Linux evdev (`input-event-codes.h`).
pub mod codes {
    pub const KEY_ESC: u32 = 1;
    pub const KEY_MINUS: u32 = 12;
    pub const KEY_EQUAL: u32 = 13;
    pub const KEY_Q: u32 = 16;
    pub const KEY_ENTER: u32 = 28;
    pub const KEY_SPACE: u32 = 57;
    pub const KEY_UP: u32 = 103;
    pub const KEY_LEFT: u32 = 105;
    pub const KEY_RIGHT: u32 = 106;
    pub const KEY_DOWN: u32 = 108;
    pub const BTN_LEFT: u32 = 0x110;
    pub const BTN_RIGHT: u32 = 0x111;
    pub const BTN_MIDDLE: u32 = 0x112;
    pub const KEY_BACKSPACE: u32 = 14;
    pub const KEY_TAB: u32 = 15;
    pub const KEY_LEFTCTRL: u32 = 29;
    pub const KEY_LEFTSHIFT: u32 = 42;
    pub const KEY_RIGHTSHIFT: u32 = 54;
    pub const KEY_RIGHTCTRL: u32 = 97;
    pub const KEY_HOME: u32 = 102;
    pub const KEY_END: u32 = 107;
    pub const KEY_DELETE: u32 = 111;

    /// The character a key types on a US keyboard, if it types one.
    pub fn us_char(code: u32, shift: bool) -> Option<char> {
        const ROWS: [(u32, &str, &str); 5] = [
            (2, "1234567890-=", "!@#$%^&*()_+"),
            (16, "qwertyuiop[]", "QWERTYUIOP{}"),
            (30, "asdfghjkl;'`", "ASDFGHJKL:\"~"),
            (43, "\\zxcvbnm,./", "|ZXCVBNM<>?"),
            (57, " ", " "),
        ];
        ROWS.iter().find_map(|&(first, plain, shifted)| {
            let row = if shift { shifted } else { plain };
            row.chars().nth(code.checked_sub(first)? as usize)
        })
    }
}

/// Pointer positions are in pixels relative to the window's top left corner; a window that holds
/// a button down keeps receiving the pointer outside its bounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Key {
        code: u32,
        pressed: bool,
    },
    PointerMove {
        x: i32,
        y: i32,
    },
    Button {
        button: u32,
        pressed: bool,
        x: i32,
        y: i32,
    },
    /// Wheel clicks, positive away from the user.
    Scroll {
        dy: i32,
        x: i32,
        y: i32,
    },
    Focus(bool),
}

const KIND_KEY: u32 = 1;
const KIND_MOVE: u32 = 2;
const KIND_BUTTON: u32 = 3;
const KIND_SCROLL: u32 = 4;
const KIND_FOCUS: u32 = 5;

#[derive(Invariant, Clone, Copy, Default)]
#[repr(C)]
struct RawEvent {
    kind: u32,
    code: u32,
    value: i32,
    x: i32,
    y: i32,
}

impl From<Event> for RawEvent {
    fn from(ev: Event) -> Self {
        let (kind, code, value, x, y) = match ev {
            Event::Key { code, pressed } => (KIND_KEY, code, pressed as i32, 0, 0),
            Event::PointerMove { x, y } => (KIND_MOVE, 0, 0, x, y),
            Event::Button {
                button,
                pressed,
                x,
                y,
            } => (KIND_BUTTON, button, pressed as i32, x, y),
            Event::Scroll { dy, x, y } => (KIND_SCROLL, 0, dy, x, y),
            Event::Focus(focused) => (KIND_FOCUS, 0, focused as i32, 0, 0),
        };
        Self {
            kind,
            code,
            value,
            x,
            y,
        }
    }
}

impl RawEvent {
    fn decode(self) -> Option<Event> {
        let Self {
            kind,
            code,
            value,
            x,
            y,
        } = self;
        Some(match kind {
            KIND_KEY => Event::Key {
                code,
                pressed: value != 0,
            },
            KIND_MOVE => Event::PointerMove { x, y },
            KIND_BUTTON => Event::Button {
                button: code,
                pressed: value != 0,
                x,
                y,
            },
            KIND_SCROLL => Event::Scroll { dy: value, x, y },
            KIND_FOCUS => Event::Focus(value != 0),
            _ => return None,
        })
    }
}

#[derive(Invariant, BaseType)]
#[repr(C)]
pub struct EventQueueBase {
    /// Events ever written; the word a reader sleeps on.
    head: AtomicU64,
    /// Events ever read.
    tail: AtomicU64,
    events: UnsafeCell<[RawEvent; QUEUE_LEN]>,
}

/// One writer (the display server) and one reader (the window's owner).
#[derive(Clone)]
pub struct EventQueue {
    obj: Object<EventQueueBase>,
}

impl EventQueue {
    pub fn create_new() -> Result<Self, TwzError> {
        let obj = ObjectBuilder::default().build_inplace(|obj| {
            obj.write(EventQueueBase {
                head: AtomicU64::new(0),
                tail: AtomicU64::new(0),
                events: UnsafeCell::new([RawEvent::default(); QUEUE_LEN]),
            })
        })?;
        Ok(Self { obj })
    }

    pub fn open(id: ObjID) -> Result<Self, TwzError> {
        let obj = unsafe { Object::map_unchecked(id, MapFlags::READ | MapFlags::WRITE) }?;
        Ok(Self { obj })
    }

    pub fn id(&self) -> ObjID {
        self.obj.id()
    }

    fn slot(&self, n: u64) -> *mut RawEvent {
        let events = self.obj.base().events.get().cast::<RawEvent>();
        unsafe { events.add(n as usize % QUEUE_LEN) }
    }

    /// Append an event, or drop it (returning false) when the reader is a full queue behind.
    /// Calls must not overlap.
    pub fn push(&self, ev: Event) -> bool {
        let base = self.obj.base();
        let head = base.head.load(Ordering::Acquire);
        if head.wrapping_sub(base.tail.load(Ordering::Acquire)) >= QUEUE_LEN as u64 {
            return false;
        }
        unsafe { self.slot(head).write_volatile(ev.into()) };
        base.head.store(head + 1, Ordering::Release);
        let _ = sys_thread_sync(
            &mut [ThreadSync::new_wake(ThreadSyncWake::new(
                ThreadSyncReference::Virtual(&base.head),
                usize::MAX,
            ))],
            None,
        );
        true
    }

    /// The next event, if one is queued.
    pub fn poll(&self) -> Option<Event> {
        let base = self.obj.base();
        loop {
            let tail = base.tail.load(Ordering::Acquire);
            if tail == base.head.load(Ordering::Acquire) {
                return None;
            }
            let raw = unsafe { self.slot(tail).read_volatile() };
            base.tail.store(tail + 1, Ordering::Release);
            if let Some(ev) = raw.decode() {
                return Some(ev);
            }
        }
    }

    /// A sleep that ends when an event arrives, or `None` when one is already queued.
    pub fn waitpoint(&self) -> Option<ThreadSyncSleep> {
        let base = self.obj.base();
        let head = base.head.load(Ordering::Acquire);
        if head != base.tail.load(Ordering::Acquire) {
            return None;
        }
        Some(ThreadSyncSleep::new(
            ThreadSyncReference::Virtual(&base.head),
            head,
            ThreadSyncOp::Equal,
            ThreadSyncFlags::empty(),
        ))
    }

    /// Block until an event arrives or `timeout` has passed.
    pub fn wait_timeout(&self, timeout: Duration) -> Option<Event> {
        let end = Instant::now() + timeout;
        loop {
            if let Some(ev) = self.poll() {
                return Some(ev);
            }
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            if let Some(sleep) = self.waitpoint() {
                let _ = sys_thread_sync(&mut [ThreadSync::new_sleep(sleep)], Some(left));
            }
        }
    }

    /// Block until an event arrives.
    pub fn wait(&self) -> Event {
        loop {
            if let Some(ev) = self.poll() {
                return ev;
            }
            if let Some(sleep) = self.waitpoint() {
                let _ = sys_thread_sync(&mut [ThreadSync::new_sleep(sleep)], None);
            }
        }
    }
}
