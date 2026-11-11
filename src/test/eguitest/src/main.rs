//! An egui window rendered on the CPU, driven by the display server's input events.
//!
//! `eguitest [x y w h]` (full screen by default). Prints where its widgets are and each change
//! of state, so a host script can click and type at it. Escape ends it.

use std::time::{Duration, Instant};

use secgate::util::Handle;
use twizzler_display::{Event, EventQueue, Rect, WindowConfig, WindowHandle, codes};

mod raster;

#[derive(Default, PartialEq, Clone)]
struct State {
    clicks: u32,
    text: String,
    checked: bool,
    level: f32,
}

/// Turns display events into egui's.
struct Input {
    pointer: egui::Pos2,
    modifiers: egui::Modifiers,
    quit: bool,
}

fn key(code: u32) -> Option<egui::Key> {
    use egui::Key;
    Some(match code {
        codes::KEY_ESC => Key::Escape,
        codes::KEY_ENTER => Key::Enter,
        codes::KEY_TAB => Key::Tab,
        codes::KEY_BACKSPACE => Key::Backspace,
        codes::KEY_DELETE => Key::Delete,
        codes::KEY_HOME => Key::Home,
        codes::KEY_END => Key::End,
        codes::KEY_LEFT => Key::ArrowLeft,
        codes::KEY_RIGHT => Key::ArrowRight,
        codes::KEY_UP => Key::ArrowUp,
        codes::KEY_DOWN => Key::ArrowDown,
        codes::KEY_SPACE => Key::Space,
        _ => Key::from_name(&codes::us_char(code, true)?.to_string())?,
    })
}

impl Input {
    fn translate(&mut self, ev: Event, out: &mut Vec<egui::Event>) {
        let modifiers = self.modifiers;
        match ev {
            Event::PointerMove { x, y } => {
                self.pointer = egui::pos2(x as f32, y as f32);
                out.push(egui::Event::PointerMoved(self.pointer));
            }
            Event::Button {
                button,
                pressed,
                x,
                y,
            } => {
                self.pointer = egui::pos2(x as f32, y as f32);
                let button = match button {
                    codes::BTN_LEFT => egui::PointerButton::Primary,
                    codes::BTN_RIGHT => egui::PointerButton::Secondary,
                    codes::BTN_MIDDLE => egui::PointerButton::Middle,
                    _ => return,
                };
                out.push(egui::Event::PointerButton {
                    pos: self.pointer,
                    button,
                    pressed,
                    modifiers,
                });
            }
            Event::Scroll { dy, .. } => out.push(egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Line,
                delta: egui::vec2(0.0, dy as f32),
                modifiers,
            }),
            Event::Key { code, pressed } => {
                match code {
                    codes::KEY_LEFTSHIFT | codes::KEY_RIGHTSHIFT => self.modifiers.shift = pressed,
                    codes::KEY_LEFTCTRL | codes::KEY_RIGHTCTRL => {
                        self.modifiers.ctrl = pressed;
                        self.modifiers.command = pressed;
                    }
                    codes::KEY_ESC if !pressed => self.quit = true,
                    _ => {}
                }
                if let Some(key) = key(code) {
                    out.push(egui::Event::Key {
                        key,
                        physical_key: None,
                        pressed,
                        repeat: false,
                        modifiers,
                    });
                }
                if let (true, false, Some(c)) = (
                    pressed,
                    modifiers.ctrl,
                    codes::us_char(code, modifiers.shift),
                ) {
                    out.push(egui::Event::Text(c.to_string()));
                }
            }
            Event::Focus(focused) => out.push(egui::Event::WindowFocused(focused)),
        }
    }
}

fn ui(ctx: &egui::Context, state: &mut State, places: &mut Vec<(&'static str, egui::Rect)>) {
    egui::CentralPanel::default().show(ctx, |ui| {
        ui.heading("egui on Twizzler");
        ui.label("Rendered on the CPU into a twizzler-display window.");
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let button = ui.button("Click me");
            if button.clicked() {
                state.clicks += 1;
            }
            places.push(("button", button.rect));
            ui.label(format!("clicked {} times", state.clicks));
        });
        ui.horizontal(|ui| {
            ui.label("Type here:");
            places.push(("text", ui.text_edit_singleline(&mut state.text).rect));
        });
        places.push((
            "checkbox",
            ui.checkbox(&mut state.checked, "A checkbox").rect,
        ));
        places.push((
            "slider",
            ui.add(egui::Slider::new(&mut state.level, 0.0..=100.0).text("level"))
                .rect,
        ));
        ui.add(egui::ProgressBar::new(state.level / 100.0).show_percentage());
    });
}

fn main() {
    let args: Vec<u32> = std::env::args()
        .skip(1)
        .map(|a| a.parse().expect("eguitest [x y w h]"))
        .collect();
    let config = match args[..] {
        [x, y, w, h] => WindowConfig { w, h, x, y, z: 0 },
        _ => twizzler_display::get_display_info().unwrap(),
    };
    let window = WindowHandle::open(config).unwrap();
    let events: EventQueue = window.events().unwrap();

    let ctx = egui::Context::default();
    let mut painter = raster::Painter::default();
    let mut input = Input {
        pointer: egui::Pos2::ZERO,
        modifiers: egui::Modifiers::default(),
        quit: false,
    };
    let mut state = State::default();
    let (mut shown, mut placed) = (State::default(), Vec::new());
    let start = Instant::now();
    let mut pending = Vec::new();
    let (mut frames, mut render_time) = (0u32, Duration::ZERO);
    println!("eguitest: ready {}x{}", config.w, config.h);

    while !input.quit {
        let began = Instant::now();
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(config.w as f32, config.h as f32),
            )),
            time: Some(start.elapsed().as_secs_f64()),
            modifiers: input.modifiers,
            events: std::mem::take(&mut pending),
            focused: true,
            ..Default::default()
        };
        let mut places = Vec::new();
        let out = ctx.run(raw, |ctx| ui(ctx, &mut state, &mut places));
        let primitives = ctx.tessellate(out.shapes, out.pixels_per_point);
        painter.update(&out.textures_delta);
        window.window_buffer.update_buffer(|mut buf, w, h| {
            let pixels = buf.as_slice_mut();
            pixels.fill(0xff000000);
            painter.paint(
                pixels,
                w as usize,
                h as usize,
                &primitives,
                out.pixels_per_point,
            );
            buf.damage(Rect::full());
        });
        window.window_buffer.flip();
        frames += 1;
        render_time += began.elapsed();

        if places != placed {
            for (name, rect) in &places {
                let c = rect.center();
                println!("eguitest: {name} at {} {}", c.x as i32, c.y as i32);
            }
            placed = places;
        }
        if state != shown {
            println!(
                "eguitest: clicks {} checked {} level {} text {:?}",
                state.clicks, state.checked, state.level as i32, state.text
            );
            shown = state.clone();
        }

        let repaint = out
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .map_or(Duration::MAX, |v| v.repaint_delay);
        // Draw again at once when egui asks, else when input arrives or its timer ends.
        if !repaint.is_zero() {
            let wait = repaint.min(Duration::from_secs(3600));
            if let Some(ev) = events.wait_timeout(wait) {
                input.translate(ev, &mut pending);
            }
        }
        while let Some(ev) = events.poll() {
            input.translate(ev, &mut pending);
        }
    }
    println!(
        "eguitest: done, {frames} frames, {} us each",
        render_time.as_micros() / frames.max(1) as u128
    );
}
