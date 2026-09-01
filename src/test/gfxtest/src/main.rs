use std::time::Duration;

use secgate::util::Handle;
use twizzler_display::{Rect, WindowConfig};

static IMG: &'static [u8] = include_bytes!("../img.png");

fn main() {
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
