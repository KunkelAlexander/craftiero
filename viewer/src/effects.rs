// What a shot looks like: projectiles in flight, and the fireballs, dust and
// beam lines that appear for a moment after an event. Like `worms.rs`, this is
// the only place that knows colours; the sim knows nothing about how any of it
// looks.
//
// Effects live only in the viewer. Nothing in the sim reads them, so they need
// no replay and no syncing: a client that joined a second ago simply shows
// fewer of them.

use crate::renderer::DrawCapsule;
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
const SPARK: Vec3 = Vec3::new(1.0, 0.72, 0.28);
const HOT: Vec3 = Vec3::new(1.0, 0.90, 0.60);

/// Colour and size of each weapon's projectile, as picked in a paint program.
/// (A capsule: the radius, and a streak behind it that grows with the speed.)
fn look(weapon: u8) -> (Vec3, f32) {
    match weapon {
        BLASTER => (Vec3::new(0.35, 0.92, 1.0), 0.09),
        ROCKET => (Vec3::new(1.0, 0.55, 0.2), 0.17),
        BOUNCER => (Vec3::new(0.5, 1.0, 0.45), 0.14),
        BOMB => (Vec3::new(0.95, 0.25, 0.25), 0.28),
        CLUSTER => (Vec3::new(1.0, 0.9, 0.3), 0.2),
        BOMBLET => (Vec3::new(1.0, 0.9, 0.3), 0.11),
        SWARM => (Vec3::new(1.0, 0.4, 0.85), 0.11),
        TELEPORT => (Vec3::new(0.7, 0.5, 1.0), 0.2),
        NOVA => (Vec3::new(0.85, 0.95, 1.0), 0.55),
        _ => (Vec3::ONE, 0.1),
    }
}

/// The projectiles as capsules. `alpha` is how far the frame is into the
/// current tick (0..1), exactly as for the worms: a bolt moves a metre a tick,
/// so drawing it only where it was at the last tick would judder on a 144 Hz
/// display. A projectile flies straight (or nearly), so instead of storing its
/// last position, step back along its velocity.
pub fn projectiles(list: &[Projectile], alpha: f32) -> Vec<DrawCapsule> {
    list.iter()
        .map(|p| {
            let (colour, radius) = look(p.weapon);
            let at = p.pos - p.vel * DT * (1.0 - alpha);
            // The streak behind it says which way it goes. A bomb or a bomblet at
            // rest has no direction, so it is a short stub.
            let speed = p.vel.length();
            let tail = if speed > 0.5 { at - p.vel / speed * (speed * 0.03).clamp(0.0, 1.2) } else { at - Vec3::Y * 0.01 };
            DrawCapsule { a: tail, b: at, radius, colour, shadow: false }
        })
        .collect()
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
}

impl Effect {
    fn burst(shape: Shape, colour: Vec3, life: f32) -> Effect {
        Effect { shape, colour, vel: Vec3::ZERO, gravity: 0.0, age: 0.0, life, burst: true, width: 0.12 }
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
                self.push_burst(Effect::burst(
                    Shape::Ball { at: pos, radius },
                    Vec3::new(1.0, 0.62, 0.22),
                    0.25 + radius * 0.04,
                ));
                // A little dust thrown out by the blast, heavier than a landing.
                self.dust(pos + Vec3::Y * 0.2, 10.0);
            }
            Event::Beam { from, to } => {
                self.push_burst(Effect::burst(Shape::Line { from, to }, Vec3::new(0.6, 1.0, 1.0), 0.2));
            }
            Event::Shot { pos, .. } => {
                // The tracer's direction is known to `main`; a short hot ball at
                // the muzzle is enough from any angle and needs no extra state.
                self.push_burst(Effect::puff(pos, Vec3::ZERO, 0.18, HOT, 0.06, 0.0));
            }
            Event::Hit { pos, .. } => {
                self.hit(pos);
            }
            Event::Died { pos, .. } => {
                self.push_burst(Effect::burst(
                    Shape::Ball { at: pos + Vec3::Y * 0.9, radius: 1.3 },
                    Vec3::new(1.0, 1.0, 0.9),
                    0.45,
                ));
            }
            Event::Teleported { from, to, .. } => {
                for at in [from, to] {
                    self.push_burst(Effect::burst(
                        Shape::Ball { at: at + Vec3::Y * 0.9, radius: 1.0 },
                        Vec3::new(0.7, 0.5, 1.0),
                        0.3,
                    ));
                }
            }
            _ => {}
        }
    }

    /// A hit: a brief flash and a fan of sparks. `pos` is the point on the
    /// victim, so the sparks read as coming off the body, not the shooter.
    pub fn hit(&mut self, at: Vec3) {
        self.push_burst(Effect::puff(at, Vec3::ZERO, 0.16, Vec3::new(1.0, 0.98, 0.9), 0.07, 0.0));
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

    pub fn capsules(&self) -> Vec<DrawCapsule> {
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
                        DrawCapsule {
                            a: at,
                            b: at + Vec3::Y * 0.01,
                            radius: radius * size,
                            colour: e.colour,
                            shadow: false,
                        }
                    }
                    Shape::Line { from, to } => {
                        DrawCapsule { a: from, b: to, radius: e.width * (1.0 - u), colour: e.colour, shadow: false }
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

    #[test]
    fn a_shot_flashes_briefly() {
        let mut e = Effects::default();
        e.add(&Event::Shot { worm: 0, weapon: 0, pos: Vec3::ZERO });
        assert_eq!(count(&e), 1);
        e.update(0.07);
        assert_eq!(count(&e), 0, "a muzzle flash is over in a few frames");
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
        let at = e.capsules()[0].a;
        e.update(0.1);
        assert!((e.capsules()[0].a - at).length() > 0.001, "the puffs move");
        e.update(1.0);
        assert_eq!(count(&e), 0);
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
