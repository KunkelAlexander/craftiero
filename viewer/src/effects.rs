// What a shot looks like: projectiles in flight, and the fireballs, dust and
// beam lines that appear for a moment after an event. Like `worms.rs`, this is
// the only place that knows colours; the sim knows nothing about how any of it
// looks.
//
// Effects live only in the viewer. Nothing in the sim reads them, so they need
// no replay and no syncing: a client that joined a second ago simply shows
// fewer of them.

use crate::renderer::DrawPrim;
use glam::Vec3;
use sim::weapons::{BLASTER, BOMB, BOMBLET, BOUNCER, CLUSTER, NOVA, ROCKET, SWARM, TELEPORT};
use sim::{Event, Projectile, DT};
use std::f32::consts::TAU;

/// The renderer holds 64 capsules in total, and worms/projectiles are drawn
/// before effects. Keep the cosmetic lists small so a busy fight drops dust
/// instead of rockets.
const MAX_BURSTS: usize = 24;
const MAX_DUST: usize = 20;

const DUST: Vec3 = Vec3::new(0.60, 0.55, 0.46);
const SMOKE: Vec3 = Vec3::new(0.38, 0.36, 0.34);
const SPARK: Vec3 = Vec3::new(1.0, 0.72, 0.28);
const HOT: Vec3 = Vec3::new(1.0, 0.90, 0.60);

// Colours shared with the weapons in `worms.rs`, so a shot matches its gun.
const CYAN: Vec3 = Vec3::new(0.35, 0.92, 1.0);
const EMBER: Vec3 = Vec3::new(1.0, 0.55, 0.16);
const LIME: Vec3 = Vec3::new(0.55, 0.95, 0.30);
const MAGENTA: Vec3 = Vec3::new(0.90, 0.35, 0.85);
const VIOLET: Vec3 = Vec3::new(0.55, 0.42, 1.0);
const ICE: Vec3 = Vec3::new(0.80, 0.92, 1.0);
const BRASS: Vec3 = Vec3::new(0.80, 0.62, 0.26);
const BEAM: Vec3 = Vec3::new(0.55, 0.92, 1.0);
const OLIVE: Vec3 = Vec3::new(0.31, 0.37, 0.20);
const BOMBSHELL: Vec3 = Vec3::new(0.16, 0.15, 0.17);

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

/// The muzzle flash: a hot ball whose colour and size match the weapon, so a
/// rocket booms orange and a blaster snaps cyan.
fn flash(weapon: u8) -> (Vec3, f32) {
    match weapon {
        BLASTER => (CYAN, 0.20),
        ROCKET => (EMBER, 0.34),
        BOUNCER => (LIME, 0.24),
        BOMB => (EMBER, 0.26),
        CLUSTER => (BRASS, 0.28),
        SWARM => (MAGENTA, 0.18),
        TELEPORT => (VIOLET, 0.26),
        NOVA => (ICE, 0.38),
        _ => (HOT, 0.20),
    }
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

enum Shape {
    /// A round burst (fireball, flash), or a puff of dust.
    Ball { at: Vec3, radius: f32 },
    /// A beam or a tracer: a thin line that thins out.
    Line { from: Vec3, to: Vec3 },
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
}

impl Effect {
    fn burst(shape: Shape, colour: Vec3, life: f32) -> Effect {
        Effect { shape, colour, vel: Vec3::ZERO, gravity: 0.0, age: 0.0, life, burst: true, width: 0.12, hot: false }
    }

    /// Draw it self-lit (flat and bright). Only for things small enough that a
    /// flat colour reads as light: a flash, a spark, a beam. A big fireball is
    /// left shaded, or it turns into a flat balloon.
    fn glow(mut self) -> Effect {
        self.hot = true;
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
}

impl Effects {
    /// Start whatever effect `event` calls for (most events have none).
    pub fn add(&mut self, event: &Event) {
        match *event {
            Event::Explosion { pos, radius } => {
                // The sim's `radius` is the *blast* radius (the edge of the
                // damage sphere), so the visible ball has to sit well inside it:
                // a fireball as big as the damage volume hides the whole fight.
                self.push_burst(
                    Effect::burst(
                        Shape::Ball { at: pos, radius: radius * 0.26 },
                        Vec3::new(1.0, 0.98, 0.92),
                        0.10,
                    )
                    .glow(),
                );
                let fire = [
                    (0.44, 0.12, Vec3::new(1.0, 0.60, 0.20)),
                    (0.33, 0.17, Vec3::new(1.0, 0.80, 0.28)),
                    (0.22, 0.20, Vec3::new(1.0, 0.96, 0.78)),
                ];
                for (k, (size, dist, col)) in fire.into_iter().enumerate() {
                    // Deterministic spray, like every other effect here: no RNG.
                    let a = k as f32 * 2.1 + pos.x * 1.3 + pos.z * 0.7;
                    let off = Vec3::new(a.cos(), 0.2 + 0.5 * (a * 1.7).sin(), a.sin()) * radius * dist;
                    self.push_burst(Effect::burst(
                        Shape::Ball { at: pos + off, radius: radius * size },
                        col,
                        0.25 + radius * 0.04,
                    ));
                }
                // Debris sparks fly out, then a small dark smoke puff rises:
                // small, because a big one would swallow the fireball.
                self.debris(pos, radius);
                self.push_dust(Effect::puff(
                    pos + Vec3::Y * 0.3,
                    Vec3::Y * 1.2,
                    radius * 0.22,
                    SMOKE,
                    0.8 + radius * 0.05,
                    -0.2,
                ));
                self.dust(pos + Vec3::Y * 0.2, 10.0);
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
            Event::Shot { weapon, pos, .. } => {
                // A short hot ball at the muzzle in the weapon's own colour (the
                // tracer's direction is known to `main`), plus a wisp of smoke
                // that drifts up after it.
                let (col, size) = flash(weapon);
                self.push_burst(Effect::puff(pos, Vec3::ZERO, size, col, 0.06, 0.0).glow());
                self.push_dust(Effect::puff(pos + Vec3::Y * 0.02, Vec3::Y * 0.5, 0.08, SMOKE, 0.4, -0.1));
            }
            Event::Hit { pos, .. } => {
                self.hit(pos);
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

    /// A hit: a brief flash and a fan of sparks. `pos` is the point on the
    /// victim, so the sparks read as coming off the body, not the shooter.
    pub fn hit(&mut self, at: Vec3) {
        self.push_burst(Effect::puff(at, Vec3::ZERO, 0.16, Vec3::new(1.0, 0.98, 0.9), 0.07, 0.0).glow());
        for i in 0..5 {
            // Deterministic spray: no RNG in the viewer, and the same hit at the
            // same place looks the same in a replay.
            let a = i as f32 * 2.399 + at.x * 1.7 + at.z * 0.9;
            let up = 0.2 + 0.45 * (i as f32 * 2.1).sin().abs();
            let dir = Vec3::new(a.cos(), up, a.sin()).normalize_or_zero();
            self.push_burst(Effect::puff(
                at,
                dir * (3.5 + i as f32 * 0.5),
                0.055,
                SPARK,
                0.12 + i as f32 * 0.02,
                16.0,
            ));
        }
    }

    /// A fan of debris thrown out of an explosion: bigger and farther than a
    /// hit's sparks, so a rocket reads as a real blast rather than a pop.
    fn debris(&mut self, at: Vec3, radius: f32) {
        let n = 7;
        for i in 0..n {
            let a = i as f32 * 2.399 + at.x * 1.3 + at.z * 0.7;
            let up = 0.35 + 0.55 * (i as f32 * 2.1).sin().abs();
            let dir = Vec3::new(a.cos(), up, a.sin()).normalize_or_zero();
            self.push_burst(Effect::puff(
                at,
                dir * (4.0 + radius * 0.8),
                0.06,
                SPARK,
                0.20 + (i as f32 % 3.0) * 0.03,
                14.0,
            ).glow());
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

    pub fn update(&mut self, dt: f32) {
        for e in self.list.iter_mut().chain(self.dust.iter_mut()) {
            e.age += dt;
            if let Shape::Ball { at, .. } = &mut e.shape {
                *at += e.vel * dt;
                e.vel.y -= e.gravity * dt;
                // Light drag: dust hangs, sparks lose their speed quickly.
                e.vel *= (-1.5 * dt).exp();
            }
        }
        self.list.retain(|e| e.age < e.life);
        self.dust.retain(|e| e.age < e.life);
    }

    pub fn capsules(&self) -> Vec<DrawPrim> {
        self.list
            .iter()
            .chain(self.dust.iter())
            .map(|e| {
                let u = (e.age / e.life).clamp(0.0, 1.0);
                match e.shape {
                    Shape::Ball { at, radius } => {
                        // A burst grows in the first third, then shrinks; dust
                        // only shrinks, so it looks like it is settling.
                        let size = if e.burst {
                            if u < 0.35 { u / 0.35 } else { 1.0 - (u - 0.35) / 0.65 }
                        } else {
                            1.0 - u
                        };
                        // A capsule needs a direction: a stub makes a sphere.
                        let ball = DrawPrim::capsule(at, at + Vec3::Y * 0.01, radius * size, e.colour, false);
                        if e.hot { ball.glowing() } else { ball }
                    }
                    Shape::Line { from, to } => {
                        let line = DrawPrim::capsule(from, to, e.width * (1.0 - u), e.colour, false);
                        if e.hot { line.glowing() } else { line }
                    }
                }
            })
            .collect()
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
        e.add(&Event::Hit { victim: 0, pos: Vec3::new(4.0, 1.0, 2.0), damage: 10.0 });
        assert!(count(&e) >= 5, "a flash and a fan of sparks: {}", count(&e));
        e.update(0.05);
        assert!(count(&e) >= 3, "still in the air: {}", count(&e));
        e.update(0.5);
        assert_eq!(count(&e), 0);
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
        assert!(count(&e) <= MAX_BURSTS + MAX_DUST);
    }

    #[test]
    fn an_explosion_is_a_fireball_plus_dust() {
        let mut e = Effects::default();
        e.add(&Event::Explosion { pos: Vec3::ZERO, radius: 3.0 });
        assert!(count(&e) > 1, "the blast also kicks up dust: {}", count(&e));
    }
}
