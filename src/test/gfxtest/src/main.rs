use std::time::Duration;

use secgate::util::Handle;
use twizzler_display::{Rect, WindowConfig};

static IMG: &'static [u8] = include_bytes!("../img.png");

/// `gfxtest pattern [x y w h]`: a window (full screen by default) showing a pattern that
/// `screencheck.py` can check pixel for pixel.
fn pattern(args: &[u32]) {
    let config = match args {
        &[x, y, w, h] => WindowConfig { w, h, x, y, z: 0 },
        _ => twizzler_display::get_display_info().unwrap(),
    };
    let window = twizzler_display::WindowHandle::open(config).unwrap();
    let draw = |shift: u32| {
        window.window_buffer.update_buffer(|mut buf, w, h| {
            for y in 0..h {
                for x in 0..w {
                    let (px, py) = (x + shift, y + shift);
                    buf[(y * w + x) as usize] = if x == 0 {
                        0xff0000ff
                    } else if x == w - 1 {
                        0xffffff00
                    } else if y == 0 {
                        0xffff0000
                    } else if y == h - 1 {
                        0xff00ff00
                    } else {
                        0xff000000 | (px & 0xff) << 16 | (py & 0xff) << 8 | ((px ^ py) & 0xff)
                    };
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
    println!("gfxtest: pattern held");
    std::thread::sleep(Duration::from_secs(10));
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).is_some_and(|a| a == "pattern") {
        let nums: Vec<u32> = args[2..].iter().map(|a| a.parse().unwrap()).collect();
        return pattern(&nums);
    }

    let image = image::ImageReader::new(std::io::Cursor::new(IMG))
        .with_guessed_format()
        .unwrap()
        .decode()
        .unwrap();
    let pixels = image.as_rgb8().unwrap();

    let window = twizzler_display::WindowHandle::open(WindowConfig {
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
