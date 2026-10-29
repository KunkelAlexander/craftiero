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
const SLOT_KEYS: &str = "1234567890-=[]\\";

// Cel shading, like the world: every shape is one flat colour with a single
// hard shadow band along its bottom (`shade`) and a soft dark-brown ink line,
// in an earthy palette that sits next to the terrain instead of shouting over it.
const INK: u32 = 0x1e1a14;
const TEXT: u32 = 0xf4ecd8;
const TRACK: u32 = 0x4a3f33;
const GOLD: u32 = 0xe6c75a;
const WARN: u32 = 0xd98a3a;
const DANGER: u32 = 0xd9684c;
const PANEL_RED: u32 = 0xb5543f;
const PANEL_GREY: u32 = 0x8a877c;
const PANEL_ORANGE: u32 = 0xc58a3e;
const READY: u32 = 0xdcecc0;
const RELOAD: u32 = 0xd2e2ee;

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
    kill: bool,
    /// Damage summed over a run of hits: each hit within `COMBO_WINDOW` of the
    /// last adds to it, so a shotgun blast or a burst reads as one number.
    total: f32,
    total_seconds: f32,
}

const COMBO_WINDOW: f32 = 1.0;

/// Damage the player took, summed the same way and shown as "-35" under the reticle.
#[derive(Default)]
struct Taken {
    total: f32,
    seconds: f32,
}

pub struct Hud {
    pipeline: wgpu::RenderPipeline,
    buffer: wgpu::Buffer,
    verts: Vec<Vertex>,
    /// Screen size in pixels and pixels per virtual unit, set by `draw`.
    size: (f32, f32),
    scale: f32,
    marker: Marker,
    taken: Taken,
    /// The HP number bounces whenever the value changes (hit, heal, respawn).
    hp_seen: f32,
    hp_age: f32,
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
        Hud { pipeline, buffer, verts: Vec::new(), size: (1.0, 1.0), scale: 1.0, marker: Marker::default(),
            taken: Taken::default(),
            hp_seen: MAX_HP,
            hp_age: 10.0,
        }
    }

    /// Feed it the sim's events. Only hits dealt or taken by the player matter
    /// here; `me` is the player's worm.
    pub fn event(&mut self, e: &Event, me: u8) {
        match *e {
            Event::Hit { victim, damage, .. } if victim == me => {
                let before = if self.taken.seconds > 0.0 { self.taken.total } else { 0.0 };
                self.taken = Taken { total: before + damage, seconds: COMBO_WINDOW };
            }
            Event::Hit { victim, damage, .. } if victim != me => {
                let total = if self.marker.total_seconds > 0.0 { self.marker.total + damage } else { damage };
                self.marker = Marker { seconds: 0.3, kill: false, total, total_seconds: COMBO_WINDOW };
            }
            // `Died` follows the final `Hit` in the same tick: upgrade the marker.
            Event::Died { victim, killer, .. } if killer == me && victim != me => {
                self.marker.seconds = 0.5;
                self.marker.kill = true;
                self.marker.total_seconds = COMBO_WINDOW;
            }
            _ => {}
        }
    }

    pub fn tick(&mut self, dt: f32) {
        self.marker.seconds = (self.marker.seconds - dt).max(0.0);
        self.marker.total_seconds = (self.marker.total_seconds - dt).max(0.0);
        self.taken.seconds = (self.taken.seconds - dt).max(0.0);
        self.hp_age = (self.hp_age + dt).min(10.0);
    }

    /// Paint the HUD for `worm` over `target` (the finished frame).
    pub fn draw(&mut self, gpu: &Gpu, encoder: &mut wgpu::CommandEncoder, target: &wgpu::TextureView, screen: (u32, u32), worm: &Worm) {
        self.size = (screen.0 as f32, screen.1 as f32);
        self.scale = self.size.1 / 720.0;
        let width = self.size.0 / self.scale;
        self.verts.clear();
        let hp = worm.hp.round();
        if hp != self.hp_seen {
            self.hp_seen = hp;
            self.hp_age = 0.0;
        }
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
        self.plate(x, y, 270.0, 44.0, PANEL_ORANGE);
        let end = self.label("KILLS", x + 16.0, y + 15.0, TEXT);
        let end = self.fat(&worm.kills.to_string(), end + 10.0, y + 12.0, 3.0, TEXT);
        self.rect(end + 14.0, y + 10.0, 3.0, 24.0, INK);
        let end = self.label("DEATHS", end + 32.0, y + 15.0, TEXT);
        self.fat(&worm.deaths.to_string(), end + 10.0, y + 12.0, 3.0, TEXT);
    }

    /// Bottom left: `HP 78` and ten segments of 10 HP each.
    fn vitals(&mut self, worm: &Worm) {
        let (x, y) = (MARGIN, 720.0 - MARGIN - 84.0);
        let fraction = (worm.hp / MAX_HP).clamp(0.0, 1.0);
        let low = worm.alive && fraction <= 0.25;
        let fill = if !worm.alive { PANEL_GREY } else if low { PANEL_RED } else { PANEL_ORANGE };
        self.plate(x, y, 280.0, 84.0, fill);
        self.label("HP", x + 16.0, y + 38.0, TEXT);
        // Pattern: Tween. The number overshoots and settles each time it changes.
        let digits = format!("{:.0}", worm.hp.max(0.0));
        let pixel = 6.0 * pop(self.hp_age);
        let (cx, cy) = (x + 46.0 + text_width(&digits, 6.0) / 2.0, y + 10.0 + 21.0);
        self.fat(&digits, cx - text_width(&digits, pixel) / 2.0, cy - 3.5 * pixel, pixel, TEXT);
        let note = if !worm.alive {
            Some(format!("RESPAWN {:.0}", worm.respawn_timer.ceil()))
        } else if low {
            Some("LOW HEALTH".to_string())
        } else {
            None
        };
        if let Some(note) = note {
            self.label(&note, x + 264.0 - text_width(&note, 2.0), y + 14.0, TEXT);
        }
        let bar = if fraction > 0.5 { GOLD } else if fraction > 0.25 { WARN } else { TEXT };
        self.segments(x + 16.0, y + 60.0, 248.0, 14.0, 10, fraction, bar);
    }

    /// Bottom right: the slot rail above a plate with the name, the ammo and
    /// one status line (ready, reloading, charging or melee).
    fn weapon(&mut self, worm: &Worm, width: f32) {
        let (x, y) = (width - MARGIN - 332.0, 720.0 - MARGIN - 142.0);

        // The rail: one slot per weapon, keyed like the keyboard. A blue mark
        // under a slot means it is reloading, a red one that it is empty.
        // Right-aligned with the plate: fourteen slots are wider than it.
        let rail = x + 332.0 - worm.weapons.len() as f32 * 28.0 + 4.0;
        for (i, gun) in worm.weapons.iter().enumerate() {
            let def = &WEAPONS[gun.id as usize];
            let sx = rail + i as f32 * 28.0;
            let current = i == worm.current;
            // The chosen slot is a bright chip that sits proud of the rail.
            let lift = if current { 5.0 } else { 0.0 };
            self.rect(sx - 2.0, y - 34.0 - lift, 28.0, 28.0 + lift, INK);
            self.rect(sx, y - 32.0 - lift, 24.0, 24.0 + lift, if current { GOLD } else { 0x7a5a3a });
            let key = &SLOT_KEYS[i..i + 1];
            if current {
                self.text(key, sx + 7.0, y - 27.0 - lift, 2.0, INK);
            } else {
                self.label(key, sx + 7.0, y - 27.0, TEXT);
            }
            if gun.reload > 0.0 {
                self.rect(sx + 3.0, y - 12.0, 18.0, 3.0, RELOAD);
            } else if def.mag > 0 && gun.ammo == 0 {
                self.rect(sx + 3.0, y - 12.0, 18.0, 3.0, DANGER);
            }
        }

        let gun = &worm.weapons[worm.current];
        let def = &WEAPONS[gun.id as usize];
        self.plate(x, y, 332.0, 142.0, PANEL_ORANGE);
        self.fat(&def.name.to_uppercase(), x + 16.0, y + 12.0, 3.0, TEXT);

        // Ammo: one pip per round up to eight, a bar beyond that.
        if def.kind != Kind::Whip {
            let ammo = gun.ammo as f32;
            let mag = def.mag as f32;
            let colour = if gun.ammo == 0 { DANGER } else if ammo <= mag * 0.25 && def.mag > 1 { GOLD } else { TEXT };
            let end = self.fat(&gun.ammo.to_string(), x + 16.0, y + 42.0, 5.0, colour);
            self.label(&format!("/ {}", def.mag), end + 8.0, y + 63.0, TEXT);
            if def.mag > 8 {
                self.segments(x + 16.0, y + 86.0, 300.0, 12.0, 24, ammo / mag, colour);
            } else {
                let pip = (300.0 - 5.0 * (mag - 1.0)) / mag;
                let pip = pip.min(56.0);
                self.rect(x + 14.0, y + 84.0, mag * pip + 5.0 * (mag - 1.0) + 4.0, 16.0, INK);
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
            ("MELEE", String::new(), TEXT, None)
        } else if gun.reload > 0.0 {
            ("RELOADING", format!("{:.1} S", gun.reload), RELOAD, Some(1.0 - gun.reload / def.reload))
        } else if charge > 0.0 {
            ("CHARGING", format!("{:.0}%", charge * 100.0), GOLD, Some(charge))
        } else {
            ("READY", String::new(), READY, None)
        };
        self.label(label, x + 16.0, y + 106.0, colour);
        self.label(&right, x + 316.0 - text_width(&right, 2.0), y + 106.0, colour);
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
        }
        // The number outlives the cross so a quick series of hits stays readable.
        if self.marker.total_seconds > 0.0 {
            let damage = format!("{:.0}", self.marker.total);
            let fill = if self.marker.kill { DANGER } else { GOLD };
            let pixel = 3.5 + (self.marker.total / 120.0).clamp(0.0, 1.0) * 2.5;
            let left = self.marker.total_seconds;
            self.cartoon(&damage, (cx + 62.0, cy - 52.0), pixel, COMBO_WINDOW - left, left, fill);
        }
        // Incoming damage mirrors it on the other side, below the crosshair.
        if self.taken.seconds > 0.0 {
            let lost = format!("-{:.0}", self.taken.total);
            let pixel = 3.5 + (self.taken.total / 120.0).clamp(0.0, 1.0) * 2.5;
            let left = self.taken.seconds;
            self.cartoon(&lost, (cx - 62.0, cy + 52.0), pixel, COMBO_WINDOW - left, left, DANGER);
        }
    }

    /// A damage number as a cartoon sticker: ink outline, two flat cel
    /// tones, and it pops in big, settles, floats up and shrinks away.
    /// `age` is seconds since the last hit, `left` seconds until it is gone.
    /// Each font pixel is a free (unsnapped) quad so it can rotate and scale.
    fn cartoon(&mut self, s: &str, centre: (f32, f32), pixel: f32, age: f32, left: f32, fill: u32) {
        let pop = pop(age);
        let fade = (left / 0.25).clamp(0.0, 1.0);
        let size = pixel * pop * (0.4 + 0.6 * fade);
        let centre = (centre.0, centre.1 - age * 14.0);
        let (w, h) = (s.chars().count() as f32 * 6.0 - 1.0, 7.0);
        // Font-pixel units (origin at the text's middle) to virtual units.
        let place = |u: f32, v: f32| {
            let (u, v) = ((u - w / 2.0) * size, (v - h / 2.0) * size);
            (centre.0 + u, centre.1 + v)
        };
        let quad = |hud: &mut Hud, u0: f32, v0: f32, u1: f32, v1: f32, colour: Colour| {
            let (a, b, c, d) = (place(u0, v0), place(u1, v0), place(u1, v1), place(u0, v1));
            hud.triangle([a, b, c], colour);
            hud.triangle([a, c, d], colour);
        };
        let lit = |ch: char, col: i32, row: i32| {
            (0..5).contains(&col) && (0..7).contains(&row) && glyph(ch)[row as usize] >> (4 - col) & 1 == 1
        };
        let mut ink = rgb(INK);
        ink[3] = fade;
        let mut face = rgb(fill);
        face[3] = fade;
        let mut dark = rgb(shade(fill));
        dark[3] = fade;
        let glyphs: Vec<(usize, char)> = s.chars().enumerate().collect();
        // Outline first (every lit pixel grown by 0.7 font pixels), then the faces.
        for &(i, ch) in &glyphs {
            for row in 0..7 {
                for col in 0..5 {
                    if lit(ch, col, row) {
                        let (u, v) = ((i * 6) as f32 + col as f32, row as f32);
                        quad(self, u - 0.7, v - 0.7, u + 1.7, v + 1.7, ink);
                    }
                }
            }
        }
        for &(i, ch) in &glyphs {
            for row in 0..7 {
                for col in 0..5 {
                    if lit(ch, col, row) {
                        let (u, v) = ((i * 6) as f32 + col as f32, row as f32);
                        // A hair of overlap hides seams between rotated quads.
                        let colour = if row < 4 { face } else { dark };
                        quad(self, u - 0.02, v - 0.02, u + 1.02, v + 1.02, colour);
                    }
                }
            }
        }
    }

    // --- shapes ---------------------------------------------------------

    /// `segs` equal cells filled left to right up to `fraction`. The last lit
    /// cell may be partial, so the picture and the number never disagree.
    fn segments(&mut self, x: f32, y: f32, w: f32, h: f32, segs: u32, fraction: f32, colour: u32) {
        let gap = if segs > 1 { 2.0 } else { 0.0 };
        let cell = (w - gap * (segs - 1) as f32) / segs as f32;
        self.rect(x - 2.0, y - 2.0, w + 4.0, h + 4.0, INK);
        for i in 0..segs {
            let cx = x + i as f32 * (cell + gap);
            let lit = (fraction * segs as f32 - i as f32).clamp(0.0, 1.0);
            self.rect(cx, y, cell, h, TRACK);
            self.rect(cx, y, cell * lit, h, colour);
            self.rect(cx, y + h * 0.6, cell * lit, h * 0.4, shade(colour));
        }
    }

    /// A cel-shaded panel: an ink outline, the lit colour on top and one hard
    /// band of shade along the bottom, the same two tones the world uses.
    fn plate(&mut self, x: f32, y: f32, w: f32, h: f32, fill: u32) {
        self.octagon(x - 3.0, y - 3.0, w + 6.0, h + 6.0, 14.0, INK);
        self.octagon(x, y, w, h, 11.0, shade(fill));
        // The lit part: the same octagon cut flat at 62% of the height.
        let (c, k) = (11.0, h * 0.62);
        let p = [(x + c, y), (x + w - c, y), (x + w, y + c), (x + w, y + k), (x, y + k), (x, y + c)];
        let mid = (x + w / 2.0, y + k / 2.0);
        let colour = rgb(fill);
        for i in 0..6 {
            self.triangle([mid, p[i], p[(i + 1) % 6]], colour);
        }
    }

    /// Small text with a hard 2-unit drop shadow, so it stays readable on any panel.
    fn label(&mut self, s: &str, x: f32, y: f32, colour: u32) -> f32 {
        self.text(s, x + 2.0, y + 2.0, 2.0, INK);
        self.text(s, x, y, 2.0, colour)
    }

    /// Big text, cel shaded: every lit font pixel is grown by 0.6 pixel in ink
    /// first (the outline), then the letters go on top in two flat tones.
    fn fat(&mut self, s: &str, x: f32, y: f32, pixel: f32, colour: u32) -> f32 {
        let lit = |ch: char, col: i32, row: i32| {
            (0..5).contains(&col) && (0..7).contains(&row) && glyph(ch)[row as usize] >> (4 - col) & 1 == 1
        };
        for pass in 0..2 {
            for (i, ch) in s.chars().enumerate() {
                for row in 0..7 {
                    for col in 0..5 {
                        if !lit(ch, col, row) {
                            continue;
                        }
                        let (px, py) = (x + ((i * 6) as i32 + col) as f32 * pixel, y + row as f32 * pixel);
                        if pass == 0 {
                            self.rect(px - pixel * 0.6, py - pixel * 0.6, pixel * 2.2, pixel * 2.2, INK);
                        } else {
                            // Lit on the upper four rows, shaded below: one hard band.
                            self.rect(px, py, pixel, pixel, if row < 4 { colour } else { shade(colour) });
                        }
                    }
                }
            }
        }
        x + text_width(s, pixel)
    }

    /// A rectangle with its four corners cut by `c`, as a triangle fan.
    fn octagon(&mut self, x: f32, y: f32, w: f32, h: f32, c: f32, colour: u32) {
        self.octagon_c(x, y, w, h, c, rgb(colour));
    }

    fn octagon_c(&mut self, x: f32, y: f32, w: f32, h: f32, c: f32, colour: Colour) {
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

/// Pattern: Tween. Scale of something `age` seconds after it changed: it jumps
/// in small, overshoots to 1.3 and settles at 1.0, a squash-and-stretch pop.
fn pop(age: f32) -> f32 {
    if age < 0.06 {
        0.5 + 0.8 * age / 0.06
    } else if age < 0.16 {
        1.3 - 0.3 * (age - 0.06) / 0.10
    } else {
        1.0
    }
}

/// The shadow tone of a colour: darker and a touch warmer, like the world's cel bands.
fn shade(hex: u32) -> u32 {
    let c = |shift: u32, warm: f32| ((((hex >> shift) & 0xff) as f32 * (0.72 + warm)).min(255.0) as u32) << shift;
    c(16, 0.02) | c(8, 0.0) | c(0, -0.06)
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
