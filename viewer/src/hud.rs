//! The heads-up display: health, kills and deaths, the weapon, and hit markers.
//!
//! Design: `mockups/hud_mockup_v5.html`. Every number has a label, every bar
//! agrees with its number, and one status line says what the weapon is doing.
//!
//! How it draws: the HUD is a list of coloured triangles, rebuilt every frame
//! and drawn in one pass over the finished picture. Pattern: Immediate Mode UI.
//! There are no widgets and no retained state: `draw` looks at the worm and
//! says what to paint, so the HUD can never disagree with the sim. The only
//! state kept here is the hit marker, because that is an event, not a number.
//!
//! Text is a 5x7 pixel font kept in this file (the project has no asset files).
//! Each lit pixel is a small rectangle, which is plenty for a few dozen letters.
//!
//! Layout is in "virtual pixels": the screen is always 720 units tall and as
//! wide as the window needs. `scale` turns them into real pixels, so the HUD is
//! the same size relative to the window at any resolution.

use crate::renderer::Gpu;
use sim::weapons::Kind;
use sim::world::Worm;
use sim::{Event, MAX_HP, WEAPONS};

type Colour = [f32; 4];

/// A colour from 0xRRGGBB, in the same sRGB values the mockup uses.
fn rgb(hex: u32) -> Colour {
    let c = |shift: u32| ((hex >> shift) & 0xff) as f32 / 255.0;
    [c(16), c(8), c(0), 1.0]
}

const MARGIN: f32 = 28.0;
const SLOT_KEYS: &str = "1234567890-=";

const BRASS: u32 = 0xc99a55;
const BRASS_LIGHT: u32 = 0xe6c07e;
const PLATE: u32 = 0x1a1b10;
const TRACK: u32 = 0x0b0c07;
const TEXT: u32 = 0xf8ecd0;
const MUTED: u32 = 0xd3c59f;
const GOLD: u32 = 0xf4cc60;
const WARN: u32 = 0xf4b043;
const DANGER: u32 = 0xff6a4a;
const READY: u32 = 0xb9dc7a;
const RELOAD: u32 = 0x9ab5d6;
const CHARGE: u32 = 0xffa43c;

const MAX_VERTS: usize = 3 * 10_000;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    /// Clip space, already converted on the CPU, so the shader needs no uniforms.
    pos: [f32; 2],
    colour: Colour,
}

/// The reticle's reply to "you hit something" (or "you killed it").
#[derive(Default)]
struct Marker {
    seconds: f32,
    damage: f32,
    kill: bool,
}

pub struct Hud {
    pipeline: wgpu::RenderPipeline,
    buffer: wgpu::Buffer,
    verts: Vec<Vertex>,
    /// Screen size in pixels and pixels per virtual unit, set by `draw`.
    size: (f32, f32),
    scale: f32,
    marker: Marker,
}

impl Hud {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Hud {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("hud"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("hud"),
            bind_group_layouts: &[],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("hud"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x4],
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("hud vertices"),
            size: (MAX_VERTS * std::mem::size_of::<Vertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Hud { pipeline, buffer, verts: Vec::new(), size: (1.0, 1.0), scale: 1.0, marker: Marker::default() }
    }

    /// Feed it the sim's events. Only the player's own hits matter here.
    pub fn event(&mut self, e: &Event) {
        match *e {
            Event::Hit { victim, damage, .. } if victim != 0 => {
                self.marker = Marker { seconds: 0.3, damage, kill: false };
            }
            // `Died` follows the final `Hit` in the same tick: upgrade the marker.
            Event::Died { victim, killer: 0, .. } if victim != 0 => {
                self.marker.seconds = 0.5;
                self.marker.kill = true;
            }
            _ => {}
        }
    }

    pub fn tick(&mut self, dt: f32) {
        self.marker.seconds = (self.marker.seconds - dt).max(0.0);
    }

    /// Paint the HUD for `worm` over `target` (the finished frame).
    pub fn draw(&mut self, gpu: &Gpu, encoder: &mut wgpu::CommandEncoder, target: &wgpu::TextureView, screen: (u32, u32), worm: &Worm) {
        self.size = (screen.0 as f32, screen.1 as f32);
        self.scale = self.size.1 / 720.0;
        let width = self.size.0 / self.scale;
        self.verts.clear();
        self.kills_and_deaths(worm);
        self.vitals(worm);
        self.weapon(worm, width);
        self.reticle(width / 2.0);

        gpu.queue.write_buffer(&self.buffer, 0, bytemuck::cast_slice(&self.verts));
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("hud"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_vertex_buffer(0, self.buffer.slice(..));
        pass.draw(0..self.verts.len() as u32, 0..1);
    }

    // --- panels ---------------------------------------------------------

    /// Top left: `KILLS 4 | DEATHS 2`.
    fn kills_and_deaths(&mut self, worm: &Worm) {
        let (x, y) = (MARGIN, MARGIN);
        self.plate(x, y, 270.0, 44.0, BRASS);
        let end = self.text("KILLS", x + 16.0, y + 15.0, 2.0, BRASS_LIGHT);
        let end = self.text(&worm.kills.to_string(), end + 8.0, y + 12.0, 3.0, TEXT);
        self.rect(end + 14.0, y + 10.0, 2.0, 24.0, BRASS);
        let end = self.text("DEATHS", end + 30.0, y + 15.0, 2.0, 0xd9a07e);
        self.text(&worm.deaths.to_string(), end + 8.0, y + 12.0, 3.0, TEXT);
    }

    /// Bottom left: `HP 78` and ten segments of 10 HP each.
    fn vitals(&mut self, worm: &Worm) {
        let (x, y) = (MARGIN, 720.0 - MARGIN - 84.0);
        let fraction = (worm.hp / MAX_HP).clamp(0.0, 1.0);
        let low = worm.alive && fraction <= 0.25;
        self.plate(x, y, 280.0, 84.0, if low { DANGER } else { BRASS });
        self.text("HP", x + 16.0, y + 38.0, 2.0, BRASS_LIGHT);
        let colour = if !worm.alive { MUTED } else if low { 0xffb4a2 } else { TEXT };
        self.text(&format!("{:.0}", worm.hp.max(0.0)), x + 46.0, y + 10.0, 6.0, colour);
        let note = if !worm.alive {
            Some((format!("RESPAWN {:.0}", worm.respawn_timer.ceil()), MUTED))
        } else if low {
            Some(("LOW HEALTH".to_string(), DANGER))
        } else {
            None
        };
        if let Some((note, colour)) = note {
            self.text(&note, x + 264.0 - text_width(&note, 2.0), y + 14.0, 2.0, colour);
        }
        let bar = if fraction > 0.5 { GOLD } else if fraction > 0.25 { WARN } else { DANGER };
        self.segments(x + 16.0, y + 60.0, 248.0, 14.0, 10, fraction, bar);
    }

    /// Bottom right: the slot rail above a plate with the name, the ammo and
    /// one status line (ready, reloading, charging or melee).
    fn weapon(&mut self, worm: &Worm, width: f32) {
        let (x, y) = (width - MARGIN - 332.0, 720.0 - MARGIN - 142.0);

        // The rail: one slot per weapon, keyed like the keyboard. A blue mark
        // under a slot means it is reloading, a red one that it is empty.
        for (i, gun) in worm.weapons.iter().enumerate() {
            let def = &WEAPONS[gun.id as usize];
            let sx = x + i as f32 * 28.0;
            let current = i == worm.current;
            self.rect(sx, y - 32.0, 24.0, 24.0, if current { BRASS_LIGHT } else { BRASS });
            self.rect(sx + 1.0, y - 31.0, 22.0, 22.0, if current { BRASS_LIGHT } else { PLATE });
            let key = &SLOT_KEYS[i..i + 1];
            self.text(key, sx + 7.0, y - 27.0, 2.0, if current { PLATE } else { MUTED });
            if gun.reload > 0.0 {
                self.rect(sx + 3.0, y - 12.0, 18.0, 3.0, RELOAD);
            } else if def.mag > 0 && gun.ammo == 0 {
                self.rect(sx + 3.0, y - 12.0, 18.0, 3.0, DANGER);
            }
        }

        let gun = &worm.weapons[worm.current];
        let def = &WEAPONS[gun.id as usize];
        self.plate(x, y, 332.0, 142.0, BRASS);
        self.text(&def.name.to_uppercase(), x + 16.0, y + 12.0, 3.0, TEXT);

        // Ammo: one pip per round up to eight, a bar beyond that.
        if def.kind != Kind::Whip {
            let ammo = gun.ammo as f32;
            let mag = def.mag as f32;
            let colour = if gun.ammo == 0 { DANGER } else if ammo <= mag * 0.25 && def.mag > 1 { WARN } else { GOLD };
            let end = self.text(&gun.ammo.to_string(), x + 16.0, y + 42.0, 5.0, colour);
            self.text(&format!("/ {}", def.mag), end + 8.0, y + 63.0, 2.0, MUTED);
            if def.mag > 8 {
                self.segments(x + 16.0, y + 86.0, 300.0, 12.0, 24, ammo / mag, colour);
            } else {
                let pip = (300.0 - 5.0 * (mag - 1.0)) / mag;
                let pip = pip.min(56.0);
                for i in 0..def.mag {
                    let px = x + 16.0 + i as f32 * (pip + 5.0);
                    self.rect(px, y + 86.0, pip, 12.0, TRACK);
                    if i < gun.ammo {
                        self.rect(px, y + 86.0, pip, 12.0, colour);
                    }
                }
            }
        }

        // The status line: what the weapon is doing right now.
        let charge = worm.charge_fraction();
        let (label, right, colour, progress) = if def.kind == Kind::Whip {
            ("MELEE", String::new(), BRASS_LIGHT, None)
        } else if gun.reload > 0.0 {
            ("RELOADING", format!("{:.1} S", gun.reload), RELOAD, Some(1.0 - gun.reload / def.reload))
        } else if charge > 0.0 {
            ("CHARGING", format!("{:.0}%", charge * 100.0), CHARGE, Some(charge))
        } else {
            ("READY", String::new(), READY, None)
        };
        self.text(label, x + 16.0, y + 106.0, 2.0, colour);
        self.text(&right, x + 316.0 - text_width(&right, 2.0), y + 106.0, 2.0, colour);
        if let Some(progress) = progress {
            self.segments(x + 16.0, y + 126.0, 300.0, 6.0, 1, progress, colour);
        }
    }

    /// The four chevrons around the crosshair, plus the hit marker if one is live.
    fn reticle(&mut self, cx: f32) {
        let cy = 360.0;
        for turn in 0..4 {
            // Rotate the chevron by a quarter turn each time.
            let at = |x: f32, y: f32| match turn {
                0 => (cx + x, cy + y),
                1 => (cx - y, cy + x),
                2 => (cx - x, cy - y),
                _ => (cx + y, cy - x),
            };
            for ((x0, y0), (x1, y1)) in [((-5.0, -22.0), (0.0, -17.0)), ((0.0, -17.0), (5.0, -22.0))] {
                let (a, b) = (at(x0, y0), at(x1, y1));
                self.line(a, b, 4.0, [0.04, 0.035, 0.02, 0.7]);
                self.line(a, b, 2.0, rgb(0xf4dfad));
            }
        }
        if self.marker.seconds > 0.0 {
            let (reach, colour) = if self.marker.kill { (21.0, DANGER) } else { (16.0, 0xffffff) };
            for (sx, sy) in [(1.0, 1.0), (-1.0, 1.0), (1.0, -1.0), (-1.0, -1.0)] {
                let (a, b) = ((cx + sx * 9.0, cy + sy * 9.0), (cx + sx * reach, cy + sy * reach));
                self.line(a, b, 6.0, [0.04, 0.035, 0.02, 0.8]);
                self.line(a, b, 3.0, rgb(colour));
            }
            let damage = format!("{:.0}", self.marker.damage);
            self.text(&damage, cx + 31.0, cy - 35.0, 3.0, 0x0b0905);
            self.text(&damage, cx + 29.0, cy - 37.0, 3.0, if self.marker.kill { 0xff9a7a } else { 0xffffff });
        }
    }

    // --- shapes ---------------------------------------------------------

    /// `segs` equal cells filled left to right up to `fraction`. The last lit
    /// cell may be partial, so the picture and the number never disagree.
    fn segments(&mut self, x: f32, y: f32, w: f32, h: f32, segs: u32, fraction: f32, colour: u32) {
        let gap = if segs > 1 { 2.0 } else { 0.0 };
        let cell = (w - gap * (segs - 1) as f32) / segs as f32;
        for i in 0..segs {
            let cx = x + i as f32 * (cell + gap);
            let lit = (fraction * segs as f32 - i as f32).clamp(0.0, 1.0);
            self.rect(cx, y, cell, h, TRACK);
            self.rect(cx, y, cell * lit, h, colour);
        }
    }

    /// A chamfered plate: a brass edge with a flat dark fill inside.
    fn plate(&mut self, x: f32, y: f32, w: f32, h: f32, edge: u32) {
        self.octagon(x, y, w, h, 18.0, edge);
        self.octagon(x + 2.0, y + 2.0, w - 4.0, h - 4.0, 17.0, PLATE);
    }

    /// A rectangle with its four corners cut by `c`, as a triangle fan.
    fn octagon(&mut self, x: f32, y: f32, w: f32, h: f32, c: f32, colour: u32) {
        let colour = rgb(colour);
        let p = [
            (x + c, y), (x + w - c, y), (x + w, y + c), (x + w, y + h - c),
            (x + w - c, y + h), (x + c, y + h), (x, y + h - c), (x, y + c),
        ];
        let mid = (x + w / 2.0, y + h / 2.0);
        for i in 0..8 {
            self.triangle([mid, p[i], p[(i + 1) % 8]], colour);
        }
    }

    /// An axis-aligned rectangle, snapped to whole pixels so the pixel font stays crisp.
    fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, colour: u32) {
        let s = self.scale;
        let (x0, y0, x1, y1) = ((x * s).round() / s, (y * s).round() / s, ((x + w) * s).round() / s, ((y + h) * s).round() / s);
        let colour = rgb(colour);
        self.triangle([(x0, y0), (x1, y0), (x1, y1)], colour);
        self.triangle([(x0, y0), (x1, y1), (x0, y1)], colour);
    }

    fn line(&mut self, a: (f32, f32), b: (f32, f32), thickness: f32, colour: Colour) {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len = (dx * dx + dy * dy).sqrt();
        let (nx, ny) = (-dy / len * thickness / 2.0, dx / len * thickness / 2.0);
        let (p0, p1, p2, p3) = ((a.0 + nx, a.1 + ny), (a.0 - nx, a.1 - ny), (b.0 - nx, b.1 - ny), (b.0 + nx, b.1 + ny));
        self.triangle([p0, p1, p2], colour);
        self.triangle([p0, p2, p3], colour);
    }

    /// Points are in virtual units; this is the one place they become clip space.
    fn triangle(&mut self, points: [(f32, f32); 3], colour: Colour) {
        if self.verts.len() + 3 > MAX_VERTS {
            return;
        }
        for (x, y) in points {
            let pos = [x * self.scale / self.size.0 * 2.0 - 1.0, 1.0 - y * self.scale / self.size.1 * 2.0];
            self.verts.push(Vertex { pos, colour });
        }
    }

    /// Draws `s` with its top left at `x, y`; `pixel` is the size of one font
    /// pixel. Returns where the text ends, so the next item can follow it.
    fn text(&mut self, s: &str, x: f32, y: f32, pixel: f32, colour: u32) -> f32 {
        for (i, ch) in s.chars().enumerate() {
            for (row, bits) in glyph(ch).iter().enumerate() {
                for col in 0..5 {
                    if bits >> (4 - col) & 1 == 1 {
                        self.rect(x + (i * 6 + col) as f32 * pixel, y + row as f32 * pixel, pixel, pixel, colour);
                    }
                }
            }
        }
        x + text_width(s, pixel)
    }
}

/// Width of `s` in virtual units: five font pixels per letter plus one of space.
fn text_width(s: &str, pixel: f32) -> f32 {
    (s.chars().count() as f32 * 6.0 - 1.0).max(0.0) * pixel
}

fn glyph(ch: char) -> [u8; 7] {
    FONT.iter().find(|(c, _)| *c == ch).map_or([0; 7], |&(_, rows)| rows)
}

/// A 5x7 pixel font: seven rows per letter, the lowest five bits of each row
/// are the pixels (left to right). Only what the HUD says, upper case.
const FONT: [(char, [u8; 7]); 43] = [
    ('A', [0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001]),
    ('B', [0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110]),
    ('C', [0b01110, 0b10001, 0b10000, 0b10000, 0b10000, 0b10001, 0b01110]),
    ('D', [0b11110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11110]),
    ('E', [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111]),
    ('F', [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000]),
    ('G', [0b01110, 0b10001, 0b10000, 0b10111, 0b10001, 0b10001, 0b01111]),
    ('H', [0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001]),
    ('I', [0b01110, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110]),
    ('J', [0b00111, 0b00010, 0b00010, 0b00010, 0b00010, 0b10010, 0b01100]),
    ('K', [0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001]),
    ('L', [0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111]),
    ('M', [0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001]),
    ('N', [0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001, 0b10001]),
    ('O', [0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110]),
    ('P', [0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000]),
    ('Q', [0b01110, 0b10001, 0b10001, 0b10001, 0b10101, 0b10010, 0b01101]),
    ('R', [0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001]),
    ('S', [0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110]),
    ('T', [0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100]),
    ('U', [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110]),
    ('V', [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100]),
    ('W', [0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b10101, 0b01010]),
    ('X', [0b10001, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b10001]),
    ('Y', [0b10001, 0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b00100]),
    ('Z', [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b11111]),
    ('0', [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110]),
    ('1', [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110]),
    ('2', [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111]),
    ('3', [0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110]),
    ('4', [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010]),
    ('5', [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110]),
    ('6', [0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110]),
    ('7', [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000]),
    ('8', [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110]),
    ('9', [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100]),
    ('/', [0b00001, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b10000]),
    ('%', [0b11001, 0b11001, 0b00010, 0b00100, 0b01000, 0b10011, 0b10011]),
    ('.', [0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b01100, 0b01100]),
    ('-', [0b00000, 0b00000, 0b00000, 0b11111, 0b00000, 0b00000, 0b00000]),
    ('=', [0b00000, 0b00000, 0b11111, 0b00000, 0b11111, 0b00000, 0b00000]),
    ('+', [0b00000, 0b00100, 0b00100, 0b11111, 0b00100, 0b00100, 0b00000]),
    (' ', [0; 7]),
];

/// Coloured triangles, nothing else. The target is an sRGB texture, so the
/// hardware encodes on write: the colours (sRGB hex in Rust) go in linear.
const SHADER: &str = r#"
struct Out {
    @builtin(position) pos: vec4<f32>,
    @location(0) colour: vec4<f32>,
};

@vertex
fn vs(@location(0) pos: vec2<f32>, @location(1) colour: vec4<f32>) -> Out {
    return Out(vec4<f32>(pos, 0.0, 1.0), vec4<f32>(pow(colour.rgb, vec3<f32>(2.2)), colour.a));
}

@fragment
fn fs(in: Out) -> @location(0) vec4<f32> {
    return in.colour;
}
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use sim::ARSENAL;

    // A letter missing from the font draws as a blank gap: easy to miss by eye.
    #[test]
    fn the_font_has_every_character_the_hud_can_say() {
        let fixed = "KILLS DEATHS HP LOW HEALTH RESPAWN READY RELOADING CHARGING MELEE 0123456789 /%.-=";
        let names = WEAPONS.iter().map(|w| w.name.to_uppercase()).collect::<Vec<_>>().join(" ");
        for ch in fixed.chars().chain(names.chars()) {
            assert!(FONT.iter().any(|(c, _)| *c == ch), "no glyph for {ch:?}");
        }
    }

    #[test]
    fn the_slot_rail_has_a_key_for_every_weapon() {
        assert_eq!(SLOT_KEYS.len(), ARSENAL.len());
    }
}
