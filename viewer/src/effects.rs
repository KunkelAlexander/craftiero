// What a shot looks like: projectiles in flight, and the fireballs and beam lines
// that appear for a moment after an event. Like `worms.rs`, this is the only
// place that knows colours; the sim knows nothing about how any of it looks.
//
// Effects live only in the viewer. Nothing in the sim reads them, so they need
// no replay and no syncing: a client that joined a second ago simply shows
// fewer of them.

use crate::renderer::DrawCapsule;
use glam::Vec3;
use sim::weapons::{BLASTER, BOMB, BOMBLET, BOUNCER, CLUSTER, NOVA, ROCKET, SWARM, TELEPORT};
use sim::{Event, Projectile, DT};

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
    /// A fireball: grows to full size quickly, then shrinks away.
    Ball { at: Vec3, radius: f32 },
    /// A beam: a thin line that thins out.
    Line { from: Vec3, to: Vec3 },
}

struct Effect {
    shape: Shape,
    colour: Vec3,
    /// Seconds since it appeared, and how long it lasts.
    age: f32,
    life: f32,
}

#[derive(Default)]
pub struct Effects {
    list: Vec<Effect>,
}

impl Effects {
    /// Start whatever effect `event` calls for (most events have none).
    pub fn add(&mut self, event: &Event) {
        let effect = match *event {
            Event::Explosion { pos, radius } => Effect {
                shape: Shape::Ball { at: pos, radius },
                colour: Vec3::new(1.0, 0.62, 0.22),
                age: 0.0,
                life: 0.25 + radius * 0.04,
            },
            Event::Beam { from, to } => {
                Effect { shape: Shape::Line { from, to }, colour: Vec3::new(0.6, 1.0, 1.0), age: 0.0, life: 0.2 }
            }
            Event::Died { pos, .. } => Effect {
                shape: Shape::Ball { at: pos + Vec3::Y * 0.9, radius: 1.3 },
                colour: Vec3::new(1.0, 1.0, 0.9),
                age: 0.0,
                life: 0.45,
            },
            Event::Teleported { from, to, .. } => {
                for at in [from, to] {
                    self.list.push(Effect {
                        shape: Shape::Ball { at: at + Vec3::Y * 0.9, radius: 1.0 },
                        colour: Vec3::new(0.7, 0.5, 1.0),
                        age: 0.0,
                        life: 0.3,
                    });
                }
                return;
            }
            _ => return,
        };
        self.list.push(effect);
    }

    pub fn update(&mut self, dt: f32) {
        for e in &mut self.list {
            e.age += dt;
        }
        self.list.retain(|e| e.age < e.life);
    }

    pub fn capsules(&self) -> Vec<DrawCapsule> {
        self.list
            .iter()
            .map(|e| {
                let u = (e.age / e.life).clamp(0.0, 1.0);
                match e.shape {
                    Shape::Ball { at, radius } => {
                        // Up to full size in the first third, then down to nothing.
                        let size = if u < 0.35 { u / 0.35 } else { 1.0 - (u - 0.35) / 0.65 };
                        // A capsule needs a direction: a stub makes a sphere.
                        DrawCapsule { a: at, b: at + Vec3::Y * 0.01, radius: radius * size, colour: e.colour, shadow: false }
                    }
                    Shape::Line { from, to } => {
                        DrawCapsule { a: from, b: to, radius: 0.12 * (1.0 - u), colour: e.colour, shadow: false }
                    }
                }
            })
            .collect()
    }
}
