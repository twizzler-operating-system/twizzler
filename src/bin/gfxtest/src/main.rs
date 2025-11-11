use std::time::Duration;

use pix::{chan::Channel, el::Pixel};
use secgate::util::Handle;
use twizzler_display::WindowConfig;

static IMG: &'static [u8] = include_bytes!("../img.png");

fn main() {
    println!("Hello, world!");

    let decoder = png_pong::Decoder::new(IMG).unwrap();
    let frame = decoder.into_steps().last().unwrap().unwrap();
    let (slice, w, h) = match &frame.raster {
        png_pong::PngRaster::Rgb8(raster) => (raster.pixels(), raster.width(), raster.height()),
        _ => todo!(),
    };

    let mut window =
        twizzler_display::WindowHandle::open(WindowConfig { w, h, x: 0, y: 0 }).unwrap();
    window.window_buffer.fill_current_buffer(|buf, _, _| {
        for y in 0..h {
            for x in 0..w {
                let px = slice[(y * w + x) as usize];
                let r: u8 = px.channels()[0].into();
                let g: u8 = px.channels()[1].into();
                let b: u8 = px.channels()[2].into();
                let r = r as u32;
                let g = g as u32;
                let b = b as u32;
                buf[(y * w + x) as usize] = (r << 16 | g << 8 | b << 0 | 0xff000000);
            }
        }
        //buf.fill(0xffffffff);
    });
    window.window_buffer.flip();
    std::thread::sleep(Duration::from_secs(3));
}
