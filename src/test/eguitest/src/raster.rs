//! egui's triangle meshes drawn on the CPU into an XRGB frame.

use std::collections::HashMap;

use egui::{ClippedPrimitive, ImageData, Pos2, TextureId, TexturesDelta, epaint::Primitive};

/// Premultiplied RGBA, as egui's `Color32` stores it.
type Rgba = [u8; 4];

struct Texture {
    size: [usize; 2],
    pixels: Vec<Rgba>,
}

#[derive(Default)]
pub struct Painter {
    textures: HashMap<TextureId, Texture>,
}

fn edge(a: Pos2, b: Pos2, p: Pos2) -> f32 {
    (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x)
}

/// Whether a pixel exactly on the edge from `a` to `b` belongs to this triangle, so that two
/// triangles sharing the edge do not both blend it.
fn owns(a: Pos2, b: Pos2) -> bool {
    let d = b - a;
    d.y > 0.0 || (d.y == 0.0 && d.x < 0.0)
}

fn blend(dst: &mut u32, [r, g, b, a]: Rgba) {
    if a == 255 {
        *dst = 0xff000000 | (r as u32) << 16 | (g as u32) << 8 | b as u32;
        return;
    }
    let keep = 255 - a as u32;
    let over = |src: u8, shift: u32| {
        (src as u32 + ((*dst >> shift & 0xff) * keep + 127) / 255).min(255) << shift
    };
    *dst = 0xff000000 | over(r, 16) | over(g, 8) | over(b, 0);
}

impl Painter {
    pub fn update(&mut self, delta: &TexturesDelta) {
        for (id, change) in &delta.set {
            let ImageData::Color(image) = &change.image;
            let pixels = image.pixels.iter().map(|c| c.to_array());
            match change.pos {
                None => {
                    self.textures.insert(
                        *id,
                        Texture {
                            size: image.size,
                            pixels: pixels.collect(),
                        },
                    );
                }
                Some([x, y]) => {
                    let Some(tex) = self.textures.get_mut(id) else {
                        continue;
                    };
                    for (i, px) in pixels.enumerate() {
                        let at = (y + i / image.size[0]) * tex.size[0] + x + i % image.size[0];
                        tex.pixels[at] = px;
                    }
                }
            }
        }
        for id in &delta.free {
            self.textures.remove(id);
        }
    }

    pub fn paint(
        &self,
        frame: &mut [u32],
        width: usize,
        height: usize,
        primitives: &[ClippedPrimitive],
        pixels_per_point: f32,
    ) {
        for clipped in primitives {
            let Primitive::Mesh(mesh) = &clipped.primitive else {
                continue;
            };
            let Some(tex) = self.textures.get(&mesh.texture_id) else {
                continue;
            };
            let clip = clipped.clip_rect * pixels_per_point;
            let bound = |v: f32, max: usize| v.round().clamp(0.0, max as f32) as usize;
            let (clip_x, clip_y) = (
                bound(clip.min.x, width)..bound(clip.max.x, width),
                bound(clip.min.y, height)..bound(clip.max.y, height),
            );
            for tri in mesh.indices.chunks_exact(3) {
                let mut v = [0, 1, 2].map(|i| mesh.vertices[tri[i] as usize]);
                let pos = |v: &egui::epaint::Vertex| (v.pos.to_vec2() * pixels_per_point).to_pos2();
                let mut area = edge(pos(&v[0]), pos(&v[1]), pos(&v[2]));
                if area == 0.0 {
                    continue;
                }
                if area < 0.0 {
                    v.swap(1, 2);
                    area = -area;
                }
                let p = [pos(&v[0]), pos(&v[1]), pos(&v[2])];
                let min = |f: fn(Pos2) -> f32| p.iter().map(|p| f(*p)).fold(f32::MAX, f32::min);
                let max = |f: fn(Pos2) -> f32| p.iter().map(|p| f(*p)).fold(f32::MIN, f32::max);
                let xs = (min(|p| p.x).floor().max(clip_x.start as f32) as usize)
                    ..(max(|p| p.x).ceil().min(clip_x.end as f32) as usize);
                let ys = (min(|p| p.y).floor().max(clip_y.start as f32) as usize)
                    ..(max(|p| p.y).ceil().min(clip_y.end as f32) as usize);
                let owned = [owns(p[1], p[2]), owns(p[2], p[0]), owns(p[0], p[1])];
                let flat = v[0].color == v[1].color
                    && v[0].color == v[2].color
                    && v[0].uv == v[1].uv
                    && v[0].uv == v[2].uv;
                let texel = |u: f32, t: f32| {
                    let at = |f: f32, size: usize| ((f * size as f32) as usize).min(size - 1);
                    tex.pixels[at(t, tex.size[1]) * tex.size[0] + at(u, tex.size[0])]
                };
                let shade = |w: [f32; 3]| -> Rgba {
                    let mix = |f: fn(&egui::epaint::Vertex) -> f32| {
                        w[0] * f(&v[0]) + w[1] * f(&v[1]) + w[2] * f(&v[2])
                    };
                    let t = texel(mix(|v| v.uv.x), mix(|v| v.uv.y));
                    let c = [
                        mix(|v| v.color.r() as f32),
                        mix(|v| v.color.g() as f32),
                        mix(|v| v.color.b() as f32),
                        mix(|v| v.color.a() as f32),
                    ];
                    [0, 1, 2, 3].map(|i| (c[i] * t[i] as f32 / 255.0 + 0.5) as u8)
                };
                let solid = flat.then(|| shade([1.0, 0.0, 0.0]));
                for y in ys {
                    for x in xs.clone() {
                        let at = Pos2::new(x as f32 + 0.5, y as f32 + 0.5);
                        let w = [
                            edge(p[1], p[2], at),
                            edge(p[2], p[0], at),
                            edge(p[0], p[1], at),
                        ];
                        if (0..3).any(|i| w[i] < 0.0 || (w[i] == 0.0 && !owned[i])) {
                            continue;
                        }
                        let src = solid.unwrap_or_else(|| shade(w.map(|w| w / area)));
                        if src[3] != 0 || src[0] != 0 || src[1] != 0 || src[2] != 0 {
                            blend(&mut frame[y * width + x], src);
                        }
                    }
                }
            }
        }
    }
}
