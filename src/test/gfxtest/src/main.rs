use std::time::Duration;

use secgate::util::Handle;
use twizzler_display::{BufferMut, Rect, WindowConfig, WindowHandle};

static IMG: &'static [u8] = include_bytes!("../img.png");

const BLOCK: u32 = 0xffff00ff;

fn pixel(x: u32, y: u32, w: u32, h: u32, shift: u32) -> u32 {
    let (px, py) = (x + shift, y + shift);
    if x == 0 {
        0xff0000ff
    } else if x == w - 1 {
        0xffffff00
    } else if y == 0 {
        0xffff0000
    } else if y == h - 1 {
        0xff00ff00
    } else {
        0xff000000 | (px & 0xff) << 16 | (py & 0xff) << 8 | ((px ^ py) & 0xff)
    }
}

/// Fill the part of `r` inside the buffer with `color`, or with the pattern.
fn fill(buf: &mut BufferMut, w: u32, h: u32, r: Rect, color: Option<u32>) {
    for y in r.y.min(h)..r.y.saturating_add(r.h).min(h) {
        for x in r.x.min(w)..r.x.saturating_add(r.w).min(w) {
            buf[(y * w + x) as usize] = color.unwrap_or_else(|| pixel(x, y, w, h, 0));
        }
    }
}

fn open(args: &[u32], z: u32) -> WindowHandle {
    let config = match args {
        &[x, y, w, h] => WindowConfig { w, h, x, y, z },
        _ => twizzler_display::get_display_info().unwrap(),
    };
    WindowHandle::open(config).unwrap()
}

/// Flip `frames` full frames of `color`, or of the pattern.
fn full(window: &WindowHandle, frames: u32, color: Option<u32>) {
    for _ in 0..frames {
        window.window_buffer.update_buffer(|mut buf, w, h| {
            fill(&mut buf, w, h, Rect::full(), color);
            buf.damage(Rect::full());
        });
        window.window_buffer.flip();
    }
}

/// Flip one frame that draws each block and damages exactly `damage`.
fn blocks(window: &WindowHandle, blocks: &[Rect], damage: &[Rect]) {
    window.window_buffer.update_buffer(|mut buf, w, h| {
        for b in blocks {
            fill(&mut buf, w, h, *b, Some(BLOCK));
        }
        for d in damage {
            buf.damage(*d);
        }
    });
    window.window_buffer.flip();
}

fn hold(name: &str) {
    println!("gfxtest: {name} held");
    std::thread::sleep(Duration::from_secs(10));
}

/// `gfxtest pattern [x y w h]`: a window (full screen by default) showing a pattern that
/// `screencheck.py` can check pixel for pixel.
fn pattern(args: &[u32]) {
    let window = open(args, 0);
    let draw = |shift: u32| {
        window.window_buffer.update_buffer(|mut buf, w, h| {
            for y in 0..h {
                for x in 0..w {
                    buf[(y * w + x) as usize] = pixel(x, y, w, h, shift);
                }
            }
            buf.damage(Rect::full());
        });
    };
    // Every frame differs from the one held, so one buffer showing through the other is visible.
    for shift in [111, 74, 37, 0] {
        draw(shift);
        window.window_buffer.flip();
    }
    // A next frame that is drawn but never flipped must not reach the screen.
    draw(200);
    hold("pattern");
}

/// A block moves over the pattern. Each frame draws only the block and the spot it left, and
/// damages a larger area around both, relying on the buffer still holding the previous frame.
fn partial(args: &[u32]) {
    let window = open(args, 0);
    full(&window, 1, None);
    let pos = |i: u32| Rect::new(40 + 24 * i, 50 + 16 * i, 32, 32);
    for i in 1..=5 {
        window.window_buffer.update_buffer(|mut buf, w, h| {
            fill(&mut buf, w, h, pos(i - 1), None);
            fill(&mut buf, w, h, pos(i), Some(BLOCK));
            buf.damage(Rect::new(pos(i).x - 40, pos(i).y - 32, 88, 80));
        });
        window.window_buffer.flip();
    }
    hold("partial");
}

/// More damage rects in one frame than a buffer records separately.
fn rects(args: &[u32]) {
    let window = open(args, 0);
    full(&window, 2, None);
    let rects: Vec<Rect> = (0..12)
        .map(|i| Rect::new(20 + 40 * i, 30 + 25 * i, 16, 16))
        .collect();
    blocks(&window, &rects, &rects);
    hold("rects");
}

/// Damage partly and wholly outside the window.
fn clip(args: &[u32]) {
    let window = open(args, 0);
    full(&window, 2, None);
    let WindowConfig { w, h, .. } = window.get_config().unwrap();
    let rects = [Rect::new(w - 20, h - 20, 40, 40), Rect::new(60, 70, 20, 20)];
    let outside = [Rect::new(w + 5, 3, 10, 10), Rect::new(3, h + 100, 10, 10)];
    blocks(
        &window,
        &rects,
        &[rects[0], outside[0], outside[1], rects[1]],
    );
    hold("clip");
}

/// Partial updates in a window under another one, which is then dropped.
fn overlap(args: &[u32]) {
    let WindowConfig { x, y, w, h, .. } = match args {
        &[x, y, w, h] => WindowConfig { w, h, x, y, z: 0 },
        _ => twizzler_display::get_display_info().unwrap(),
    };
    let upper = open(&[x + w / 4, y + h / 4, w / 2, h / 2], 1);
    let lower = open(&[x, y, w, h], 0);
    full(&upper, 1, Some(0xff2060a0));
    full(&lower, 2, None);
    let rects = [
        Rect::new(w / 2, h / 2, 16, 16),
        Rect::new(w / 4 - 8, h / 2, 16, 16),
        Rect::new(10, 10, 16, 16),
    ];
    blocks(&lower, &rects, &rects);
    hold("overlap");
    drop(upper);
    hold("overlap-dropped");
}

/// Move and shrink a window, redrawing as the new size arrives.
fn reconf(args: &[u32]) {
    let window = open(args, 0);
    full(&window, 2, None);
    let WindowConfig { x, y, w, h, z } = window.get_config().unwrap();
    // Let the last frame be composited first, so the move is a pass of its own.
    std::thread::sleep(Duration::from_millis(200));
    window
        .reconfigure(WindowConfig {
            x: x + 40,
            y: y + 30,
            w: w / 2,
            h: h / 2,
            z,
        })
        .unwrap();
    full(&window, 4, None);
    hold("reconf");
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let modes: [(&str, fn(&[u32])); 6] = [
        ("pattern", pattern),
        ("partial", partial),
        ("rects", rects),
        ("clip", clip),
        ("overlap", overlap),
        ("reconf", reconf),
    ];
    if let Some((_, mode)) = modes.iter().find(|m| args.get(1).is_some_and(|a| a == m.0)) {
        let nums: Vec<u32> = args[2..].iter().map(|a| a.parse().unwrap()).collect();
        return mode(&nums);
    }

    let image = image::ImageReader::new(std::io::Cursor::new(IMG))
        .with_guessed_format()
        .unwrap()
        .decode()
        .unwrap();
    let pixels = image.as_rgb8().unwrap();

    let window = WindowHandle::open(WindowConfig {
        w: pixels.width(),
        h: pixels.height(),
        x: 0,
        y: 0,
        z: 0,
    })
    .unwrap();
    window.window_buffer.update_buffer(|mut buf, _, _| {
        for y in 0..pixels.height() {
            for x in 0..pixels.width() {
                let px = pixels[(x, y)];
                let r = px[0] as u32;
                let g = px[1] as u32;
                let b = px[2] as u32;
                let r = r as u32;
                let g = g as u32;
                let b = b as u32;
                buf[(y * pixels.width() + x) as usize] = r << 16 | g << 8 | b << 0 | 0xff000000;
            }
        }
        buf.damage(Rect::full());
    });
    window.window_buffer.flip();
    std::thread::sleep(Duration::from_secs(3));
}
