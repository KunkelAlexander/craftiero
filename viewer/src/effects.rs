// What a shot looks like: projectiles in flight, and the fireballs, shock rings,
// dust and beam lines that appear for a moment after an event. The palette and
// the per-weapon signature live in `feel.rs`; the sim knows nothing about how
// any of it looks.
//
// Effects live only in the viewer. Nothing in the sim reads them, so they need
// no replay and no syncing: a client that joined a second ago simply shows
// fewer of them.

use crate::feel;
use crate::feel::{BEAM, BRASS, CYAN, DUST, EMBER, HOT, ICE, LIME, MAGENTA, PLASMA_HOT, SMOKE, VIOLET};
use crate::renderer::DrawPrim;
use glam::Vec3;
use sdf::Grid;
use sim::weapons::{BLASTER, BOMB, BOMBLET, BOUNCER, CLUSTER, NOVA, ROCKET, SWARM, TELEPORT, WHIP};
use sim::{whip_path, AimPath, Event, Projectile, Worm, DT, WHIP_POINTS};
use std::f32::consts::TAU;

/// Gameplay effects share the renderer's 256-slot core pool with the worms,
/// projectiles and aim aid. Keep these lists small so a busy fight drops dust
/// instead of rockets.
const MAX_BURSTS: usize = 24;
const MAX_DUST: usize = 20;
/// Air traces. They are the first thing to drop when a fight is busy, because
/// they are the least important: the shot itself still has to be drawn.
const MAX_TRAILS: usize = 24;
/// Blood has its own 128-slot renderer pool, which gameplay cannot evict. These
/// two lists feed it: stains are queued first, so a full pool loses droplets,
/// never the marks on the floor.
const MAX_BLOOD: usize = 128;
const MAX_SPLATS: usize = 48;

const HINT: Vec3 = Vec3::new(1.0, 0.86, 0.35);

// Ammunition colours that are not part of the impact signature.
const OLIVE: Vec3 = Vec3::new(0.31, 0.37, 0.20);
const BOMBSHELL: Vec3 = Vec3::new(0.16, 0.15, 0.17);

/// Liero blood: uncompromising comic red, not the brown-red of a war film. The
/// droplets are self-lit so they stay legible in a dark crater; the splats are
/// darker because a stain on rock should look like a stain, not a lamp.
const BLOOD: Vec3 = Vec3::new(0.78, 0.035, 0.045);
const BLOOD_DARK: Vec3 = Vec3::new(0.50, 0.018, 0.028);

/// A deterministic 0..1 from a float. Blood spray must look violent, but the
/// viewer has no RNG: the same hit in a replay must throw the same droplets.
fn noise(seed: f32) -> f32 {
    let s = (seed * 12.9898).sin() * 43758.5453;
    s - s.floor()
}

/// Two unit vectors spanning the plane normal to `normal`: a shock ring lies in
/// that plane, so an explosion on a wall paints the wall instead of cutting a
/// flat circle through the rock. A degenerate normal falls back to the ground.
fn plane_tangents(normal: Vec3) -> (Vec3, Vec3) {
    let n = if normal.length_squared() > 1.0e-6 { normal.normalize() } else { Vec3::Y };
    let mut side = n.cross(Vec3::Y);
    if side.length_squared() < 1.0e-4 {
        side = n.cross(Vec3::X);
    }
    let side = side.normalize_or_zero();
    let up = side.cross(n).normalize_or_zero();
    (side, up)
}

/// A frame along the direction of travel: forward, a perpendicular, and up.
/// Falls back to +Z for a projectile at rest, so a sphere never degenerates into
/// a zero-length capsule (which the raycast cannot solve).
fn frame(dir: Vec3) -> (Vec3, Vec3, Vec3) {
    let fwd = if dir.length_squared() > 1.0e-8 { dir.normalize() } else { Vec3::Z };
    let mut side = fwd.cross(Vec3::Y);
    if side.length_squared() < 1.0e-6 {
        side = Vec3::X;
    }
    let side = side.normalize();
    (fwd, side, side.cross(fwd).normalize())
}

/// The muzzle flash: colour and size come from the weapon's signature (see
/// `feel.rs`), so a rocket booms orange and a blaster snaps cyan.
fn flash(weapon: u8) -> (Vec3, f32) {
    feel::flash(weapon)
}

/// The charge a weapon with a wind-up is building, at the muzzle. Drawn from
/// state rather than from an event, so it appears from the first tick and shows
/// in a screenshot taken mid-charge. A hot core swells with the wind-up while a
/// ring of dashes spins and closes in: the last frame before the shot is a
/// bright, tight knot, which is what the shot then releases.
pub fn charging(worm: &Worm) -> Vec<DrawPrim> {
    let t = worm.charge_fraction();
    if t <= 0.0 {
        return Vec::new();
    }
    let id = worm.weapons[worm.current].id;
    // The whip is the one wind-up that is not a charge: its lash is part of the
    // worm's own model, and the draw-back is drawn there (see `worms::lash`).
    // A gathering ring around the muzzle would be a second, wrong idea.
    if id == WHIP {
        return Vec::new();
    }
    let (colour, _) = flash(id);
    let at = worm.muzzle();
    let (dir, side, up) = frame(worm.aim_dir());
    // Start dim and saturate towards white-hot: a charge that is already white
    // would disappear against the Nova's own pale warhead.
    let hot = (colour * 0.70).lerp(Vec3::ONE, 0.8 * t);

    let mut out = Vec::with_capacity(15);
    // A small hot point just past the barrel tip, where the shot will leave.
    // The muzzle of an oversized gun is more than a metre ahead of the hand, so
    // the charge has to sit past the barrel to be seen rather than swallowed by
    // the gun's own warhead (the Nova's is a 0.44 m ball of the same colour).
    let core = at + dir * 1.55;
    out.push(DrawPrim::capsule(core, core + up * 0.004, 0.05 + 0.10 * t * t, hot, false).glowing());
    // A ring of dashes spinning up around the muzzle, wider than the gun so it
    // is never swallowed by it, and closing in as the charge completes.
    let radius = 1.00 - 0.30 * t;
    let width = 0.035 + 0.035 * t;
    let n = 10;
    for k in 0..n {
        let a0 = k as f32 * TAU / n as f32 + t * 3.0;
        let a1 = a0 + TAU / n as f32 * 0.55;
        let p0 = core + (side * a0.cos() + up * a0.sin()) * radius;
        let p1 = core + (side * a1.cos() + up * a1.sin()) * radius;
        out.push(DrawPrim::capsule(p0, p1, width, hot, false).glowing().inkless());
    }
    // Four streaks just outside it, turning the other way: energy being drawn
    // in, not just a ring that exists.
    for k in 0..4 {
        let a = k as f32 * TAU / 4.0 - t * 2.0;
        let far = radius + 0.34;
        let near = radius + 0.06;
        let p0 = core + (side * a.cos() + up * a.sin()) * far;
        let p1 = core + (side * a.cos() + up * a.sin()) * near;
        out.push(DrawPrim::capsule(p0, p1, 0.020, hot, false).glowing().inkless());
    }
    out
}

/// One projectile, drawn as a small assembly of primitives so the ammunition
/// reads as a *thing* in flight, not a coloured dash. `at` is the position this
/// frame; everything is built around it along the direction of travel.
fn ammo(weapon: u8, at: Vec3, vel: Vec3, out: &mut Vec<DrawPrim>) {
    let (d, _side, up) = frame(vel);
    let back = |t: f32| at - d * t;
    let ball = |p: Vec3, r: f32, col: Vec3| DrawPrim::capsule(p, p + up * 0.004, r, col, false);
    match weapon {
        BLASTER => {
            // A plasma bolt: a hot head with a short tail so its speed reads.
            out.push(DrawPrim::cylinder(back(0.36), at, 0.035, CYAN, false));
            out.push(ball(at, 0.11, HOT).glowing());
        }
        ROCKET => {
            // A real rocket: body, nose, and a flame where the exhaust is.
            out.push(DrawPrim::cylinder(back(0.50), at, 0.085, OLIVE, false));
            out.push(DrawPrim::cylinder(at, at + d * 0.22, 0.05, EMBER, false));
            out.push(ball(back(0.60), 0.13, EMBER).glowing());
        }
        BOUNCER => {
            out.push(ball(at, 0.17, LIME).glowing());
        }
        BOMB => {
            out.push(ball(at, 0.30, BOMBSHELL));
            out.push(ball(at + up * 0.30, 0.06, EMBER).glowing());
        }
        CLUSTER => {
            // A brass shell with a hot tracer tip.
            out.push(DrawPrim::cylinder(back(0.34), at, 0.10, BRASS, false));
            out.push(ball(at + d * 0.06, 0.075, EMBER).glowing());
        }
        BOMBLET => {
            out.push(ball(at, 0.11, EMBER).glowing());
        }
        SWARM => {
            // A tiny dart: a slim body and a bright nose.
            out.push(DrawPrim::cylinder(back(0.26), at - d * 0.05, 0.045, MAGENTA, false));
            out.push(ball(at + d * 0.05, 0.06, MAGENTA).glowing());
        }
        TELEPORT => {
            // An orb inside a halo: the ring is what says "portal".
            out.push(DrawPrim::cylinder(at - d * 0.012, at + d * 0.012, 0.30, VIOLET, false).glowing());
            out.push(ball(at, 0.18, VIOLET).glowing());
        }
        NOVA => {
            // A screen-clearing warhead: huge, icy, ringed in cyan.
            out.push(DrawPrim::cylinder(at - d * 0.015, at + d * 0.015, 0.64, CYAN, false).glowing());
            out.push(ball(at, 0.50, ICE));
        }
        _ => {
            out.push(ball(at, 0.10, HOT));
        }
    }
}

/// The projectiles as primitives. `alpha` is how far the frame is into the
/// current tick (0..1), exactly as for the worms: a bolt moves a metre a tick,
/// so drawing it only where it was at the last tick would judder on a 144 Hz
/// display. A projectile flies straight (or nearly), so instead of storing its
/// last position, step back along its velocity.
pub fn projectiles(list: &[Projectile], alpha: f32) -> Vec<DrawPrim> {
    let mut out = Vec::with_capacity(list.len() * 3);
    for p in list {
        let at = p.pos - p.vel * DT * (1.0 - alpha);
        ammo(p.weapon, at, p.vel, &mut out);
    }
    out
}

/// The air trace a projectile leaves: a small puff of smoke (or frost, or hot
/// metal) every tick or two, which hangs for half a second after the shot has
/// gone past. Returns the colour, size, ticks between puffs, and whether it is
/// self-lit.
fn trail(weapon: u8) -> Option<(Vec3, f32, u32, bool)> {
    match weapon {
        // A rocket leaves the classic grey plume.
        ROCKET => Some((Vec3::new(0.62, 0.60, 0.57), 0.180, 1, false)),
        CLUSTER => Some((Vec3::new(0.62, 0.56, 0.45), 0.140, 1, false)),
        NOVA => Some((Vec3::new(0.76, 0.86, 0.95), 0.100, 1, false)),
        // A bomblet is a hot ember: a short, bright wake.
        BOMBLET => Some((EMBER, 0.070, 1, true)),
        _ => None,
    }
}

/// The aim aid a weapon draws: a dotted arc for a lob, a ring on the rock where
/// the shot will land, a blast footprint, and/or a short stub leaving a
/// ricochet. `AimPath` comes from the sim, so everything sits on the rock the
/// shot will really hit. `World::aim_path` decides which weapons get which.
pub fn trajectory(path: &AimPath) -> Vec<DrawPrim> {
    let mut out = Vec::with_capacity(32);
    let n = path.points.len();
    if n > 0 {
        // At most 14 dots: enough to read the curve, cheap enough that the aid
        // never pushes real effects out of the renderer's budget.
        let stride = n.div_ceil(14);
        for (k, p) in path.points.iter().enumerate().step_by(stride) {
            let t = k as f32 / n as f32;
            // Big enough that the ink outline cannot swallow the bright middle:
            // a 5 cm dot 20 m away is all outline and reads as a stone.
            let radius = 0.150 - 0.045 * t;
            // A tiny stub makes a sphere: the raymarcher has no point primitive.
            out.push(
                DrawPrim::capsule(*p, *p + Vec3::Y * 0.004, radius, HINT, false)
                    .glowing()
                    .inkless(),
            );
        }
    }
    if let Some(hit) = path.hit {
        // A ring lying on the surface, because that is what says "it lands
        // here". Built from short capsules (there is no torus), aligned to the
        // rock's normal so it follows a slope instead of floating flat.
        let normal = if path.normal.length_squared() > 0.5 {
            path.normal.normalize_or_zero()
        } else {
            Vec3::Y
        };
        let mut side = normal.cross(Vec3::Y);
        if side.length_squared() < 1.0e-4 {
            side = Vec3::X;
        }
        let side = side.normalize();
        let up = side.cross(normal).normalize_or_zero();
        // A blast's footprint is the whole point of the mark; a lob only needs
        // a small "here" dot. Thicken the line a little with the ring so an
        // 8 m Nova does not read as a hairline scribble at range.
        let radius = if path.blast > 0.0 { path.blast } else { 0.70 };
        let width = 0.050 + 0.004 * path.blast;
        let segments = 12;
        for k in 0..segments {
            let a0 = k as f32 * TAU / segments as f32;
            let a1 = (k + 1) as f32 * TAU / segments as f32;
            let p0 = hit + (side * a0.cos() + up * a0.sin()) * radius;
            let p1 = hit + (side * a1.cos() + up * a1.sin()) * radius;
            // A thin continuous ring: inkless, so it stays a circle instead of
            // turning into a row of dark pebbles at a distance.
            out.push(
                DrawPrim::capsule(p0, p1, width, HINT, false)
                    .glowing()
                    .inkless(),
            );
        }
        // A big dot in the middle, so the mark still reads where the ring is
        // edge-on to the camera.
        out.push(
            DrawPrim::capsule(hit, hit + normal * 0.01, 0.160, HINT, false)
                .glowing()
                .inkless(),
        );
    }
    if let Some((from, to)) = path.bounce {
        // Three fading dashes along the first leg: enough to read the new
        // direction, not a full second bounce that would pretend to know every
        // pebble on the way.
        let dir = (to - from).normalize_or_zero();
        if dir.length_squared() > 0.5 {
            for k in 0..3 {
                let a = from + dir * (0.25 + 0.55 * k as f32);
                let b = from + dir * (0.75 + 0.55 * k as f32);
                out.push(
                    DrawPrim::capsule(a, b, 0.060 - 0.012 * k as f32, HINT, false)
                        .glowing()
                        .inkless(),
                );
            }
        }
    }
    out
}

enum Shape {
    /// A round burst (fireball, flash), or a puff of dust.
    Ball { at: Vec3, radius: f32 },
    /// A beam or a tracer: a thin line that thins out.
    Line { from: Vec3, to: Vec3 },
    /// A shockwave ring, expanding in the plane of the surface it hit.
    Ring { at: Vec3, normal: Vec3, radius: f32 },
    /// A blood splat lying on the rock: one flattened lozenge on `normal`.
    Splat { at: Vec3, normal: Vec3, radius: f32 },
}

struct Effect {
    shape: Shape,
    colour: Vec3,
    /// Motion for dust and sparks. A fireball or a beam leaves it zero.
    vel: Vec3,
    /// Downward acceleration, m/s^2.
    gravity: f32,
    /// Seconds since it appeared, and how long it lasts.
    age: f32,
    life: f32,
    /// A fireball grows in then shrinks; a puff of dust only shrinks.
    burst: bool,
    /// Radius of a line effect, in metres.
    width: f32,
    /// Self-lit: flashes, fireballs and beams are their own light source, so the
    /// shader draws them flat and bright. Dust and smoke are lit normally.
    hot: bool,
    /// Skip the ink outline (soft smoke, the aim aid). Without this a small
    /// primitive far away is all outline and reads as a dark pebble.
    inkless: bool,
}

impl Effect {
    fn burst(shape: Shape, colour: Vec3, life: f32) -> Effect {
        Effect {
            shape,
            colour,
            vel: Vec3::ZERO,
            gravity: 0.0,
            age: 0.0,
            life,
            burst: true,
            width: 0.12,
            hot: false,
            inkless: false,
        }
    }

    /// Draw it self-lit (flat and bright). Only for things small enough that a
    /// flat colour reads as light: a flash, a spark, a beam. A big fireball is
    /// left shaded, or it turns into a flat balloon.
    fn glow(mut self) -> Effect {
        self.hot = true;
        self
    }

    /// Skip the ink outline: for soft smoke, a flat decal, a small puff at
    /// range. Without this the outline swallows the colour and it reads as a
    /// dark pebble.
    fn inkless(mut self) -> Effect {
        self.inkless = true;
        self
    }

    fn puff(at: Vec3, vel: Vec3, radius: f32, colour: Vec3, life: f32, gravity: f32) -> Effect {
        Effect {
            shape: Shape::Ball { at, radius },
            colour,
            vel,
            gravity,
            age: 0.0,
            life,
            burst: false,
            width: 0.0,
            hot: false,
            inkless: false,
        }
    }

    /// A blood stain on a surface. It does not move or grow; it just sits there
    /// and dries out, which is the whole point of a splat.
    fn splat(at: Vec3, normal: Vec3, radius: f32) -> Effect {
        Effect {
            shape: Shape::Splat { at, normal, radius },
            colour: BLOOD_DARK,
            vel: Vec3::ZERO,
            gravity: 0.0,
            age: 0.0,
            // Long enough that a fight leaves a readable floor, short enough
            // that the pool recycles instead of becoming permanent.
            life: 14.0,
            burst: false,
            width: 0.0,
            hot: false,
            // A decal has no side; an ink rim would draw a 3D tube around it.
            inkless: true,
        }
    }

    /// A shockwave ring: it grows out to `radius`, thins and fades. Self-lit and
    /// inkless, so it reads as a wave of air rather than a drawn circle.
    fn ring(at: Vec3, normal: Vec3, radius: f32, colour: Vec3, life: f32) -> Effect {
        Effect {
            shape: Shape::Ring { at, normal, radius },
            colour,
            vel: Vec3::ZERO,
            gravity: 0.0,
            age: 0.0,
            life,
            burst: true,
            width: 0.0,
            hot: true,
            inkless: true,
        }
    }
}

/// Apply an effect's draw flags to a primitive: self-lit, and/or no ink.
fn styled(prim: DrawPrim, e: &Effect) -> DrawPrim {
    let prim = if e.hot { prim.glowing() } else { prim };
    if e.inkless { prim.inkless() } else { prim }
}

/// Turn a run of effects into primitives. Shared by the gameplay pool and the
/// blood pool so the two cannot drift apart in how they draw.
fn draw_effects<'a>(iter: impl Iterator<Item = &'a Effect>, out: &mut Vec<DrawPrim>) {
    for e in iter {
        let u = (e.age / e.life).clamp(0.0, 1.0);
        match &e.shape {
            Shape::Ball { at, radius } => {
                // A burst grows in the first third, then shrinks; dust only
                // shrinks, so it looks like it is settling.
                let size = if e.burst {
                    if u < 0.35 { u / 0.35 } else { 1.0 - (u - 0.35) / 0.65 }
                } else {
                    1.0 - u
                };
                // A capsule needs a direction: a stub makes a sphere.
                let ball = DrawPrim::capsule(*at, *at + Vec3::Y * 0.01, *radius * size, e.colour, false);
                out.push(styled(ball, e));
            }
            Shape::Line { from, to } => {
                let line = DrawPrim::capsule(*from, *to, e.width * (1.0 - u), e.colour, false);
                out.push(styled(line, e));
            }
            Shape::Ring { at, normal, radius } => {
                // Expand from a tight knot to the blast radius, thinning as
                // it goes, in the plane of the surface the blast hit. Eighteen
                // segments keeps the circle smooth; the ring is one effect, so
                // the budget stays predictable.
                let r = *radius * (0.15 + 0.85 * u);
                let width = (0.03 + radius * 0.015) * (1.0 - u).max(0.0);
                let (side, up) = plane_tangents(*normal);
                let n = 18;
                for k in 0..n {
                    let a0 = k as f32 * TAU / n as f32;
                    let a1 = (k + 1) as f32 * TAU / n as f32;
                    let p0 = *at + (side * a0.cos() + up * a0.sin()) * r;
                    let p1 = *at + (side * a1.cos() + up * a1.sin()) * r;
                    out.push(styled(DrawPrim::capsule(p0, p1, width, e.colour, false), e));
                }
            }
            Shape::Splat { at, normal, radius } => {
                // One flat lozenge lying on the surface. Varying its angle
                // and length with a hash keeps a wall of splats from looking
                // stamped; it dries (darkens and thins) over its long life.
                let dry = if u < 0.70 { 1.0 } else { 1.0 - (u - 0.70) / 0.30 };
                let base = if normal.y.abs() > 0.90 { Vec3::X } else { normal.cross(Vec3::Y) };
                let base = if base.length_squared() > 1.0e-4 { base.normalize() } else { Vec3::X };
                let spin = noise(at.x * 2.3 + at.z * 1.7) * TAU;
                let (s, c) = spin.sin_cos();
                let dir = base * c + normal.cross(base) * s;
                // Short and fat: a pool, not a dash. Long, thin splats read as
                // debris shadows on the grass; a broad lozenge reads as blood.
                let long = *radius * (0.30 + 0.25 * noise(at.z * 3.1 + at.x));
                let a = *at - dir * long;
                let b = *at + dir * long * 0.75;
                let colour = e.colour * (0.45 + 0.55 * dry);
                out.push(styled(
                    DrawPrim::capsule(a, b, *radius * 0.80 * dry.max(0.35), colour, false),
                    e,
                ));
            }
        }
    }
}

#[derive(Default)]
pub struct Effects {
    /// Explosions, beams, death flashes, sparks and muzzle flashes. Drawn
    /// first, so they survive the renderer's capsule budget.
    list: Vec<Effect>,
    /// Dust and footstep puffs. Cheap and frequent, dropped first.
    dust: Vec<Effect>,
    /// Air traces left by rockets and other fast projectiles.
    trails: Vec<Effect>,
    /// Blood in flight. Kept apart from the burst list so a messy hit cannot
    /// push a fireball out of the capsule budget.
    blood: Vec<Effect>,
    /// Blood that has landed: persistent, decorative, and the first thing to
    /// drop when the frame is full.
    splats: Vec<Effect>,
    /// Counts sim ticks, so a trail's spacing follows the fight rather than the
    /// display's frame rate.
    trail_tick: u32,
}

impl Effects {
    /// Start whatever effect `event` calls for (most events have none).
    pub fn add(&mut self, event: &Event) {
        match *event {
            Event::Explosion { weapon, pos, normal, radius } => {
                self.blast(weapon, pos, normal, radius);
            }
            Event::Beam { from, to } => {
                // A hot line, with a flash at each end: the muzzle it left and
                // the point it hit. One capsule, so it stays cheap.
                let mut line = Effect::burst(Shape::Line { from, to }, BEAM, 0.20).glow();
                line.width = 0.24;
                self.push_burst(line);
                self.push_burst(Effect::burst(Shape::Ball { at: from, radius: 0.24 }, BEAM, 0.09).glow());
                self.push_burst(Effect::burst(Shape::Ball { at: to, radius: 0.55 }, BEAM, 0.16).glow());
            }
            Event::Whip { from, dir, side, phase, length, style, .. } => {
                // The lash itself is carried by the worm (see `worms.rs`), so
                // the strike only needs to mark the crack: a hot flash at the
                // tip and a few sparks thrown off the rock or the body it hit.
                let points = whip_path(from, dir, side, phase, length, style);
                let tip = points[WHIP_POINTS - 1];
                self.push_burst(Effect::puff(tip, Vec3::ZERO, 0.30, PLASMA_HOT, 0.15, 0.0).glow());
                for i in 0..5 {
                    let a = i as f32 * 2.399 + phase;
                    let spark = Vec3::new(a.cos(), 0.35 + 0.4 * (a * 1.7).sin().abs(), a.sin());
                    self.push_burst(Effect::puff(
                        tip,
                        spark.normalize_or_zero() * (2.5 + i as f32 * 0.4),
                        0.05,
                        PLASMA_HOT,
                        0.16,
                        9.0,
                    ).glow());
                }
            }
            Event::Shot { weapon, pos, .. } => {
                // A short hot ball at the muzzle in the weapon's own colour (the
                // tracer's direction is known to `main`), plus a wisp of smoke
                // that drifts up after it.
                let (col, size) = flash(weapon);
                self.push_burst(Effect::puff(pos, Vec3::ZERO, size, col, 0.06, 0.0).glow());
                self.push_dust(Effect::puff(pos + Vec3::Y * 0.02, Vec3::Y * 0.5, 0.08, SMOKE, 0.4, -0.1));
            }
            Event::Hit { weapon, pos, damage, dir, .. } => {
                // The flash and sparks. Blood is added by `main` right after
                // this, because its splats need the terrain grid to land on.
                self.hit(pos, weapon, damage, dir);
            }
            Event::Died { pos, .. } => {
                self.push_burst(
                    Effect::burst(
                        Shape::Ball { at: pos + Vec3::Y * 0.9, radius: 1.3 },
                        Vec3::new(1.0, 1.0, 0.9),
                        0.45,
                    )
                    .glow(),
                );
            }
            Event::Teleported { from, to, .. } => {
                for at in [from, to] {
                    self.push_burst(
                        Effect::burst(
                            Shape::Ball { at: at + Vec3::Y * 0.9, radius: 1.0 },
                            Vec3::new(0.7, 0.5, 1.0),
                            0.3,
                        )
                        .glow(),
                    );
                }
            }
            _ => {}
        }
    }

    /// One blast, authored by its weapon. The sim's `radius` is the *damage*
    /// reach, so every visible size is a fraction of it: the fireball stays
    /// proportional to what the weapon can hurt without hiding the fight.
    fn blast(&mut self, weapon: u8, pos: Vec3, normal: Vec3, radius: f32) {
        let look = feel::blast(weapon);
        // A white-hot core, then three fire layers thrown out from the centre.
        self.push_burst(
            Effect::burst(Shape::Ball { at: pos, radius: radius * look.core_size }, look.core, 0.10).glow(),
        );
        for k in 0..3 {
            // Deterministic spray, like every other effect here: no RNG.
            let a = k as f32 * 2.1 + pos.x * 1.3 + pos.z * 0.7;
            let off = Vec3::new(a.cos(), 0.2 + 0.5 * (a * 1.7).sin(), a.sin()) * radius * look.fire_reach[k];
            let mut fire = Effect::burst(
                Shape::Ball { at: pos + off, radius: radius * look.fire_size[k] },
                look.fire[k],
                (0.25 + radius * 0.04) * look.fire_life,
            );
            // Self-lit and soft: a shaded sphere with an ink rim reads as a
            // balloon, not as fire.
            fire.hot = true;
            fire.inkless = true;
            self.push_burst(fire);
        }
        self.debris(pos, radius, &look);
        // The smoke is a separate, longer-lived thing: it is what the player
        // sees after the flash has gone, and what makes a bomb feel dirty. It
        // grows in from nothing, so the fire is not hidden on the first frame.
        let mut smoke = Effect::burst(
            Shape::Ball { at: pos + Vec3::Y * 0.3, radius: radius * look.smoke_size },
            look.smoke,
            look.smoke_life,
        );
        smoke.vel = Vec3::Y * look.smoke_rise;
        smoke.gravity = -0.2;
        smoke.inkless = true;
        self.push_dust(smoke);
        if look.dust > 0.0 {
            self.dust(pos + Vec3::Y * 0.2, look.dust);
        }
        // Shock rings: heavy weapons get one, the Nova gets two.
        for k in 0..look.rings {
            let scale = 1.0 - 0.18 * k as f32;
            let life = look.ring_life * (1.0 + 0.35 * k as f32);
            let colour = look.fire[0].lerp(Vec3::ONE, 0.35);
            // A few centimetres proud of the rock: the capsule has width, and a
            // ring exactly on the surface would half-sink into it.
            self.push_burst(Effect::ring(pos + normal * 0.05, normal, radius * scale, colour, life));
        }
    }

    /// A hit: a brief flash and a fan of sparks. `pos` is the point on the
    /// victim, so the sparks read as coming off the body, not the shooter.
    /// Colour, count and size come from the weapon; `push` is the way the body
    /// was knocked, so the spray follows the blow instead of always going up.
    pub fn hit(&mut self, at: Vec3, weapon: u8, damage: f32, push: Vec3) {
        let look = feel::hit(weapon);
        let scale = (damage / 70.0).clamp(0.35, 1.4);
        let push = push.normalize_or_zero();
        self.push_burst(Effect::puff(at, Vec3::ZERO, look.size * scale, look.flash, 0.07, 0.0).glow());
        for i in 0..look.sparks {
            // Deterministic spray: no RNG in the viewer, and the same hit at the
            // same place looks the same in a replay. The radial fan is bent
            // along the push, so a rocket throws sparks away from the blast.
            let a = i as f32 * 2.399 + at.x * 1.7 + at.z * 0.9;
            let up = 0.2 + 0.45 * (i as f32 * 2.1).sin().abs();
            let radial = Vec3::new(a.cos(), up, a.sin());
            let dir = (radial * 0.7 + push * 0.8).normalize_or_zero();
            self.push_burst(Effect::puff(
                at,
                dir * (look.speed + i as f32 * 0.5),
                0.045 + 0.02 * scale,
                look.spark,
                0.12 + i as f32 * 0.02,
                16.0,
            ));
        }
    }

    /// Liero blood. `at` is the victim's feet (the sim's `Hit` position) and
    /// `dir` is the way the damage pushed the body, so the spray mostly follows
    /// the wound outwards; a third of it kicks back towards the shooter, which
    /// is what makes the hit read from the firing side.
    ///
    /// The droplets are plain ballistic puffs: no collision, no simulation. The
    /// splats are found with two or three raycasts, so a stain really lands on
    /// the rock below (or the wall behind) instead of hanging in the air.
    pub fn blood(&mut self, at: Vec3, dir: Vec3, damage: f32, grid: &Grid) {
        let chest = at + Vec3::Y * 0.95;
        let push = if dir.length_squared() > 1.0e-6 { dir.normalize() } else { Vec3::Y };
        let force = (damage / 28.0).clamp(0.35, 2.4);
        // Liero, not a nosebleed: roughly five times the old spray. The
        // renderer's blood pool holds 128 primitives; the rest are dropped in
        // its own pool, never from gameplay.
        let count = (28.0 + damage * 1.40).clamp(28.0, 150.0) as usize;

        for i in 0..count {
            let seed = i as f32 * 1.7 + at.x * 3.1 + at.z * 1.3;
            let a = seed * 2.399;
            let side = Vec3::new(a.cos(), 0.0, a.sin());
            // Splash-back on every third drop, the rest out of the exit wound.
            let along = if i % 3 == 0 { -push } else { push };
            let up = 0.35 + 0.85 * noise(seed + 7.0);
            let speed = (4.0 + 4.5 * force) * (0.55 + 0.75 * noise(seed + 3.0));
            let dir =
                (along * (0.55 + 0.45 * noise(seed + 11.0)) + Vec3::Y * up + side * 0.90).normalize_or_zero();
            self.push_blood(
                Effect::puff(
                    chest + side * 0.10,
                    dir * speed,
                    (0.060 + 0.035 * force) * (0.65 + 0.7 * noise(seed + 19.0)),
                    BLOOD,
                    0.55 + 0.65 * noise(seed + 23.0),
                    17.0,
                )
                .glow()
                .inkless(),
            );
        }
        // Fat gobs, because Liero never believed in restraint.
        let gobs = (3.0 + damage / 25.0).clamp(3.0, 10.0) as usize;
        for i in 0..gobs {
            let a = i as f32 * 2.399 + at.x;
            let side = Vec3::new(a.cos(), 0.0, a.sin());
            self.push_blood(
                Effect::puff(
                    chest,
                    (push * 0.5 + Vec3::Y * 0.9 + side * 0.5).normalize_or_zero() * (4.5 + 2.5 * force),
                    0.115 + 0.045 * force,
                    BLOOD,
                    0.60 + 0.10 * noise(a),
                    17.0,
                )
                .glow()
                .inkless(),
            );
        }

        // A stain under the body: eight to twenty-six points around the feet,
        // each dropped onto the first surface below, and each bigger than the
        // old marks. A full strike paints a rough ring around the worm.
        let n = (8.0 + damage * 0.20).clamp(8.0, 26.0) as usize;
        for i in 0..n {
            let seed = i as f32 * 2.3 + at.x * 0.7 + at.z * 1.1;
            let a = seed * 2.399;
            let off = Vec3::new(a.cos(), 0.0, a.sin()) * (0.60 + 1.0 * noise(seed + 5.0));
            let from = at + off + Vec3::Y * 1.4;
            if let Some(t) = grid.raycast(from, Vec3::NEG_Y, 6.0) {
                let hit = from - Vec3::Y * t;
                let normal = grid.gradient(hit);
                let normal = if normal.length_squared() > 1.0e-4 { normal.normalize() } else { Vec3::Y };
                self.push_splat(Effect::splat(
                    hit + normal * 0.015,
                    normal,
                    0.14 + 0.09 * noise(seed + 13.0) + 0.04 * force,
                ));
            }
        }
        // And three on the wall the spray is pointing at, so a hit in a
        // corridor paints the corridor instead of leaving one neat dot.
        if let Some(t) = grid.raycast(chest, push, 4.5) {
            let hit = chest + push * t;
            let normal = grid.gradient(hit);
            let normal = if normal.length_squared() > 1.0e-4 { normal.normalize() } else { -push };
            let side = if normal.y.abs() > 0.90 { Vec3::X } else { normal.cross(Vec3::Y) };
            let side = if side.length_squared() > 1.0e-4 { side.normalize() } else { Vec3::X };
            let up = normal.cross(side).normalize_or_zero();
            for k in 0..3 {
                let a = k as f32 * 2.399 + at.x;
                let off = side * (a.cos() * 0.34) + up * (a.sin() * 0.34);
                self.push_splat(Effect::splat(
                    hit + normal * 0.02 + off,
                    normal,
                    0.10 + 0.08 * noise(at.z + k as f32) + 0.02 * force,
                ));
            }
        }
    }

    /// A fan of debris thrown out of an explosion: bigger and farther than a
    /// hit's sparks, so a rocket reads as a real blast rather than a pop. A bomb
    /// throws dirt (not glowing sparks), which is what makes it read as ground.
    fn debris(&mut self, at: Vec3, radius: f32, look: &feel::Blast) {
        for i in 0..look.debris {
            let a = i as f32 * 2.399 + at.x * 1.3 + at.z * 0.7;
            let up = look.debris_up + 0.5 * (i as f32 * 2.1).sin().abs();
            let dir = Vec3::new(a.cos(), up, a.sin()).normalize_or_zero();
            let mut spark = Effect::puff(
                at,
                dir * (look.debris_speed + radius * 0.6),
                0.06 + radius * 0.006,
                look.debris_colour,
                0.20 + (i as f32 % 3.0) * 0.03,
                14.0,
            );
            if look.debris_hot {
                spark = spark.glow();
            }
            self.push_burst(spark);
        }
    }

    /// Dust kicked up by a landing at `at` (the worm's feet). `strength` is the
    /// impact speed; a hard landing throws more and bigger puffs.
    pub fn dust(&mut self, at: Vec3, strength: f32) {
        let n = ((strength - 2.0) * 0.6).clamp(2.0, 6.0) as usize;
        for i in 0..n {
            let a = i as f32 * TAU / n as f32 + at.x * 0.7;
            let out = Vec3::new(a.cos(), 0.0, a.sin());
            let speed = 0.5 + strength * 0.05;
            self.push_dust(Effect::puff(
                at + out * 0.22,
                out * speed + Vec3::Y * (0.25 + strength * 0.025),
                0.10 + strength * 0.004,
                DUST,
                0.35 + 0.02 * (i % 3) as f32,
                1.2,
            ));
        }
    }

    /// One small puff per stride. `back` is the direction of travel, so the
    /// dust is left behind instead of running ahead of the worm.
    pub fn footstep(&mut self, at: Vec3, back: Vec3) {
        self.push_dust(Effect::puff(
            at + Vec3::Y * 0.05,
            -back * 0.30 + Vec3::Y * 0.25,
            0.07,
            DUST,
            0.26,
            1.0,
        ));
    }

    /// Leave an air trace behind the projectiles that should have one. Call
    /// this once per sim tick, not per frame: the trail is a record of where
    /// the shot has been, so it must not depend on the frame rate.
    pub fn trails(&mut self, list: &[Projectile]) {
        self.trail_tick = self.trail_tick.wrapping_add(1);
        for p in list {
            let Some((colour, radius, stride, hot)) = trail(p.weapon) else { continue };
            if self.trail_tick % stride != 0 {
                continue;
            }
            // A bomb at rest has no direction and is not going anywhere.
            if p.vel.length_squared() < 1.0 {
                continue;
            }
            // Start behind the projectile, so the trail grows out of its tail.
            let dir = p.vel.normalize_or_zero();
            let at = p.pos - dir * 0.30;
            // Deterministic wobble and size jitter (no RNG, like every effect
            // here): without it the puffs line up like beads on a string
            // instead of reading as smoke.
            let seed = at.x * 1.7 + at.y * 0.9 + at.z * 1.3;
            let off = Vec3::new(seed.sin(), (seed * 0.7).sin() * 0.35, (seed * 1.7).cos()) * 0.09;
            let scale = 0.80 + 0.45 * (seed * 2.3).sin().abs();
            // A short capsule rather than a ball: consecutive ones overlap into
            // one ragged plume, where a row of spheres reads as a string of
            // pearls. Its length follows the distance flown this tick (with a
            // little overlap), so a slow Nova gets short puffs and a fast
            // rocket a long streak instead of both becoming one solid rod.
            let length = (p.vel.length() * DT * stride as f32 * 1.25).max(0.20);
            let mut e = Effect::burst(
                Shape::Line { from: at + off, to: at + off - dir * length },
                colour,
                0.60,
            );
            e.width = radius * scale;
            e.hot = hot;
            // Smoke should be soft: an ink rim turns a tiny puff into a stone.
            e.inkless = true;
            self.push_trail(e);
        }
    }

    pub fn update(&mut self, dt: f32) {
        for e in self
            .list
            .iter_mut()
            .chain(self.blood.iter_mut())
            .chain(self.splats.iter_mut())
            .chain(self.dust.iter_mut())
            .chain(self.trails.iter_mut())
        {
            e.age += dt;
            if let Shape::Ball { at, .. } = &mut e.shape {
                *at += e.vel * dt;
                e.vel.y -= e.gravity * dt;
                // Light drag: dust hangs, sparks lose their speed quickly.
                e.vel *= (-1.5 * dt).exp();
            }
        }
        self.list.retain(|e| e.age < e.life);
        self.blood.retain(|e| e.age < e.life);
        self.splats.retain(|e| e.age < e.life);
        self.dust.retain(|e| e.age < e.life);
        self.trails.retain(|e| e.age < e.life);
    }

    /// The gameplay effects: blasts, beams, sparks, dust and trails. These
    /// share the renderer's 256-slot core pool.
    pub fn core_capsules(&self) -> Vec<DrawPrim> {
        let mut out = Vec::with_capacity(self.list.len() + self.dust.len() + self.trails.len() + 16);
        draw_effects(self.list.iter().chain(self.dust.iter()).chain(self.trails.iter()), &mut out);
        out
    }

    /// The cosmetic pool: stains first (the evidence stays), then droplets.
    /// The renderer keeps these separate from `core_capsules`, so blood can
    /// never push a worm or a rocket out of the frame.
    pub fn blood_capsules(&self) -> Vec<DrawPrim> {
        let mut out = Vec::with_capacity(self.splats.len() + self.blood.len());
        draw_effects(self.splats.iter().chain(self.blood.iter()), &mut out);
        out
    }

    /// Everything, for tests that do not care about the pools.
    #[cfg(test)]
    pub fn capsules(&self) -> Vec<DrawPrim> {
        let mut out = self.core_capsules();
        out.extend(self.blood_capsules());
        out
    }

    fn push_burst(&mut self, e: Effect) {
        if self.list.len() >= MAX_BURSTS {
            self.list.remove(0);
        }
        self.list.push(e);
    }

    fn push_dust(&mut self, e: Effect) {
        if self.dust.len() >= MAX_DUST {
            self.dust.remove(0);
        }
        self.dust.push(e);
    }

    fn push_trail(&mut self, e: Effect) {
        if self.trails.len() >= MAX_TRAILS {
            self.trails.remove(0);
        }
        self.trails.push(e);
    }

    fn push_blood(&mut self, e: Effect) {
        if self.blood.len() >= MAX_BLOOD {
            self.blood.remove(0);
        }
        self.blood.push(e);
    }

    fn push_splat(&mut self, e: Effect) {
        if self.splats.len() >= MAX_SPLATS {
            self.splats.remove(0);
        }
        self.splats.push(e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count(e: &Effects) -> usize {
        e.capsules().len()
    }

    /// The `a` endpoint of a puff (all effect primitives are capsules).
    fn a_of(c: &DrawPrim) -> Vec3 {
        match *c {
            DrawPrim::Capsule { a, .. } | DrawPrim::Cylinder { a, .. } => a,
            DrawPrim::Cuboid { centre, .. } => centre,
        }
    }

    #[test]
    fn a_shot_flashes_briefly_then_smokes() {
        let mut e = Effects::default();
        e.add(&Event::Shot { worm: 0, weapon: 0, pos: Vec3::ZERO });
        assert_eq!(count(&e), 2, "a flash and a wisp of smoke");
        e.update(0.07);
        assert_eq!(count(&e), 1, "the flash is over, the smoke hangs on");
        e.update(0.5);
        assert_eq!(count(&e), 0);
    }

    #[test]
    fn a_hit_flashes_and_throws_sparks() {
        let mut e = Effects::default();
        e.add(&Event::Hit {
            victim: 0,
            weapon: BLASTER,
            pos: Vec3::new(4.0, 1.0, 2.0),
            damage: 10.0,
            dir: Vec3::Z,
        });
        assert!(count(&e) >= 5, "a flash and a fan of sparks: {}", count(&e));
        e.update(0.05);
        assert!(count(&e) >= 3, "still in the air: {}", count(&e));
        e.update(0.5);
        assert_eq!(count(&e), 0);
    }

    #[test]
    fn a_hit_sprays_blood_and_stains_the_rock() {
        // Flat rock at y = 2, so the stains have somewhere real to land.
        let grid = Grid::from_fn(glam::IVec3::new(8, 4, 8), |p| p.y - 2.0);
        let mut e = Effects::default();
        e.blood(Vec3::new(4.0, 2.0, 4.0), Vec3::NEG_Z, 34.0, &grid);
        assert!(e.blood.len() >= 60, "a Liero spray, not a drop: {}", e.blood.len());
        assert!(e.splats.len() >= 10, "blood marks the floor: {}", e.splats.len());
        for s in &e.splats {
            match &s.shape {
                Shape::Splat { at, normal, radius } => {
                    assert!((at.y - 2.0).abs() < 0.2, "the stain is on the rock: {at:?}");
                    assert!(normal.is_normalized());
                    assert!(*radius > 0.0);
                }
                _ => panic!("a splat should be a splat"),
            }
        }
        // Droplets are ballistic and short-lived; stains are the receipt.
        e.update(1.2);
        assert!(e.blood.is_empty(), "the droplets have fallen");
        assert!(!e.splats.is_empty(), "the stains remain");
        assert_eq!(e.capsules().len(), e.splats.len(), "one cheap primitive per stain");
    }

    #[test]
    fn blood_is_drawn_from_its_own_pool() {
        let grid = Grid::from_fn(glam::IVec3::new(8, 4, 8), |p| p.y - 2.0);
        let mut e = Effects::default();
        // A gameplay effect to protect: a beam line.
        e.add(&Event::Beam { from: Vec3::ZERO, to: Vec3::Z * 5.0 });
        let core = e.core_capsules().len();
        e.blood(Vec3::new(4.0, 2.0, 4.0), Vec3::Z, 70.0, &grid);
        assert_eq!(e.core_capsules().len(), core, "blood never touches gameplay primitives");
        assert!(!e.blood_capsules().is_empty());
        assert_eq!(e.capsules().len(), core + e.blood_capsules().len(), "the test view sees both pools");
    }

    #[test]
    fn a_long_fight_cannot_flood_the_blood_budget() {
        let grid = Grid::from_fn(glam::IVec3::new(8, 4, 8), |p| p.y - 2.0);
        let mut e = Effects::default();
        for i in 0..40 {
            e.blood(Vec3::new(4.0 + i as f32 * 0.1, 2.0, 4.0), Vec3::Z, 70.0, &grid);
        }
        assert!(e.blood.len() <= MAX_BLOOD, "droplets are capped: {}", e.blood.len());
        assert!(e.splats.len() <= MAX_SPLATS, "stains are capped: {}", e.splats.len());
    }

    #[test]
    fn a_landing_throws_a_ring_of_dust() {
        let mut e = Effects::default();
        e.dust(Vec3::ZERO, 12.0);
        assert!(count(&e) >= 4, "hard landings throw a ring: {}", count(&e));
        let at = a_of(&e.capsules()[0]);
        e.update(0.1);
        assert!((a_of(&e.capsules()[0]) - at).length() > 0.001, "the puffs move");
        e.update(1.0);
        assert_eq!(count(&e), 0);
    }

    #[test]
    fn a_projectile_at_rest_still_draws_a_real_sphere() {
        // A dropped bomb has no velocity, so the motion frame has no forward
        // vector. It must still produce a usable sphere, not a degenerate one.
        let bomb = Projectile { pos: Vec3::new(3.0, 4.0, 5.0), vel: Vec3::ZERO, weapon: BOMB, owner: 0, life: 3.0 };
        let prims = projectiles(std::slice::from_ref(&bomb), 1.0);
        assert!(!prims.is_empty());
        for p in &prims {
            match *p {
                DrawPrim::Capsule { a, b, radius, .. } | DrawPrim::Cylinder { a, b, radius, .. } => {
                    assert!(a.is_finite() && b.is_finite() && radius > 0.0, "{a:?} {b:?} {radius}");
                    assert!((a - b).length() > 0.0 || radius > 0.0, "a sphere needs a radius");
                }
                DrawPrim::Cuboid { centre, half, .. } => assert!(centre.is_finite() && half.max_element() > 0.0),
            }
        }
    }

    #[test]
    fn dust_is_capped_so_it_cannot_push_real_effects_out() {
        let mut e = Effects::default();
        for _ in 0..100 {
            e.dust(Vec3::ZERO, 12.0);
            e.footstep(Vec3::ZERO, Vec3::X);
        }
        assert!(e.dust.len() <= MAX_DUST, "dust is bounded: {}", e.dust.len());
        assert!(count(&e) <= MAX_BURSTS + MAX_DUST + MAX_TRAILS);
    }

    #[test]
    fn a_rocket_leaves_a_trail_that_hangs_then_fades() {
        let rocket = Projectile { pos: Vec3::new(0.0, 8.0, 0.0), vel: Vec3::Z * 35.0, weapon: ROCKET, owner: 0, life: 1.0 };
        let mut e = Effects::default();
        e.trails(std::slice::from_ref(&rocket));
        assert_eq!(count(&e), 1, "one puff behind the rocket");
        e.trails(std::slice::from_ref(&rocket));
        assert_eq!(count(&e), 2, "the next tick leaves the next puff");
        e.update(0.2);
        assert_eq!(count(&e), 2, "the trail hangs in the air");
        e.update(0.5);
        assert_eq!(count(&e), 0, "and then it is gone");
    }

    #[test]
    fn fast_flat_shots_leave_no_trail() {
        let bolt = Projectile { pos: Vec3::ZERO, vel: Vec3::Z * 60.0, weapon: BLASTER, owner: 0, life: 1.0 };
        let mut e = Effects::default();
        e.trails(std::slice::from_ref(&bolt));
        assert_eq!(count(&e), 0);
    }

    #[test]
    fn trails_are_capped() {
        let rocket = Projectile { pos: Vec3::ZERO, vel: Vec3::Z * 35.0, weapon: ROCKET, owner: 0, life: 1.0 };
        let mut e = Effects::default();
        for _ in 0..100 {
            e.trails(std::slice::from_ref(&rocket));
        }
        assert!(e.trails.len() <= MAX_TRAILS, "trails are bounded: {}", e.trails.len());
    }

    #[test]
    fn the_aim_aid_draws_dots_and_a_landing_ring() {
        let path = AimPath {
            points: (0..10).map(|k| Vec3::new(0.0, 2.0 - k as f32 * 0.2, k as f32 * 1.5)).collect(),
            hit: Some(Vec3::new(0.0, 0.0, 13.5)),
            normal: Vec3::Y,
            ..Default::default()
        };
        let prims = trajectory(&path);
        assert!(prims.len() >= 12, "dots plus a ring: {}", prims.len());
        for p in &prims {
            match *p {
                DrawPrim::Capsule { a, b, radius, .. } | DrawPrim::Cylinder { a, b, radius, .. } => {
                    assert!(a.is_finite() && b.is_finite() && radius > 0.0);
                }
                DrawPrim::Cuboid { centre, half, .. } => assert!(centre.is_finite() && half.max_element() > 0.0),
            }
        }
    }

    #[test]
    fn the_aim_aid_scales_the_ring_to_the_blast_and_draws_a_bounce_stub() {
        let at = Vec3::new(0.0, 0.0, 10.0);
        let blast = AimPath { hit: Some(at), normal: Vec3::Y, blast: 8.0, ..Default::default() };
        let prims = trajectory(&blast);
        assert_eq!(prims.len(), 13, "a ring and its centre dot, no arc");
        let far = prims
            .iter()
            .filter_map(|p| match *p {
                DrawPrim::Capsule { a, .. } => Some(a.distance(at)),
                _ => None,
            })
            .fold(0.0f32, f32::max);
        assert!(far > 7.5, "the ring is 8 m across: {far}");

        let bounce = AimPath {
            hit: Some(at),
            normal: Vec3::Y,
            bounce: Some((at, at + Vec3::Z * 2.0)),
            ..Default::default()
        };
        let prims = trajectory(&bounce);
        assert!(prims.len() >= 15, "a ring and a 2 m stub: {}", prims.len());
    }

    #[test]
    fn a_whip_crack_flashes_at_the_tip_and_fades_fast() {
        let mut e = Effects::default();
        e.add(&Event::Whip {
            worm: 0,
            from: Vec3::ZERO,
            dir: Vec3::Z,
            side: Vec3::NEG_X,
            phase: 0.0,
            length: 9.5,
            style: sim::WhipStyle::Sweep,
        });
        // The lash itself is part of the worm (see `worms::lash`); the event
        // only adds the crack at the far end and the sparks it throws.
        let prims = e.capsules();
        assert!(prims.len() >= 5, "a flash and sparks: {}", prims.len());
        for p in &prims {
            match *p {
                DrawPrim::Capsule { a, b, radius, .. } | DrawPrim::Cylinder { a, b, radius, .. } => {
                    assert!(a.is_finite() && b.is_finite() && radius > 0.0);
                }
                DrawPrim::Cuboid { centre, half, .. } => assert!(centre.is_finite() && half.max_element() > 0.0),
            }
        }
        e.update(0.3);
        assert_eq!(count(&e), 0, "the sparks are gone in a blink");
    }

    #[test]
    fn an_explosion_is_a_fireball_plus_dust() {
        let mut e = Effects::default();
        e.add(&Event::Explosion { weapon: ROCKET, pos: Vec3::ZERO, normal: Vec3::Y, radius: 3.0 });
        assert!(count(&e) > 1, "the blast also kicks up dust: {}", count(&e));
    }

    #[test]
    fn a_nova_and_a_bomblet_are_different_pictures() {
        let mut nova = Effects::default();
        nova.add(&Event::Explosion { weapon: NOVA, pos: Vec3::ZERO, normal: Vec3::Y, radius: 8.0 });
        let mut pop = Effects::default();
        pop.add(&Event::Explosion { weapon: BOMBLET, pos: Vec3::ZERO, normal: Vec3::Y, radius: 1.8 });

        // A Nova gets two shock rings, eighteen capsules each; a bomblet none.
        assert!(count(&nova) > count(&pop) * 3, "{} vs {}", count(&nova), count(&pop));
        // The rings expand: after a moment the widest primitive is far out.
        nova.update(0.2);
        let far = nova
            .capsules()
            .iter()
            .map(|p| a_of(p).length())
            .fold(0.0f32, f32::max);
        assert!(far > 2.0, "the shockwave has travelled: {far}");
        // And the Nova's cloud outlives the pop's.
        nova.update(1.0);
        pop.update(1.0);
        assert!(count(&nova) > 0 && count(&pop) == 0, "the crater still smokes: {} {}", count(&nova), count(&pop));
    }

    #[test]
    fn a_shock_ring_lies_on_the_surface_it_hit() {
        // An explosion on a wall must paint the wall, not cut a flat ground
        // ring through the rock. A ring with normal +X stays in the YZ plane.
        let at = Vec3::new(1.0, 2.0, 3.0);
        let mut e = Effects::default();
        e.push_burst(Effect::ring(at, Vec3::X, 5.0, Vec3::ONE, 0.5));
        e.update(0.25);
        let prims = e.capsules();
        assert_eq!(prims.len(), 18, "one capsule per segment");
        for p in &prims {
            let a = a_of(p);
            assert!((a.x - at.x).abs() < 1.0e-4, "the ring is in the wall plane: {a:?}");
            assert!(a.distance(at) > 1.0, "and it has expanded: {a:?}");
        }
    }
}
