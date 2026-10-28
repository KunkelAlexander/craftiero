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
use crate::renderer::{DrawPrim, Light, Soot, Sprite};
use glam::Vec3;
use sdf::Grid;
use sim::weapons::{BLASTER, BOMB, BOMBLET, BOUNCER, CLUSTER, NOVA, ROCKET, SWARM, TELEPORT, WHIP};
use sim::weapons::LAVA as LAVA_GUN;
use sim::{AimPath, Event, LavaBit, Projectile, Worm, ZoneKind, DT, STAMP_RADIUS};
use std::f32::consts::TAU;

/// Gameplay effects share the renderer's 256-slot core pool with the worms,
/// projectiles and aim aid. Keep these lists small so a busy fight drops dust
/// instead of rockets.
const MAX_BURSTS: usize = 96;
const MAX_DUST: usize = 96;
/// Air traces. They are the first thing to drop when a fight is busy, because
/// they are the least important: the shot itself still has to be drawn.
const MAX_TRAILS: usize = 24;
/// Blood droplets are billboards now, not SDF capsules: one quad each, drawn
/// with the terrain behind them. The pool is cosmetic and generous, so a messy
/// hit never has to drop half its spray.
const MAX_BLOOD: usize = 512;
/// Surface marks. Blood is a painted decal (drawn from the G-buffer) and scorch
/// is the same kind of mark; both are cheap. Blood lives for ten minutes, so
/// the map keeps a fight's history until a busy match recycles the oldest. At
/// most `MAX_SPLATS` are held, newest last; when full the oldest is overwritten.
const MAX_SPLATS: usize = 8192;

const HINT: Vec3 = Vec3::new(1.0, 0.86, 0.35);

// Ammunition colours that are not part of the impact signature.
const OLIVE: Vec3 = Vec3::new(0.31, 0.37, 0.20);
const BOMBSHELL: Vec3 = Vec3::new(0.16, 0.15, 0.17);

/// Liero blood: uncompromising comic red, not the brown-red of a war film. The
/// droplets are self-lit so they stay legible in a dark crater; the splats are
/// darker because a stain on rock should look like a stain, not a lamp.
const BLOOD: Vec3 = Vec3::new(0.78, 0.035, 0.045);
/// Fresh lava: orange, hotter than fire.
const LAVA: Vec3 = Vec3::new(1.0, 0.42, 0.06);
/// The hottest lava, the orange it settles to, and the dull red crust it ends
/// as. Deliberately not near white: a glowing surface is tone-mapped, and a
/// bright yellow would roll off to a flat cream blob with no shape left.
const LAVA_HOT: Vec3 = Vec3::new(0.85, 0.27, 0.04);
const LAVA_MID: Vec3 = Vec3::new(0.70, 0.12, 0.02);
const LAVA_CRUST: Vec3 = Vec3::new(0.30, 0.06, 0.03);
const STEAM: Vec3 = Vec3::new(0.52, 0.50, 0.49);
const BLOOD_DARK: Vec3 = Vec3::new(0.50, 0.018, 0.028);
/// The scorch a shot leaves on rock: near-black, so it reads as a burn, not a
/// hole, and never competes with the weapon's own colour.
const SCORCH: Vec3 = Vec3::new(0.09, 0.075, 0.07);
/// Freshly broken rock: the lumps a blast throws out.
const ROCK: Vec3 = Vec3::new(0.34, 0.28, 0.23);

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
            // A plasma bolt: a hot head with a long, thin tail so its speed
            // reads. The head is deliberately larger and warmer than the trail
            // puffs; when they were the same size the bolt disappeared inside
            // its own wake.
            out.push(DrawPrim::cylinder(back(0.75), back(0.10), 0.045, CYAN, false));
            out.push(ball(at, 0.22, HOT).glowing());
            out.push(ball(at, 0.12, Vec3::ONE).glowing());
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
            // A landmine: a squat disc that always lies flat (whichever way it
            // tumbled), with a raised dome, a glowing trigger light and studs
            // around the rim. The sim keeps it a little above the ground, so the
            // body sinks below `at` to sit on it.
            let flat = Vec3::Y;
            out.push(DrawPrim::cylinder(at - flat * 0.13, at + flat * 0.03, 0.36, BOMBSHELL, false));
            out.push(DrawPrim::cylinder(at - flat * 0.03, at + flat * 0.02, 0.375, OLIVE, false));
            out.push(DrawPrim::cylinder(at + flat * 0.03, at + flat * 0.08, 0.22, OLIVE, false));
            out.push(ball(at + flat * 0.10, 0.06, EMBER).glowing());
            for k in 0..4 {
                let a = k as f32 * TAU / 4.0 + TAU / 8.0;
                let stud = at + Vec3::new(a.cos(), 0.0, a.sin()) * 0.28 + flat * 0.04;
                out.push(ball(stud, 0.045, BOMBSHELL));
            }
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

/// The lava as billboards: per bit one solid, soft-edged orange blob (alpha
/// blended, so a hundred of them overlap into an opaque mass instead of adding
/// up to white) and a faint wide halo (additive) that makes it glow. They are
/// sprites and not capsules because a hose has a hundred bits in flight, and the
/// raymarcher's primitive list (shared with the worms) has 256 places. A fresh
/// bit is bright orange, then it goes deeper red-orange, then it dulls to a red
/// crust as it cools, and the halo goes out: the colour is the clock that tells
/// you how soon it sets. Returns `(glow, body)`.
pub fn lava_sprites(list: &[LavaBit], alpha: f32) -> (Vec<Sprite>, Vec<Sprite>) {
    let mut glow = Vec::with_capacity(list.len());
    let mut body = Vec::with_capacity(list.len());
    for (k, b) in list.iter().enumerate() {
        let at = b.pos - b.vel * DT * (1.0 - alpha);
        let c = b.cooled();
        // Three-stop ramp: hot -> orange (by 0.5) -> crust.
        let colour = if c < 0.5 { LAVA_HOT.lerp(LAVA_MID, c * 2.0) } else { LAVA_MID.lerp(LAVA_CRUST, (c - 0.5) * 2.0) };
        // A slow shimmer, different for every bit, so the surface seethes.
        let shimmer = 1.0 + 0.10 * (b.age * 7.0 + k as f32 * 1.9).sin();
        let seed = k as f32 * 1.37;
        // As big as the rock it becomes: the stamp's radius plus the bulge its
        // smooth join adds. Drawn smaller, the lava shrank into a scatter of
        // dots and then froze into a lump twice its size.
        let size = (STAMP_RADIUS + 0.3) * shimmer;
        body.push(Sprite::ball(at + Vec3::Y * 0.3, size, colour, 0.97, seed, 0.15));
        let heat = (1.0 - c / 0.8).max(0.0);
        if heat > 0.0 {
            glow.push(Sprite::ball(at + Vec3::Y * 0.3, size * 2.0, LAVA, 0.16 * heat, seed, 0.6));
        }
    }
    (glow, body)
}

/// The air trace a projectile leaves: a small puff of smoke (or frost, or hot
/// metal) every tick or two, which hangs for half a second after the shot has
/// gone past. Returns the colour, size, ticks between puffs, and whether it is
/// self-lit.
fn trail(weapon: u8) -> Option<(Vec3, f32, u32, bool)> {
    match weapon {
        // The blaster is a fast bolt, so its ammunition reads as a hot comet
        // tail rather than a puff of smoke.
        BLASTER => Some((CYAN, 0.055, 1, true)),
        // A rocket leaves the classic grey plume.
        ROCKET => Some((Vec3::new(0.62, 0.60, 0.57), 0.180, 1, false)),
        CLUSTER => Some((Vec3::new(0.62, 0.56, 0.45), 0.140, 1, false)),
        NOVA => Some((Vec3::new(0.76, 0.86, 0.95), 0.100, 1, false)),
        // A bomblet is a hot ember: a short, bright wake.
        BOMBLET => Some((EMBER, 0.070, 1, true)),
        _ => None,
    }
}

/// Push `p` out to `clearance` from the rock along the surface normal. Markers
/// are capsules centred on their own position, so one placed exactly on the
/// surface has its lower half buried and reads as broken dashes rather than a
/// ring or a dot. The aim aid is the one thing that must never disappear into
/// the ground it is describing.
fn clear_of_rock(p: Vec3, clearance: f32, grid: &Grid) -> Vec3 {
    let d = grid.sample(p);
    if d >= clearance {
        return p;
    }
    let mut n = grid.gradient(p);
    if n.length_squared() < 1.0e-6 {
        n = Vec3::Y;
    }
    p + n * (clearance - d)
}

/// The aim aid a weapon draws: a dotted arc for a lob, a ring on the rock where
/// the shot will land, a blast footprint, and/or a short stub leaving a
/// ricochet. `AimPath` comes from the sim, so everything sits on the rock the
/// shot will really hit. `World::aim_path` decides which weapons get which.
/// `grid` is used only to lift the markers clear of the surface: a capsule that
/// is half inside the rock is invisible, which turned the landing ring into a
/// few disconnected yellow flecks.
pub fn trajectory(path: &AimPath, grid: &Grid) -> Vec<DrawPrim> {
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
            // The landing dot is on the surface; lift it (and any dot that
            // skims a slope) so the whole bead shows.
            let p = clear_of_rock(*p, radius * 0.85, grid);
            // A tiny stub makes a sphere: the raymarcher has no point primitive.
            out.push(
                DrawPrim::capsule(p, p + Vec3::Y * 0.004, radius, HINT, false)
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
        // Stand the line off the surface by its own radius, and lift any vertex
        // that the slope has buried, so a wide blast footprint hugs the rock
        // without cutting into it on the uphill side.
        let clearance = width + 0.035;
        let project = |p: Vec3| clear_of_rock(p, clearance, grid);
        let segments = 12;
        for k in 0..segments {
            let a0 = k as f32 * TAU / segments as f32;
            let a1 = (k + 1) as f32 * TAU / segments as f32;
            let p0 = project(hit + (side * a0.cos() + up * a0.sin()) * radius);
            let p1 = project(hit + (side * a1.cos() + up * a1.sin()) * radius);
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
        let centre = clear_of_rock(hit, 0.12, grid);
        out.push(
            DrawPrim::capsule(centre, centre + normal * 0.01, 0.160, HINT, false)
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
                let r = 0.060 - 0.012 * k as f32;
                let a = clear_of_rock(a, r, grid);
                out.push(
                    DrawPrim::capsule(a, b, r, HINT, false)
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
    /// A lump of rock thrown by a blast. Drawn as a solid capsule (with ink,
    /// shadow and shading), because flying rubble is what makes a crater look
    /// like it was torn open rather than painted on.
    Rock { at: Vec3, radius: f32 },
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
    /// Paint it as a stain on the G-buffer surface (a blood mark) instead of a
    /// scorch decal. Blood uses this; a scorch still uses the older decal.
    paint: bool,
    /// Draw in front of the scene, with no depth fade. A muzzle flash sits
    /// inside the drawn barrel and an impact flash is half-buried in the rock;
    /// both would otherwise be erased by the very geometry that made them.
    front: bool,
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
            paint: false,
            front: false,
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

    /// Draw it in front of the scene (see `front`).
    fn front(mut self) -> Effect {
        self.front = true;
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
            paint: false,
            front: false,
        }
    }

    /// A blood stain on a surface. It does not move or grow; it just sits there
    /// and dries out, which is the whole point of a splat.
    fn splat(at: Vec3, normal: Vec3, radius: f32) -> Effect {
        Effect {
            shape: Shape::Splat { at, normal, radius },
            colour: BLOOD,
            vel: Vec3::ZERO,
            gravity: 0.0,
            age: 0.0,
            // Ten minutes: a match's blood stays on the map. The pool is the
            // real limit (8k marks); a busy fight recycles the oldest instead
            // of the whole floor going clean every fourteen seconds.
            life: 600.0,
            burst: false,
            // Opacity: 1 for a fresh wound, lower for a faint burn.
            width: 1.0,
            hot: false,
            // A decal has no side; an ink rim would draw a 3D tube around it.
            inkless: true,
            // A blood mark is painted onto the rock from the G-buffer; scorch
            // uses the same shape but stays a plain decal.
            paint: true,
            front: false,
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
            paint: false,
            front: false,
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
            Shape::Rock { at, radius } => {
                // A lump, not a sphere: the two ends differ a little so the
                // silhouette is a nugget. It shrinks as it burns out.
                let size = radius * (1.0 - 0.45 * u);
                let tilt = noise(at.x * 3.0 + at.z * 2.0) - 0.5;
                let b = *at + Vec3::new(tilt, 0.55, -tilt) * size;
                let a = *at - Vec3::new(tilt, 0.55, -tilt) * size * 0.55;
                out.push(styled(DrawPrim::capsule(a, b, size, e.colour, false), e));
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
    /// The same for the lava's embers, smoke and lights.
    lava_tick: u32,
    /// Dynamic lights thrown by blasts, flashes and plasma.
    lights: Vec<LightFx>,
    /// Soot spheres on the terrain, left by blasts.
    soot: Vec<SootFx>,
    /// Zones that are running: each sends out a ring every second, so the
    /// edge of a low-gravity bubble stays readable for as long as it lasts.
    zones: Vec<ZoneFx>,
}

/// How a running zone is drawn: where, how far it reaches, and its colour.
#[derive(Clone, Copy)]
struct ZoneFx {
    at: Vec3,
    radius: f32,
    colour: Vec3,
    left: f32,
    /// Seconds until the next ring.
    next: f32,
}

/// Seconds between a zone's rings, and how long each takes to spread out.
const ZONE_PULSE: f32 = 1.0;
const ZONE_RING_LIFE: f32 = 1.4;

/// A patch of soot that fades over half a minute.
#[derive(Clone, Copy)]
struct SootFx {
    at: Vec3,
    radius: f32,
    strength: f32,
    age: f32,
    life: f32,
}

impl SootFx {
    fn now(&self) -> f32 {
        let u = (self.age / self.life).clamp(0.0, 1.0);
        // Full strength for the first half, then a slow wash-out.
        self.strength * (1.0 - ((u - 0.5) / 0.5).clamp(0.0, 1.0))
    }
}

/// A point light with a short life. Its power fades with age, so an explosion
/// lights the cave for a beat and then lets the world go dark again.
#[derive(Clone, Copy)]
struct LightFx {
    at: Vec3,
    colour: Vec3,
    radius: f32,
    power: f32,
    age: f32,
    life: f32,
}

impl LightFx {
    fn now(&self) -> f32 {
        (1.0 - (self.age / self.life).clamp(0.0, 1.0)).powi(2) * self.power
    }
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
            // The lash is the worm's own model and a swing is an animation
            // (see `worms.rs`); the crack is made where the lash cuts rock, by
            // the `Impact` events the cut sends. The start of a swing has
            // nothing to flash.
            Event::Whip { .. } => {}
            // The hose fires thirty times a second: a flash, spikes and a wisp of
            // smoke each time would be a strobe. One small glow at the nozzle.
            Event::Shot { weapon, pos, .. } if weapon == LAVA_GUN => {
                self.push_burst(Effect::puff(pos, Vec3::ZERO, 0.30, LAVA, 0.10, 0.0).glow().front());
            }
            Event::Shot { weapon, pos, .. } => {
                // A hot flash at the muzzle in the weapon's own colour (the
                // tracer's direction is known to `main`), plus a wisp of smoke
                // that drifts up after it. Long enough to read: a flash that
                // lives three frames was why the old muzzle felt like a pop.
                let (col, size) = flash(weapon);
                // A hot core and a wider halo: a flash has to be seen over the
                // gun, and a single soft dot at barrel height disappears.
                self.push_burst(Effect::puff(pos, Vec3::ZERO, size * 0.85, col, 0.16, 0.0).glow().front());
                self.push_burst(Effect::puff(pos, Vec3::ZERO, size * 2.1, col, 0.10, 0.0).glow().front());
                // Three short spikes, so the flash has a silhouette instead of
                // being a round lamp.
                for i in 0..3 {
                    let a = i as f32 * 2.094 + pos.x;
                    let off = Vec3::new(a.cos(), 0.25 + 0.3 * (a * 1.3).sin().abs(), a.sin()) * size * 1.1;
                    self.push_burst(
                        Effect::puff(pos + off, Vec3::ZERO, size * 0.42, col, 0.09, 0.0).glow().front(),
                    );
                }
                self.push_light(pos, col, 5.0, 1.6, 0.09);
                self.push_dust(Effect::puff(pos + Vec3::Y * 0.03, Vec3::Y * 0.6, 0.10, SMOKE, 0.5, -0.1));
            }
            Event::Hit { weapon, pos, damage, dir, .. } => {
                // The flash and sparks. Blood is added by `main` right after
                // this, because its splats need the terrain grid to land on.
                self.hit(pos, weapon, damage, dir);
            }
            Event::Impact { weapon, pos, normal } => {
                // A shot stopped against rock: sparks, a flash and a scorch.
                // Most missed shots would otherwise be silent and invisible.
                if weapon == WHIP {
                    self.whip_crack(pos, normal);
                } else {
                    self.impact(weapon, pos, normal);
                }
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
            Event::ZoneStarted { kind, pos, radius, seconds } => {
                let colour = match kind {
                    ZoneKind::Gravity(scale) if scale < 1.0 => Vec3::new(0.55, 0.80, 1.0),
                    ZoneKind::Gravity(_) => Vec3::new(1.0, 0.45, 0.30),
                };
                self.push_light(pos, colour, radius, 2.0, 0.5);
                self.zones.push(ZoneFx { at: pos, radius, colour, left: seconds, next: 0.0 });
            }
            Event::LavaFroze { pos } => {
                // Steam off the rock as it sets.
                let seed = pos.x * 3.1 + pos.z * 1.7;
                let drift = Vec3::new(noise(seed) - 0.5, 0.0, noise(seed + 2.0) - 0.5) * 0.5;
                self.push_dust(Effect::puff(pos + Vec3::Y * 0.4, Vec3::Y * 0.9 + drift, 0.26, STEAM, 1.3, -0.3));
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
        let up = if normal.length_squared() > 1.0e-4 { normal.normalize_or_zero() } else { Vec3::Y };
        // Fire must obey the terrain: a blast in a cave may not paint flames on
        // the rock in front of it. But a fireball centred at the impact point
        // sits at the bottom of the crater it just carved, where the near rim
        // hides every pixel. The answer is to erupt: put the fireball above the
        // hole, with a column of flame connecting it to the ground. The part of
        // the column still inside the pit is correctly hidden by the rim.
        let lift = 0.5 + radius * 0.55;
        let base = pos + up * lift;

        // A white-hot core, depth-tested like everything else.
        self.push_burst(
            Effect::burst(Shape::Ball { at: base, radius: radius * look.core_size }, look.core, 0.11).glow(),
        );
        self.push_light(
            pos + up * (lift + 0.4),
            look.fire[0].lerp(Vec3::ONE, 0.30),
            radius * 2.6 + 2.0,
            2.0 + radius * 1.6,
            0.45 + radius * 0.10,
        );
        // The crater is burnt: one soot sphere, centred a little below the
        // impact so it covers the floor and walls of the bowl.
        if self.soot.len() >= 16 {
            self.soot.remove(0);
        }
        self.soot.push(SootFx {
            at: pos - up * radius * 0.30,
            radius: radius * 1.7,
            strength: 0.95,
            age: 0.0,
            life: 24.0,
        });

        let fire_life = (0.34 + radius * 0.09) * look.fire_life;
        // One big fire body with smaller lobes around it. All-lobes reads as a
        // string of glowing beads; one big sphere reads as a balloon. Together
        // they make a ragged, connected fireball.
        let mut body = Effect::burst(
            Shape::Ball { at: base + up * radius * 0.12, radius: radius * 0.55 },
            look.fire[1],
            fire_life * 1.15,
        );
        body.vel = up * (6.0 + radius * 0.70);
        body.gravity = 5.0;
        body.hot = true;
        body.inkless = true;
        self.push_burst(body);
        for k in 0..7 {
            let a = k as f32 * 2.399 + pos.x * 1.3 + pos.z * 0.7;
            let out = Vec3::new(a.cos(), 0.0, a.sin());
            let reach = radius * look.fire_reach[k % 3] * (0.55 + 0.40 * k as f32);
            let rise = radius * (0.10 + 0.14 * ((k * 2 + 1) as f32).sin().abs());
            let size = radius * look.fire_size[k % 3] * (0.38 + 0.20 * (a * 1.7).sin().abs());
            let mut fire = Effect::burst(
                Shape::Ball { at: base + out * reach + up * rise, radius: size },
                look.fire[k % 3],
                fire_life * (0.90 + 0.12 * k as f32),
            );
            fire.vel = up * (6.0 + radius * 0.70) + out * (0.8 + radius * 0.22);
            fire.gravity = 6.0;
            fire.hot = true;
            fire.inkless = true;
            self.push_burst(fire);
        }
        // The flame column: five burning puffs from the crater floor to the
        // fireball. The lower ones are inside the pit and hidden; the upper ones
        // are the visible eruption rising over the rim.
        for k in 0..5 {
            let t = (k as f32 + 1.0) / 5.0;
            let at = pos + up * (lift * t);
            let mut flame = Effect::burst(
                Shape::Ball { at, radius: radius * 0.26 * (0.65 + 0.35 * t) },
                look.fire[(k + 1) % 3],
                fire_life * (0.55 + 0.12 * t),
            );
            flame.vel = up * (4.0 + radius * 0.45);
            flame.gravity = 5.0;
            flame.hot = true;
            flame.inkless = true;
            self.push_burst(flame);
        }

        // A short-lived white flash around the whole thing, so the blast reads
        // as a burst of light and not only a ball of fire.
        let mut flare = Effect::burst(
            Shape::Ball { at: base + up * radius * 0.18, radius: radius * 0.55 },
            look.core,
            0.13,
        );
        flare.hot = true;
        flare.inkless = true;
        self.push_burst(flare);

        self.debris(pos + up * (lift * 0.35), radius, &look);
        self.rocks(pos + up * (lift * 0.35), radius);

        // The smoke is a separate, longer-lived thing: it is what the player
        // sees after the fire has gone, and what makes a bomb feel dirty. A
        // handful of puffs of different sizes, rising at different speeds, build
        // into a column instead of one grey ball.
        let puffs = if radius > 4.0 { 12 } else { 8 };
        for k in 0..puffs {
            let a = k as f32 * 2.399 + pos.z * 0.9;
            let out = Vec3::new(a.cos(), 0.0, a.sin()) * radius * (0.05 + 0.08 * (k % 3) as f32);
            let mut smoke = Effect::puff(
                base + out + up * radius * (0.30 + 0.07 * (k % 4) as f32),
                Vec3::ZERO,
                radius * look.smoke_size * (1.15 + 0.20 * (k % 3) as f32),
                look.smoke,
                look.smoke_life * (0.85 + 0.10 * (k % 3) as f32),
                0.0,
            );
            smoke.vel = up * (look.smoke_rise * (1.2 + 0.25 * (k % 3) as f32)) + out * 0.35;
            smoke.gravity = -0.15;
            smoke.inkless = true;
            self.push_dust(smoke);
        }
        // A ring of dust along the ground, so the blast meets the crater it made
        // instead of hovering over a flat pale disc.
        let ring = if radius > 4.0 { 10 } else { 7 };
        for k in 0..ring {
            let a = k as f32 * 6.283185 / ring as f32 + pos.x;
            let out = Vec3::new(a.cos(), 0.0, a.sin());
            let mut d = Effect::puff(
                pos + out * radius * 0.55 + Vec3::Y * 0.25,
                out * (1.6 + radius * 0.30) + Vec3::Y * 0.6,
                radius * 0.38 * (0.85 + 0.15 * (k % 3) as f32),
                DUST,
                1.1 + 0.15 * (k % 3) as f32,
                2.5,
            );
            d.inkless = true;
            self.push_dust(d);
        }
        // Fire-lit smoke right over the flames: dark, warm, and low, so it does
        // not read as a second fireball.
        for k in 0..2 {
            let a = k as f32 * 2.399 + pos.x * 0.7;
            let out = Vec3::new(a.cos(), 0.0, a.sin());
            let mut d = Effect::puff(
                base + out * radius * 0.25 + up * radius * 0.10,
                up * (2.2 + radius * 0.25) + out * 0.8,
                radius * look.smoke_size * 0.55,
                look.fire[0].lerp(look.smoke, 0.80) * 0.7,
                1.1,
                0.0,
            );
            d.inkless = true;
            self.push_dust(d);
        }
        if look.dust > 0.0 {
            self.dust(pos + Vec3::Y * 0.2, look.dust);
        }
        // A scorch under the blast. On open ground the crater has replaced it,
        // but on a wall or a ceiling the burn is what marks the hit.
        self.scorch(pos, up, radius * 0.42, 0.65);
        // Shock rings: heavy weapons get one, the Nova gets two.
        for k in 0..look.rings {
            let scale = 1.0 - 0.18 * k as f32;
            let life = look.ring_life * (1.0 + 0.35 * k as f32);
            let colour = look.fire[0].lerp(Vec3::ONE, 0.15);
            // A few centimetres proud of the rock: the capsule has width, and a
            // ring exactly on the surface would half-sink into it.
            let mut ring = Effect::ring(pos + normal * 0.05, normal, radius * scale, colour, life);
            // Fainter than the fire: a shockwave is a ripple, not a white hoop.
            ring.width = 0.55;
            self.push_burst(ring);
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
        self.push_burst(Effect::puff(at, Vec3::ZERO, look.size * scale, look.flash, 0.07, 0.0).glow().front());
        self.push_light(at, look.flash, 3.4, 1.1 * scale, 0.10);
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
            ).glow());
        }
    }

    /// The plasma whip finding rock: not a muzzle flash but a crack. A white
    /// core, a fan of straight rays in the surface plane, a shock ring and
    /// sparks thrown along the wall. This is the reference's starburst.
    fn whip_crack(&mut self, at: Vec3, normal: Vec3) {
        let n = if normal.length_squared() > 1.0e-6 { normal.normalize() } else { Vec3::Y };
        let (side, up) = plane_tangents(n);
        let base = at + n * 0.06;
        self.push_burst(Effect::puff(base, Vec3::ZERO, 0.62, Vec3::ONE, 0.16, 0.0).glow().front());
        self.push_burst(Effect::puff(base, Vec3::ZERO, 0.30, PLASMA_HOT, 0.34, 0.0).glow().front());
        self.push_light(base, PLASMA_HOT, 7.0, 7.0, 0.34);
        let mut ring = Effect::ring(base, n, 1.6, PLASMA_HOT, 0.30);
        ring.width = 0.9;
        self.push_burst(ring);
        for i in 0..10 {
            let a = i as f32 * TAU / 10.0 + at.x * 0.7;
            let dir = (side * a.cos() + up * a.sin()).normalize_or_zero();
            let len = 0.55 + 0.55 * (i as f32 * 1.7).sin().abs();
            let mut ray = Effect::burst(Shape::Line { from: base, to: base + dir * len }, PLASMA_HOT, 0.18);
            ray.width = 0.020 + 0.012 * (i as f32 % 3.0);
            self.push_burst(ray.glow());
        }
        for i in 0..12 {
            let a = i as f32 * 2.399 + at.z;
            let dir = (side * a.cos() + up * a.sin() + n * 0.5).normalize_or_zero();
            self.push_burst(Effect::puff(
                at + n * 0.03,
                dir * (4.0 + i as f32 * 0.5),
                0.035,
                PLASMA_HOT,
                0.13 + i as f32 * 0.012,
                14.0,
            ).glow());
        }
        self.scorch(at, n, 0.35, 0.85);
    }

    /// A projectile ending on rock: a brief flash at the point of contact, a fan
    /// of sparks thrown along the surface, and a scorch mark left behind. The
    /// same signature as a body hit, so a blaster tick stays a tick.
    fn impact(&mut self, weapon: u8, at: Vec3, normal: Vec3) {
        let look = feel::hit(weapon);
        let n = if normal.length_squared() > 1.0e-6 { normal.normalize() } else { Vec3::Y };
        let (side, up) = plane_tangents(n);
        self.push_burst(Effect::puff(at + n * 0.04, Vec3::ZERO, look.size * 0.85, look.flash, 0.08, 0.0).glow().front());
        self.push_light(at + n * 0.05, look.flash, 2.6, 0.9, 0.09);
        for i in 0..look.sparks {
            let a = i as f32 * 2.399 + at.x * 1.7 + at.z * 0.9;
            let dir = (side * a.cos() + up * a.sin() + n * 0.6).normalize_or_zero();
            self.push_burst(Effect::puff(
                at + n * 0.03,
                dir * (look.speed * 0.75 + i as f32 * 0.4),
                0.028 + 0.012 * (i as f32 % 2.0),
                look.spark,
                0.10 + i as f32 * 0.015,
                14.0,
            ).glow());
        }
        // The mark it leaves: heavier weapons scorch a wider patch.
        let r = (0.05 + 0.05 * look.size).clamp(0.06, 0.24);
        self.scorch(at, n, r, 0.9);
    }

    /// A dark burn where a shot struck, or (reused) the floor under a blast.
    /// `strength` is its opacity: a scorch under a blast is fainter than the
    /// ring a single bolt leaves.
    fn scorch(&mut self, at: Vec3, normal: Vec3, radius: f32, strength: f32) {
        let e = Effect {
            shape: Shape::Splat { at: at + normal * 0.02, normal, radius },
            colour: SCORCH,
            vel: Vec3::ZERO,
            gravity: 0.0,
            age: 0.0,
            life: 12.0,
            burst: false,
            width: strength.clamp(0.0, 1.0),
            hot: false,
            inkless: true,
            // A scorch stays a decal: soot already darkens blast craters, and
            // only blood needs to be part of the ground.
            paint: false,
            front: false,
        };
        self.push_splat(e);
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
        // Liero, not a nosebleed: roughly twice the old spray. Droplets are
        // billboards, so the pool can be large without touching the SDF budget.
        let count = (48.0 + damage * 2.2).clamp(48.0, 260.0) as usize;

        for i in 0..count {
            let seed = i as f32 * 1.7 + at.x * 3.1 + at.z * 1.3;
            let a = seed * 2.399;
            let side = Vec3::new(a.cos(), 0.0, a.sin());
            // Splash-back on every third drop, the rest out of the exit wound.
            let along = if i % 3 == 0 { -push } else { push };
            // Every sixth drop is arterial: a thin jet on a flat, fast arc that
            // can cross the arena. Gore needs a few streaks that leave the body
            // far behind, not just a cloud hugging the wound. The low gravity
            // and long life keep them in the air past the fight.
            let arterial = i % 6 == 0;
            let (dir, speed, radius, life, gravity) = if arterial {
                let up = 0.20 + 0.35 * noise(seed + 7.0);
                let dir = (push * 1.15 + Vec3::Y * up + side * 0.30).normalize_or_zero();
                let speed = 55.0 + 55.0 * noise(seed + 31.0);
                let radius = 0.055 + 0.035 * noise(seed + 19.0);
                (dir, speed, radius, 1.10 + 0.60 * noise(seed + 23.0), 6.0 + 5.0 * noise(seed + 41.0))
            } else {
                let up = 0.35 + 0.85 * noise(seed + 7.0);
                let dir = (along * (0.55 + 0.45 * noise(seed + 11.0)) + Vec3::Y * up + side * 0.90)
                    .normalize_or_zero();
                let speed = (4.0 + 4.5 * force) * (0.55 + 0.75 * noise(seed + 3.0));
                let radius = (0.075 + 0.040 * force) * (0.65 + 0.7 * noise(seed + 19.0));
                (dir, speed, radius, 0.60 + 0.70 * noise(seed + 23.0), 17.0)
            };
            self.push_blood(
                Effect::puff(chest + side * 0.10, dir * speed, radius, BLOOD, life, gravity).glow().inkless(),
            );
        }
        // Fat gobs, because Liero never believed in restraint. Now that a
        // droplet is one quad, the spray can afford them.
        let gobs = (4.0 + damage / 20.0).clamp(4.0, 14.0) as usize;
        for i in 0..gobs {
            let a = i as f32 * 2.399 + at.x;
            let side = Vec3::new(a.cos(), 0.0, a.sin());
            self.push_blood(
                Effect::puff(
                    chest,
                    (push * 0.5 + Vec3::Y * 0.9 + side * 0.5).normalize_or_zero() * (4.5 + 2.5 * force),
                    0.130 + 0.050 * force,
                    BLOOD,
                    0.60 + 0.10 * noise(a),
                    17.0,
                )
                .glow()
                .inkless(),
            );
        }

        // A stain under the body: a rough ring around the worm, each point
        // dropped onto the first surface below. More and bigger marks than the
        // old paint pool could hold, because the map no longer throws them away.
        // The ring reaches further than the old puddle: blood thrown out of the
        // body should be on the floor around it, not only under its feet.
        let n = (24.0 + damage * 0.55).clamp(24.0, 90.0) as usize;
        for i in 0..n {
            let seed = i as f32 * 2.3 + at.x * 0.7 + at.z * 1.1;
            let a = seed * 2.399;
            let off = Vec3::new(a.cos(), 0.0, a.sin()) * (0.80 + 3.6 * noise(seed + 5.0));
            let from = at + off + Vec3::Y * 2.5;
            if let Some(t) = grid.raycast(from, Vec3::NEG_Y, 12.0) {
                if t <= 0.10 { continue; } // start outside the field: not a surface below
                let hit = from - Vec3::Y * t;
                let normal = grid.gradient(hit);
                let normal = if normal.length_squared() > 1.0e-4 { normal.normalize() } else { Vec3::Y };
                self.push_splat(Effect::splat(
                    hit + normal * 0.015,
                    normal,
                    0.34 + 0.20 * noise(seed + 13.0) + 0.12 * force,
                ));
            }
        }
        // The spray lands further out too: a fan of marks 2-22 m away in the
        // direction the blood was thrown. This is what stops the floor from
        // reading as one neat puddle around the victim.
        let spray = (10.0 + damage * 0.25).clamp(10.0, 32.0) as usize;
        let flat = Vec3::new(push.x, 0.0, push.z).normalize_or_zero();
        let flat = if flat.length_squared() > 0.5 { flat } else { Vec3::X };
        for i in 0..spray {
            let seed = i as f32 * 3.1 + at.z * 0.9 + at.x * 0.4;
            let a = seed * 2.399;
            let side = Vec3::new(a.cos(), 0.0, a.sin());
            let dir = (flat * (0.65 + 0.50 * noise(seed + 3.0)) + side * (0.25 + 0.55 * noise(seed + 9.0)))
                .normalize_or_zero();
            let reach = 2.0 + 20.0 * (i as f32 / spray as f32) * (0.55 + 0.45 * noise(seed + 17.0));
            let from = at + dir * reach + Vec3::Y * 3.0;
            if let Some(t) = grid.raycast(from, Vec3::NEG_Y, 20.0) {
                if t <= 0.10 { continue; } // start outside the field: not a surface below
                let hit = from - Vec3::Y * t;
                let normal = grid.gradient(hit);
                let normal = if normal.length_squared() > 1.0e-4 { normal.normalize() } else { Vec3::Y };
                self.push_splat(Effect::splat(
                    hit + normal * 0.015,
                    normal,
                    0.20 + 0.22 * noise(seed + 21.0),
                ));
            }
        }
        // And three on the wall the spray is pointing at, so a hit in a
        // corridor paints the corridor instead of leaving one neat dot.
        if let Some(t) = grid.raycast(chest, push, 4.5) {
            if t > 0.10 {
                let hit = chest + push * t;
                let normal = grid.gradient(hit);
                let normal = if normal.length_squared() > 1.0e-4 { normal.normalize() } else { -push };
                let side = if normal.y.abs() > 0.90 { Vec3::X } else { normal.cross(Vec3::Y) };
                let side = if side.length_squared() > 1.0e-4 { side.normalize() } else { Vec3::X };
                let up = normal.cross(side).normalize_or_zero();
                for k in 0..5 {
                    let a = k as f32 * 2.399 + at.x;
                    let off = side * (a.cos() * 0.40) + up * (a.sin() * 0.40);
                    self.push_splat(Effect::splat(
                        hit + normal * 0.02 + off,
                        normal,
                        0.30 + 0.20 * noise(at.z + k as f32) + 0.08 * force,
                    ));
                }
            }
        }
        // The arterial jets can reach the far side of the room. If there is a
        // surface out there, leave a distant splatter too, so the spray reads
        // across the arena instead of the blood stopping at the body.
        if let Some(t) = grid.raycast(chest, push, 45.0) {
            let hit = chest + push * t;
            if t > 6.0 {
                let normal = grid.gradient(hit);
                let normal = if normal.length_squared() > 1.0e-4 { normal.normalize() } else { -push };
                let side = if normal.y.abs() > 0.90 { Vec3::X } else { normal.cross(Vec3::Y) };
                let side = if side.length_squared() > 1.0e-4 { side.normalize() } else { Vec3::X };
                let up = normal.cross(side).normalize_or_zero();
                for k in 0..4 {
                    let a = k as f32 * 2.399 + at.x;
                    let off = side * (a.cos() * (0.30 + 0.25 * k as f32)) + up * (a.sin() * 0.30);
                    self.push_splat(Effect::splat(
                        hit + normal * 0.02 + off,
                        normal,
                        0.30 + 0.18 * noise(at.x + k as f32),
                    ));
                }
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
                dir * (look.debris_speed + radius * 0.9),
                0.10 + radius * 0.012,
                look.debris_colour,
                0.55 + (i as f32 % 4.0) * 0.18,
                14.0,
            );
            if look.debris_hot {
                spark = spark.glow();
            }
            self.push_burst(spark);
        }
    }

    /// Lumps of rock thrown out of a blast. These are SDF capsules, so they are
    /// shaded and outlined like the terrain and arc through the air under
    /// gravity; they are what makes a crater read as torn open.
    fn rocks(&mut self, at: Vec3, radius: f32) {
        let n = (6.0 + radius * 2.2).clamp(6.0, 22.0) as usize;
        for i in 0..n {
            let a = i as f32 * 2.399 + at.x * 1.3 + at.z * 0.7;
            let up = 0.45 + 0.65 * (i as f32 * 2.1).sin().abs();
            let dir = Vec3::new(a.cos(), up, a.sin()).normalize_or_zero();
            let speed = (4.0 + (i % 5) as f32 * 1.4) * (0.8 + radius * 0.10);
            let size = (0.09 + 0.035 * (i % 3) as f32) * (0.8 + radius * 0.12);
            self.push_burst(Effect {
                shape: Shape::Rock { at, radius: size },
                colour: ROCK * (0.75 + 0.35 * noise(a + 3.0)),
                vel: dir * speed,
                gravity: 18.0,
                age: 0.0,
                life: 1.8 + 0.25 * (i % 4) as f32,
                burst: false,
                width: 0.0,
                hot: false,
                inkless: false,
                paint: false,
                front: false,
            });
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

    /// Embers, smoke and light from the molten lava. Call it once per sim tick
    /// (like `trails`), so how much comes off does not depend on the frame rate.
    pub fn lava(&mut self, list: &[LavaBit]) {
        self.lava_tick = self.lava_tick.wrapping_add(1);
        let n = list.len();
        if n == 0 {
            return;
        }
        let t = self.lava_tick;
        // An ember lifts off a bit every other tick; which bit and where it
        // drifts come from the tick count, as everywhere in this file.
        if t % 2 == 0 {
            let k = (t as usize * 7) % n;
            let b = list[k];
            if b.cooled() < 0.85 {
                let seed = t as f32 * 1.3 + k as f32;
                let drift = Vec3::new(noise(seed) - 0.5, 0.0, noise(seed + 3.1) - 0.5) * 0.8;
                let rise = Vec3::Y * (1.0 + 1.5 * noise(seed + 7.0));
                self.push_dust(Effect::puff(b.pos + Vec3::Y * 0.3, rise + drift, 0.07, LAVA, 0.9, -0.3).glow());
            }
        }
        // Now and then a curl of smoke from another one.
        if t % 9 == 0 {
            let b = list[(t as usize * 5) % n];
            self.push_dust(Effect::puff(b.pos + Vec3::Y * 0.5, Vec3::Y * 1.1, 0.30, STEAM, 1.6, -0.2));
        }
        // Three lights spread through the list light the rock around the lava;
        // they dim as their bit cools.
        if t % 3 == 0 {
            for q in 0..3 {
                let b = list[(q * n) / 3];
                let power = 2.5 * (1.0 - b.cooled());
                if power > 0.05 {
                    self.push_light(b.pos + Vec3::Y * 0.6, LAVA, 5.0, power, 0.1);
                }
            }
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
            match &mut e.shape {
                Shape::Ball { at, .. } | Shape::Rock { at, .. } => {
                    *at += e.vel * dt;
                    e.vel.y -= e.gravity * dt;
                    // Light drag: dust hangs, sparks lose their speed quickly.
                    e.vel *= (-1.5 * dt).exp();
                }
                _ => {}
            }
        }
        self.list.retain(|e| e.age < e.life);
        self.blood.retain(|e| e.age < e.life);
        self.splats.retain(|e| e.age < e.life);
        self.dust.retain(|e| e.age < e.life);
        self.trails.retain(|e| e.age < e.life);
        for l in &mut self.lights {
            l.age += dt;
        }
        self.lights.retain(|l| l.age < l.life);
        for i in 0..self.zones.len() {
            self.zones[i].left -= dt;
            self.zones[i].next -= dt;
            if self.zones[i].next <= 0.0 {
                let ZoneFx { at, radius, colour, .. } = self.zones[i];
                self.push_burst(Effect::ring(at, Vec3::Y, radius, colour, ZONE_RING_LIFE));
                self.zones[i].next += ZONE_PULSE;
            }
        }
        self.zones.retain(|z| z.left > 0.0);
        for s in &mut self.soot {
            s.age += dt;
        }
        self.soot.retain(|s| s.age < s.life);
    }

    /// The gameplay effects drawn as solids: beam lines and flying rubble. The
    /// fire, smoke, sparks, dust and shockwaves are soft billboards (see
    /// `sprites`), which is why a fireball reads as light instead of a ball.
    pub fn core_capsules(&self) -> Vec<DrawPrim> {
        let mut out = Vec::new();
        draw_effects(
            self.list.iter().filter(|e| {
                matches!(&e.shape, Shape::Line { .. } | Shape::Rock { .. })
            }),
            &mut out,
        );
        out
    }

    /// The soft billboards and decals this frame, split by blend mode. `main`
    /// sorts `smoke` back-to-front once the camera is known. The blood list is
    /// stains (read the G-buffer for their surface) followed by flying beads.
    pub fn sprites(&self) -> Sprites {
        let mut glow = Vec::new();
        let mut smoke = Vec::new();
        for e in &self.list {
            if let Shape::Ball { .. } = &e.shape {
                if e.hot {
                    if let Some(s) = ball_sprite(e, true) {
                        glow.push(s);
                    }
                } else if let Some(s) = ball_sprite(e, false) {
                    smoke.push(s);
                }
            } else if let Shape::Ring { .. } = &e.shape {
                if let Some(s) = ring_sprite(e) {
                    glow.push(s);
                }
            }
        }
        for e in &self.dust {
            if let Some(s) = ball_sprite(e, false) {
                smoke.push(s);
            }
        }
        for e in &self.trails {
            if let Some(s) = line_sprite(e) {
                if e.hot {
                    glow.push(s);
                } else {
                    smoke.push(s);
                }
            }
        }
        let mut decals = Vec::new();
        // The blood list is the biggest allocation of the frame: reserve for
        // the marks and the spray up front instead of growing it piecemeal.
        let mut blood = Vec::with_capacity(
            (self.splats.len() + self.blood.len()).min(crate::renderer::MAX_BLOOD_SPRITES),
        );
        for e in &self.splats {
            if e.paint {
                if let Some(s) = stain(e) {
                    blood.push(s);
                }
            } else if let Some(d) = decal_sprite(e) {
                decals.push(d);
            }
        }
        // The droplets are billboards too, added after the marks so a bead in
        // flight always composites over paint on the ground it flew past.
        for e in &self.blood {
            if let Some(s) = bead_sprite(e) {
                blood.push(s);
            }
        }
        // A safety net: the effect pools are bounded, but the renderer's buffer
        // is the hard limit. Newest wins (the fresh wound, not the oldest mark).
        if blood.len() > crate::renderer::MAX_BLOOD_SPRITES {
            blood.drain(..blood.len() - crate::renderer::MAX_BLOOD_SPRITES);
        }
        // The eight strongest lights. A busy fight can have more events than
        // the shader has slots; the biggest blasts win, which is what the eye
        // expects too.
        let mut lights: Vec<LightFx> = self.lights.clone();
        lights.sort_unstable_by(|a, b| b.now().partial_cmp(&a.now()).unwrap_or(std::cmp::Ordering::Equal));
        let lights = lights
            .iter()
            .take(crate::renderer::MAX_LIGHTS)
            .map(|l| Light::point(l.at, l.colour, l.radius, l.now()))
            .collect();
        let mut soot: Vec<SootFx> = self.soot.clone();
        soot.sort_unstable_by(|a, b| b.now().partial_cmp(&a.now()).unwrap_or(std::cmp::Ordering::Equal));
        let soot = soot
            .iter()
            .take(crate::renderer::MAX_SOOT)
            .filter(|s| s.now() > 0.01)
            .map(|s| Soot::new(s.at, s.radius, s.now()))
            .collect();
        Sprites { glow, smoke, decals, blood, lights, soot }
    }

    /// The solid effects, for tests that do not care about the sprite pools.
    #[cfg(test)]
    pub fn capsules(&self) -> Vec<DrawPrim> {
        self.core_capsules()
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
            self.blood.swap_remove(0);
        }
        self.blood.push(e);
    }

    fn push_splat(&mut self, e: Effect) {
        // `swap_remove` is O(1): marks do not need to stay in order, and the
        // pool is now thousands of entries. `remove(0)` would memmove the whole
        // floor every time a busy fight added a stain.
        if self.splats.len() >= MAX_SPLATS {
            self.splats.swap_remove(0);
        }
        self.splats.push(e);
    }

    /// Add a point light. Kept short and capped: lights cost a loop iteration in
    /// every terrain pixel, and more than a handful at once reads as soup.
    fn push_light(&mut self, at: Vec3, colour: Vec3, radius: f32, power: f32, life: f32) {
        if self.lights.len() >= 24 {
            self.lights.remove(0);
        }
        self.lights.push(LightFx { at, colour, radius, power, age: 0.0, life });
    }
}

/// The three sprite lists the renderer layers over the picture. Additive glow
/// and alpha smoke are separate buffers (and passes) because they need
/// different blend modes.
pub struct Sprites {
    pub glow: Vec<Sprite>,
    pub smoke: Vec<Sprite>,
    pub decals: Vec<Sprite>,
    /// Blood on the ground and in flight. Both are painted billboards: a stain
    /// reads the G-buffer's surface point, a droplet is a round bead.
    pub blood: Vec<Sprite>,
    pub lights: Vec<Light>,
    pub soot: Vec<Soot>,
}

impl Sprites {
    /// Draw alpha smoke back-to-front (far first) so near puffs blend over far
    /// ones instead of sorting wrong where they overlap.
    pub fn sort_smoke(&mut self, cam: Vec3) {
        self.smoke.sort_unstable_by(|a, b| {
            let da = Vec3::new(a.a[0], a.a[1], a.a[2]).distance_squared(cam);
            let db = Vec3::new(b.a[0], b.a[1], b.a[2]).distance_squared(cam);
            db.partial_cmp(&da).unwrap_or(std::cmp::Ordering::Equal)
        });
    }
}

/// A round soft blob: the flashes, fire, sparks and smoke. Size and opacity
/// follow the effect's age, so a flash pops and a smoke puff grows and thins.
fn ball_sprite(e: &Effect, glow: bool) -> Option<Sprite> {
    let Shape::Ball { at, radius } = &e.shape else { return None };
    let u = (e.age / e.life).clamp(0.0, 1.0);
    let (size, alpha) = if e.burst {
        // A fireball blooms almost instantly, then shrinks and fades. It stays
        // bright for most of its life: a blast that dims as soon as it is born
        // reads as a wet match.
        let grow = (u / 0.16).clamp(0.0, 1.0);
        let s = grow * (1.0 - 0.62 * (u - 0.16).max(0.0));
        (*radius * s, (1.0 - u).powf(0.85) * 0.55)
    } else if glow {
        // A flash is full-size at birth and fades quickly.
        (*radius * (1.0 - 0.4 * u), (1.0 - u).powf(1.5))
    } else {
        // Smoke expands and thins out as it dissipates. It stays denser for
        // longer than a dust puff: a rocket's cloud is the after-image of the
        // shot and has to hang in the air.
        (*radius * (1.0 + 1.9 * u), (1.0 - u).powf(1.2) * 0.75)
    };
    if alpha <= 0.01 {
        return None;
    }
    let seed = at.x * 1.7 + at.y * 0.9 + at.z * 1.3;
    // Small softness: this is the fade distance at the terrain edge, and a large
    // one makes a sprite bleed visibly through a wall.
    let soft = if glow { (0.15 + size * 0.25).min(0.9) } else { (0.12 + size * 0.15).min(0.7) };
    Some(if e.front && glow && e.burst {
        let mut s = Sprite::flame(*at, size, e.colour, alpha, seed, soft);
        s.c[2] = 5.0; // turbulent + in front
        s
    } else if e.front {
        Sprite::front_ball(*at, size, e.colour, alpha, seed, soft)
    } else if glow && e.burst {
        Sprite::flame(*at, size, e.colour, alpha, seed, soft)
    } else {
        Sprite::ball(*at, size, e.colour, alpha, seed, soft)
    })
}

/// A trail puff: a short streak of smoke becomes one soft blob at its middle.
/// A hot trail (a blaster bolt, a bomblet ember) is bright; a rocket's plume is
/// dim smoke.
fn line_sprite(e: &Effect) -> Option<Sprite> {
    let Shape::Line { from, to } = &e.shape else { return None };
    let mid = (*from + *to) * 0.5;
    // A hot trail is a thin comet tail. This sprite is a round blob, so its
    // size has to come from the trail's thickness, not from how far the
    // projectile moved this tick: adding half the segment length made a fast
    // bolt's wake a ~2 m bubble that swallowed the bolt itself, and the player
    // saw a soft lamp hanging at the crosshair instead of a shot leaving the
    // gun. Smoke is different: it billows, so a big fast puff is right.
    let span = (*to - *from).length();
    let radius = if e.hot { (e.width * 2.2).max(0.08) } else { (e.width + span * 0.5).max(0.08) };
    let u = (e.age / e.life).clamp(0.0, 1.0);
    let alpha = if e.hot { (1.0 - u).powf(1.5) } else { (1.0 - u).powf(1.6) * 0.55 };
    if alpha <= 0.01 {
        return None;
    }
    let seed = mid.x * 1.7 + mid.y * 0.9 + mid.z * 1.3;
    let soft = if e.hot { (radius * 1.5 + 0.25).max(0.12) } else { (radius * 0.6 + 0.12).max(0.05) };
    Some(Sprite::ball(mid, radius, e.colour, alpha, seed, soft))
}

/// A shockwave: a ring that races out from the blast and thins as it goes. It
/// faces the camera because the blast is inside its own crater; a ring painted
/// on the ground would be hidden by the rim and would read as a plate.
fn ring_sprite(e: &Effect) -> Option<Sprite> {
    let Shape::Ring { at, normal, radius } = &e.shape else { return None };
    let u = (e.age / e.life).clamp(0.0, 1.0);
    // Fast at first, then coasting: a shockwave loses speed as it loses energy.
    let out = 1.0 - (1.0 - u).powi(3);
    let r = radius * (0.18 + 0.95 * out);
    let alpha = (1.0 - u).powi(2) * 0.95 * if e.width > 0.0 { e.width } else { 1.0 };
    if alpha <= 0.01 {
        return None;
    }
    let thickness = 0.10 + radius * 0.10 * (1.0 - u);
    let seed = at.x * 1.7 + at.z * 0.9 + normal.y;
    Some(Sprite::shock(*at + *normal * 0.05, r, e.colour, alpha, seed, thickness))
}


/// A blood stain as a painted decal. It holds full strength for most of its
/// life, then dries out (fades) at the end.
fn stain(e: &Effect) -> Option<Sprite> {
    let Shape::Splat { at, normal, radius } = &e.shape else { return None };
    let u = (e.age / e.life).clamp(0.0, 1.0);
    let strength = (1.0 - ((u - 0.7) / 0.3).clamp(0.0, 1.0)).clamp(0.0, 1.0) * 0.95 * e.width.clamp(0.0, 1.0);
    if strength <= 0.01 {
        return None;
    }
    let seed = at.x * 2.3 + at.z * 1.7 + normal.y * 3.1;
    // Fresh blood is bright red; it dries to a darker maroon as it ages.
    let colour = e.colour.lerp(BLOOD_DARK, u * 0.65);
    Some(Sprite::decal(*at, *normal, *radius, colour, strength, seed))
}

/// A droplet in flight: a round bead, not a ray-marched capsule. It shrinks as
/// it falls and fades in the last fifth of its life, so a spray thins out
/// instead of vanishing in one frame.
fn bead_sprite(e: &Effect) -> Option<Sprite> {
    let Shape::Ball { at, radius } = &e.shape else { return None };
    let u = (e.age / e.life).clamp(0.0, 1.0);
    let alpha = ((1.0 - u) / 0.20).clamp(0.0, 1.0);
    if alpha <= 0.01 {
        return None;
    }
    let seed = at.x * 1.7 + at.z * 1.3 + at.y * 0.9;
    Some(Sprite::blood_drop(*at, *radius * (1.0 - 0.45 * u), e.colour, alpha, seed))
}

/// A scorch mark lying on a surface. It holds full strength for most of its
/// life, then dries out (fades) at the end.
fn decal_sprite(e: &Effect) -> Option<Sprite> {
    let Shape::Splat { at, normal, radius } = &e.shape else { return None };
    let u = (e.age / e.life).clamp(0.0, 1.0);
    let alpha = (1.0 - ((u - 0.7) / 0.3).clamp(0.0, 1.0)).clamp(0.0, 1.0) * 0.95 * e.width.clamp(0.0, 1.0);
    if alpha <= 0.01 {
        return None;
    }
    let seed = at.x * 2.3 + at.z * 1.7 + normal.y * 3.1;
    // A fresh stain is bright red; it dries to a darker maroon as it ages.
    let colour = e.colour.lerp(BLOOD_DARK, u * 0.65);
    Some(Sprite::decal(*at + *normal * 0.02, *normal, *radius, colour, alpha, seed))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count(e: &Effects) -> usize {
        e.capsules().len()
    }

    fn glow_count(e: &Effects) -> usize {
        e.sprites().glow.len()
    }

    fn smoke_count(e: &Effects) -> usize {
        e.sprites().smoke.len()
    }

    fn decal_count(e: &Effects) -> usize {
        e.sprites().decals.len()
    }

    /// Blood sprites that lie on a surface (they carry a unit normal). Droplets
    /// carry none, so the two can be counted apart.
    fn stain_count(e: &Effects) -> usize {
        e.sprites().blood.iter().filter(|s| s.c[0] * s.c[0] + s.c[1] * s.c[1] + s.c[2] * s.c[2] > 0.5).count()
    }

    fn drop_count(e: &Effects) -> usize {
        e.sprites().blood.len() - stain_count(e)
    }

    #[test]
    fn a_fast_trail_stays_narrower_than_the_bolt_it_follows() {
        // Regression: a trail sprite used to take half the distance flown in
        // one tick as its radius. A blaster bolt moves 1.4 m a tick, so each
        // puff became a ~1 m glowing bubble that swallowed the projectile and
        // read as a lamp hanging at the crosshair.
        let mut e = Effect::burst(
            Shape::Line { from: Vec3::ZERO, to: Vec3::Z * 1.8 },
            CYAN,
            0.6,
        );
        e.width = 0.055;
        e.hot = true;
        let sprite = line_sprite(&e).expect("a trail sprite");
        assert!(sprite.a[3] < 0.25, "the wake is a thin streak, not a bubble: {}", sprite.a[3]);
    }

    #[test]
    fn every_bit_of_lava_is_a_blob_and_a_cooled_one_is_darker_and_does_not_glow() {
        use sim::Vec3 as V;
        let fresh = LavaBit { pos: V::ZERO, vel: V::ZERO, age: 0.0, life: 5.0, owner: 0 };
        let cold = LavaBit { age: 5.0, ..fresh };
        assert!(lava_sprites(&[], 1.0).1.is_empty());
        let (glow, body) = lava_sprites(&[fresh, cold], 1.0);
        assert_eq!(body.len(), 2);
        assert_eq!(glow.len(), 1, "only the molten one glows");
        let brightness = |s: &Sprite| s.b[0] + s.b[1] + s.b[2];
        assert!(brightness(&body[0]) > brightness(&body[1]) * 2.0);
    }

    #[test]
    fn a_shot_flashes_briefly_then_smokes() {
        let mut e = Effects::default();
        e.add(&Event::Shot { worm: 0, weapon: 0, pos: Vec3::ZERO });
        assert!(glow_count(&e) >= 5, "a core, a halo and spikes: {}", glow_count(&e));
        assert_eq!(smoke_count(&e), 1, "and a wisp of smoke");
        e.update(0.2);
        assert_eq!(glow_count(&e), 0, "the flash is over");
        assert_eq!(smoke_count(&e), 1, "the smoke hangs on");
        e.update(0.5);
        assert_eq!(smoke_count(&e), 0);
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
        assert!(glow_count(&e) >= 5, "a flash and a fan of sparks: {}", glow_count(&e));
        e.update(0.05);
        assert!(glow_count(&e) >= 3, "still in the air: {}", glow_count(&e));
        e.update(0.5);
        assert_eq!(glow_count(&e), 0);
    }

    #[test]
    fn a_hit_sprays_blood_and_stains_the_rock() {
        // Flat rock at y = 2, so the stains have somewhere real to land.
        let grid = Grid::from_fn(glam::IVec3::new(8, 4, 8), |p| p.y - 2.0);
        let mut e = Effects::default();
        e.blood(Vec3::new(4.0, 2.0, 4.0), Vec3::Z, 34.0, &grid);
        assert!(e.blood.len() >= 100, "a Liero spray, not a drop: {}", e.blood.len());
        assert!(e.splats.len() >= 30, "blood marks the floor: {}", e.splats.len());
        for s in &e.splats {
            match &s.shape {
                Shape::Splat { at, normal, radius } => {
                    assert!(normal.is_normalized());
                    assert!(*radius > 0.0);
                    // Ground marks sit on the flat floor; the far wall splatter
                    // is intentionally vertical.
                    if normal.y > 0.9 {
                        assert!((at.y - 2.0).abs() < 0.2, "the stain is on the rock: {at:?}");
                    }
                }
                _ => panic!("a splat should be a splat"),
            }
        }
        assert!(drop_count(&e) >= 100, "the spray is drawn as beads: {}", drop_count(&e));
        // Marks are not only a puddle at the feet: some land metres away.
        let far = e
            .splats
            .iter()
            .filter_map(|s| match &s.shape {
                Shape::Splat { at, .. } => Some(at.distance(Vec3::new(4.0, 2.0, 4.0))),
                _ => None,
            })
            .fold(0.0f32, f32::max);
        assert!(far > 5.0, "the spray stains the ground away from the body: {far} m");
        // Droplets are ballistic and short-lived; stains are the receipt. The
        // arterial jets are the longest-lived, so wait them out too.
        e.update(2.0);
        assert!(e.blood.is_empty(), "the droplets have fallen");
        assert!(!e.splats.is_empty(), "the stains remain");
        assert!(stain_count(&e) >= 30, "the stains paint the rock: {}", stain_count(&e));
        assert_eq!(decal_count(&e), 0, "blood is a blood mark, not a scorch decal");
    }

    #[test]
    fn blood_is_fx_and_never_touches_the_scene_primitives() {
        let grid = Grid::from_fn(glam::IVec3::new(8, 4, 8), |p| p.y - 2.0);
        let mut e = Effects::default();
        // A gameplay effect to protect: a beam line.
        e.add(&Event::Beam { from: Vec3::ZERO, to: Vec3::Z * 5.0 });
        let core = e.core_capsules().len();
        e.blood(Vec3::new(4.0, 2.0, 4.0), Vec3::Z, 70.0, &grid);
        assert_eq!(e.core_capsules().len(), core, "blood is not a scene primitive");
        assert!(drop_count(&e) > 0, "the spray is billboards");
        assert!(stain_count(&e) > 0, "the marks are painted decals");
    }

    #[test]
    fn some_blood_droplets_shoot_really_far() {
        // Gore needs arterial streaks, not only a cloud at the wound. With the
        // viewer's light drag, a 55 m/s drop covers roughly 28 m in the first
        // second; the fastest should clear the whole fight.
        let grid = Grid::from_fn(glam::IVec3::new(8, 4, 8), |p| p.y - 2.0);
        let mut e = Effects::default();
        e.blood(Vec3::new(4.0, 2.0, 4.0), Vec3::NEG_Z, 70.0, &grid);
        let fast = e.blood.iter().filter(|d| d.vel.length() > 50.0).count();
        let fastest = e.blood.iter().map(|d| d.vel.length()).fold(0.0, f32::max);
        assert!(fast >= 5, "a few jets leave the body behind: {fast}");
        assert!(fastest > 85.0, "the fastest drop can cross the arena: {fastest}");
    }

    #[test]
    fn a_long_fight_keeps_thousands_of_marks_and_recycles_the_oldest() {
        let grid = Grid::from_fn(glam::IVec3::new(8, 4, 8), |p| p.y - 2.0);
        let mut e = Effects::default();
        for i in 0..200 {
            e.blood(Vec3::new(4.0 + (i % 8) as f32 * 0.1, 2.0, 4.0), Vec3::Z, 70.0, &grid);
        }
        assert!(e.blood.len() <= MAX_BLOOD, "droplets are capped: {}", e.blood.len());
        assert_eq!(e.splats.len(), MAX_SPLATS, "the mark pool fills, then recycles: {}", e.splats.len());
        assert!(stain_count(&e) <= crate::renderer::MAX_BLOOD_SPRITES, "paint fits the buffer");
        // Two minutes on: the droplets are long gone; the map still carries
        // thousands of marks, where the old 14-second pool kept 48.
        e.update(120.0);
        assert!(e.blood.is_empty(), "droplets are brief");
        assert!(stain_count(&e) > 1000, "the floor remembers the fight: {}", stain_count(&e));
    }

    #[test]
    fn a_landing_throws_a_ring_of_dust() {
        let mut e = Effects::default();
        e.dust(Vec3::ZERO, 12.0);
        assert!(smoke_count(&e) >= 4, "hard landings throw a ring: {}", smoke_count(&e));
        let at = e.sprites().smoke[0].a;
        e.update(0.1);
        let now = e.sprites().smoke[0].a;
        let moved = Vec3::new(now[0] - at[0], now[1] - at[1], now[2] - at[2]).length();
        assert!(moved > 0.001, "the puffs move: {moved}");
        e.update(1.0);
        assert_eq!(smoke_count(&e), 0);
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
        assert_eq!(smoke_count(&e), 1, "one puff behind the rocket");
        e.trails(std::slice::from_ref(&rocket));
        assert_eq!(smoke_count(&e), 2, "the next tick leaves the next puff");
        e.update(0.2);
        assert_eq!(smoke_count(&e), 2, "the trail hangs in the air");
        e.update(0.5);
        assert_eq!(smoke_count(&e), 0, "and then it is gone");
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
            points: (0..10).map(|k| Vec3::new(0.0, 4.0 - k as f32 * 0.2, k as f32 * 1.5)).collect(),
            hit: Some(Vec3::new(0.0, 2.0, 13.5)),
            normal: Vec3::Y,
            ..Default::default()
        };
        let grid = Grid::from_fn(glam::IVec3::new(8, 4, 8), |p| p.y - 2.0);
        let prims = trajectory(&path, &grid);
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
    fn the_aim_aid_sits_proud_of_the_rock() {
        // Regression: markers placed exactly on the surface had their lower half
        // inside it, so the landing ring read as a few disconnected flecks.
        let at = Vec3::new(0.0, 2.0, 10.0);
        let blast = AimPath { hit: Some(at), normal: Vec3::Y, blast: 3.0, ..Default::default() };
        let grid = Grid::from_fn(glam::IVec3::new(8, 4, 8), |p| p.y - 2.0);
        let prims = trajectory(&blast, &grid);
        let mut lowest = f32::INFINITY;
        for p in &prims {
            if let DrawPrim::Capsule { a, .. } = *p {
                lowest = lowest.min(a.y);
            }
        }
        assert!(lowest > 2.0, "nothing is buried in the ground: {lowest}");
    }

    #[test]
    fn the_aim_aid_scales_the_ring_to_the_blast_and_draws_a_bounce_stub() {
        let at = Vec3::new(0.0, 2.0, 10.0);
        let grid = Grid::from_fn(glam::IVec3::new(8, 4, 8), |p| p.y - 2.0);
        let blast = AimPath { hit: Some(at), normal: Vec3::Y, blast: 8.0, ..Default::default() };
        let prims = trajectory(&blast, &grid);
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
        let prims = trajectory(&bounce, &grid);
        assert!(prims.len() >= 15, "a ring and a 2 m stub: {}", prims.len());
    }

    #[test]
    fn starting_a_whip_swing_spawns_no_effect_of_its_own() {
        let mut e = Effects::default();
        e.add(&Event::Whip {
            worm: 0,
            from: Vec3::ZERO,
            fwd: Vec3::Z,
            side: Vec3::NEG_X,
            lean: 0.0,
            style: sim::WhipStyle::Sweep,
            flip: false,
        });
        // The lash is part of the worm; the crack comes from the cut's `Impact`.
        assert_eq!(glow_count(&e), 0);
    }

    #[test]
    fn an_explosion_is_a_fireball_plus_dust() {
        let mut e = Effects::default();
        e.add(&Event::Explosion { weapon: ROCKET, pos: Vec3::ZERO, normal: Vec3::Y, radius: 3.0 });
        assert!(glow_count(&e) > 1, "fire and hot debris: {}", glow_count(&e));
        assert!(smoke_count(&e) > 0, "plus dust and smoke: {}", smoke_count(&e));
    }

    #[test]
    fn a_nova_and_a_bomblet_are_different_pictures() {
        let mut nova = Effects::default();
        nova.add(&Event::Explosion { weapon: NOVA, pos: Vec3::ZERO, normal: Vec3::Y, radius: 8.0 });
        let mut pop = Effects::default();
        pop.add(&Event::Explosion { weapon: BOMBLET, pos: Vec3::ZERO, normal: Vec3::Y, radius: 1.8 });

        // A Nova gets two shockwaves; a bomblet none. They are billboards now,
        // so the blast wave is visible above the crater it just made.
        let rings = |e: &Effects| e.sprites().glow.iter().filter(|s| (s.c[2] as i32 & 2) != 0).count();
        assert!(rings(&nova) >= 2, "the Nova answers with rings: {}", rings(&nova));
        assert_eq!(rings(&pop), 0);
        // More fire and debris for the bigger weapon.
        assert!(glow_count(&nova) > glow_count(&pop) + 6, "{} vs {}", glow_count(&nova), glow_count(&pop));
        // And the Nova's cloud outlives the pop's.
        nova.update(1.6);
        pop.update(1.6);
        assert!(smoke_count(&nova) > 0, "the crater still smokes");
        assert_eq!(smoke_count(&pop), 0);
    }

    #[test]
    fn a_shockwave_expands_and_sits_on_the_normal_side_of_the_blast() {
        let at = Vec3::new(1.0, 2.0, 3.0);
        let mut e = Effects::default();
        e.push_burst(Effect::ring(at, Vec3::X, 5.0, Vec3::ONE, 0.5));
        e.update(0.25);
        let s = e.sprites();
        let ring = s
            .glow
            .iter()
            .find(|s| (s.c[2] as i32 & 2) != 0)
            .expect("a shockwave sprite");
        assert!(ring.a[0] > at.x, "it stands proud of the surface: {:?}", ring.a);
        assert!(ring.a[3] > 1.0, "and it has expanded: {}", ring.a[3]);
        assert!(ring.c[3] > 0.0, "with a thickness: {}", ring.c[3]);
    }
}
