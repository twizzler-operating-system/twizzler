//! Keyboard and pointer input from virtio-input devices, routed to windows.

use std::sync::mpsc::Receiver;

use secgate::util::HandleMgr;
use twizzler_abi::object::ObjID;
use twizzler_display::{Event, Rect};
use virtio_gpu::{InputDevice, InputEvent};

use crate::{DisplayClient, DisplayInfo};

const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;
const EV_REL: u16 = 2;
const EV_ABS: u16 = 3;
const REL_X: u16 = 0;
const REL_Y: u16 = 1;
const REL_WHEEL: u16 = 8;
const ABS_X: u16 = 0;
const ABS_Y: u16 = 1;
/// Key codes from here up are buttons.
const BTN_MISC: u16 = 0x100;
const BTN_GEAR_DOWN: u16 = 0x150;
const BTN_GEAR_UP: u16 = 0x151;

/// Compartment and handle, as `HandleMgr` keys a window.
pub(crate) type WindowId = (ObjID, u32);

/// Locked after `DisplayInfo::handles`, never before.
#[derive(Default)]
pub(crate) struct InputState {
    /// Where the pointer is on screen, once a pointing device has said.
    pub pointer: Option<(u32, u32)>,
    buttons_down: u32,
    /// The window that got the first button press, until every button is up.
    grab: Option<WindowId>,
    focus: Option<WindowId>,
}

type Handles = HandleMgr<DisplayClient>;

fn window_at(handles: &Handles, x: u32, y: u32) -> Option<WindowId> {
    let at = Rect::new(x, y, 1, 1);
    handles
        .handles()
        .filter(|h| {
            at.intersect(&Rect::from(*h.2.config.read().unwrap()))
                .is_some()
        })
        // The compositor draws equal z in this order, so the last is on top.
        .max_by_key(|h| h.2.config.read().unwrap().z)
        .map(|h| (h.0, h.1))
}

fn send(handles: &Handles, to: Option<WindowId>, ev: impl FnOnce(i32, i32) -> Event) {
    let Some(client) = to.and_then(|(comp, handle)| handles.lookup(comp, handle)) else {
        return;
    };
    let config = *client.config.read().unwrap();
    client.events.push(ev(config.x as i32, config.y as i32));
}

impl InputState {
    pub(crate) fn set_focus(&mut self, handles: &Handles, to: Option<WindowId>) {
        if self.focus == to {
            return;
        }
        send(handles, self.focus, |_, _| Event::Focus(false));
        send(handles, to, |_, _| Event::Focus(true));
        self.focus = to;
    }

    pub(crate) fn window_dropped(&mut self, id: WindowId) {
        if self.focus == Some(id) {
            self.focus = None;
        }
        if self.grab == Some(id) {
            self.grab = None;
        }
    }

    fn target(&self, handles: &Handles) -> Option<WindowId> {
        let (x, y) = self.pointer?;
        self.grab.or_else(|| window_at(handles, x, y))
    }
}

impl DisplayInfo {
    fn pointer_moved(&self, x: u32, y: u32) {
        {
            let handles = self.handles.lock().unwrap();
            let mut input = self.input.lock().unwrap();
            if input.pointer == Some((x, y)) {
                return;
            }
            input.pointer = Some((x, y));
            send(&handles, input.target(&handles), |wx, wy| {
                Event::PointerMove {
                    x: x as i32 - wx,
                    y: y as i32 - wy,
                }
            });
        }
        // The compositor moves the cursor.
        self.windows_changed();
    }

    fn button(&self, button: u32, pressed: bool) {
        let handles = self.handles.lock().unwrap();
        let mut input = self.input.lock().unwrap();
        let Some((x, y)) = input.pointer else {
            return;
        };
        let target = input.target(&handles);
        if pressed {
            input.buttons_down += 1;
            input.grab = target;
            if target.is_some() {
                input.set_focus(&handles, target);
            }
        } else {
            input.buttons_down = input.buttons_down.saturating_sub(1);
            if input.buttons_down == 0 {
                input.grab = None;
            }
        }
        send(&handles, target, |wx, wy| Event::Button {
            button,
            pressed,
            x: x as i32 - wx,
            y: y as i32 - wy,
        });
    }

    fn scroll(&self, dy: i32) {
        let handles = self.handles.lock().unwrap();
        let input = self.input.lock().unwrap();
        let Some((x, y)) = input.pointer else {
            return;
        };
        send(&handles, input.target(&handles), |wx, wy| Event::Scroll {
            dy,
            x: x as i32 - wx,
            y: y as i32 - wy,
        });
    }

    fn key(&self, code: u32, pressed: bool) {
        let handles = self.handles.lock().unwrap();
        let input = self.input.lock().unwrap();
        send(&handles, input.focus, |_, _| Event::Key { code, pressed });
    }
}

/// One device's part of a report, applied at its `EV_SYN`.
struct Source {
    dev: InputDevice,
    abs_max: (u32, u32),
    abs: (Option<u32>, Option<u32>),
    rel: (i32, i32),
    wheel: i32,
}

fn scale(value: u32, max: u32, size: u32) -> u32 {
    let max = if max == 0 { 0x7fff } else { max } as u64;
    ((value as u64 * size as u64 / (max + 1)) as u32).min(size.saturating_sub(1))
}

impl Source {
    fn new(mut dev: InputDevice) -> Self {
        let max = |dev: &mut InputDevice, axis| dev.abs_info(axis).map(|a| a.max).unwrap_or(0);
        let abs_max = (max(&mut dev, ABS_X as u8), max(&mut dev, ABS_Y as u8));
        Self {
            dev,
            abs_max,
            abs: (None, None),
            rel: (0, 0),
            wheel: 0,
        }
    }

    fn flush(&mut self, info: &DisplayInfo) {
        let moved = self.abs != (None, None) || self.rel != (0, 0);
        if moved {
            let (x, y) = info.input.lock().unwrap().pointer.unwrap_or((0, 0));
            let step = |at: u32, d: i32, size: u32| {
                (at as i64 + d as i64).clamp(0, size as i64 - 1) as u32
            };
            let x = self
                .abs
                .0
                .unwrap_or_else(|| step(x, self.rel.0, info.width));
            let y = self
                .abs
                .1
                .unwrap_or_else(|| step(y, self.rel.1, info.height));
            info.pointer_moved(x, y);
        }
        if self.wheel != 0 {
            info.scroll(self.wheel);
        }
        self.abs = (None, None);
        self.rel = (0, 0);
        self.wheel = 0;
    }

    fn handle(&mut self, info: &DisplayInfo, ev: InputEvent) {
        let value = ev.value;
        match (ev.event_type, ev.code) {
            (EV_ABS, ABS_X) => self.abs.0 = Some(scale(value, self.abs_max.0, info.width)),
            (EV_ABS, ABS_Y) => self.abs.1 = Some(scale(value, self.abs_max.1, info.height)),
            (EV_REL, REL_X) => self.rel.0 += value as i32,
            (EV_REL, REL_Y) => self.rel.1 += value as i32,
            (EV_REL, REL_WHEEL) => self.wheel += value as i32,
            // QEMU follows each wheel click with a release of one of these.
            (EV_KEY, BTN_GEAR_DOWN | BTN_GEAR_UP) => {}
            (EV_KEY, code) if code >= BTN_MISC => {
                // A press belongs where the pointer is now, not where the last report left it.
                self.flush(info);
                info.button(code as u32, value != 0);
            }
            (EV_KEY, code) => info.key(code as u32, value != 0),
            (EV_SYN, _) => self.flush(info),
            _ => {}
        }
    }
}

pub(crate) fn input_thread(
    info: &'static DisplayInfo,
    devices: Vec<InputDevice>,
    wake: Receiver<Option<()>>,
) {
    let mut sources: Vec<Source> = devices.into_iter().map(Source::new).collect();
    loop {
        for source in &mut sources {
            source.dev.ack_interrupt();
            while let Some(ev) = source.dev.pop_pending_event() {
                source.handle(info, ev);
            }
        }
        if wake.recv().is_err() {
            return;
        }
    }
}

const CURSOR: [&[u8; 12]; 18] = [
    b"X           ",
    b"XX          ",
    b"X.X         ",
    b"X..X        ",
    b"X...X       ",
    b"X....X      ",
    b"X.....X     ",
    b"X......X    ",
    b"X.......X   ",
    b"X........X  ",
    b"X.........X ",
    b"X......XXXXX",
    b"X...X..X    ",
    b"X..X X..X   ",
    b"X.X  X..X   ",
    b"XX    X..X  ",
    b"X     X..X  ",
    b"       XX   ",
];

pub(crate) fn cursor_rect((x, y): (u32, u32)) -> Rect {
    Rect::new(x, y, CURSOR[0].len() as u32, CURSOR.len() as u32)
}

/// Draw the cursor with its tip at `at`, only where `damage` says the frame is being redrawn.
pub(crate) fn draw_cursor(fb: &mut [u32], fbw: u32, fbh: u32, at: (u32, u32), damage: &[Rect]) {
    for (row, line) in CURSOR.iter().enumerate() {
        for (col, ch) in line.iter().enumerate() {
            let (x, y) = (at.0 + col as u32, at.1 + row as u32);
            let color = match ch {
                b'X' => 0xff000000,
                b'.' => 0xffffffff,
                _ => continue,
            };
            let px = Rect::new(x, y, 1, 1);
            if x < fbw && y < fbh && damage.iter().any(|d| px.intersect(d).is_some()) {
                fb[(y * fbw + x) as usize] = color;
            }
        }
    }
}
