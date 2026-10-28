// Lava: a gooey fluid made of particles that flows, cools and freezes to rock.
//
// The Lava Gun is a hose: while the trigger is held, a `LavaBit` leaves the
// muzzle every other tick. Each tick the bits fall, slide down whatever they
// land on, press apart from each other (so a puddle spreads as an even layer
// instead of stacking in one spot) and stick together a little (so it stays one
// gooey mass). As a bit cools it gets
// thicker; when its time is up and it rests on rock, it freezes: it stamps a
// smooth-added sphere into the terrain and is gone.
//
// The terrain edit is the same `Event::Edit` the Tractor sends, so clients need
// no new message to see the rock. The bits themselves are plain data in
// `World::lava`; a snapshot can carry the list later.
//
// The fluid model is the "double density relaxation" of Clavet, Beaudoin and
// Poulin (2005), a position-based cousin of SPH (smoothed particle
// hydrodynamics): every bit looks at the neighbours within `SMOOTH_H`, adds up a
// density with a smooth kernel, and pushes (or pulls) them along the line
// between them until the density is the rest density. No grid, no solver, and
// it stays stable at our fixed 60 Hz step.

use crate::combat::point_segment_distance;
use crate::world::{Event, World, BODY_SPHERES, DT, WORM_RADIUS};
use glam::Vec3;
use sdf::{Op, Shape};

/// One blob of lava. `life` is how long it stays molten (a little different
/// for each bit, so a puddle freezes in a ripple and not all at once).
#[derive(Clone, Copy, Debug)]
pub struct LavaBit {
    pub pos: Vec3,
    pub vel: Vec3,
    pub age: f32,
    pub life: f32,
    pub owner: u8,
}

impl LavaBit {
    /// 0 when fresh, 1 when it is ready to freeze. The viewer colours by this.
    pub fn cooled(&self) -> f32 {
        (self.age / self.life).min(1.0)
    }
}

/// Size of a bit against the terrain, in metres.
pub const LAVA_BIT_RADIUS: f32 = 0.42;
/// Reach of the smoothing kernel: bits farther apart than this ignore each other.
const SMOOTH_H: f32 = 1.1;
/// The density (kernel sum over the neighbours) the fluid wants. About what a
/// single layer of bits 0.7 m apart feels, so a puddle settles 1-2 bits thick.
const REST_DENSITY: f32 = 0.8;
const STIFFNESS: f32 = 500.0;
const NEAR_STIFFNESS: f32 = 1000.0;
/// Below the rest density the bits attract. This is the stickiness; kept weaker
/// than the push so the lava clumps without collapsing into one point.
const COHESION: f32 = 0.3;
/// The most one pair can be moved in a tick. Fresh bits start close together and
/// the first few ticks would otherwise fire them across the room.
const MAX_PUSH: f32 = 0.03;
/// Pair viscosity: bits moving towards each other lose speed.
const VISCOSITY: f32 = 25.0;
const LAVA_GRAVITY: f32 = 40.0;
/// Air drag, then extra drag as the bit cools (the lava thickens).
const DRAG: f32 = 0.4;
const COOL_DRAG: f32 = 9.0;
/// Rubbing on rock: lava crawls, it does not skid.
const FRICTION: f32 = 4.0;
/// Above the hose's muzzle speed, or the cap would be what stops the stream.
const MAX_SPEED: f32 = 50.0;
/// Bits alive at once, in the whole world. A shot that would exceed it is thinner.
pub const LAVA_MAX: usize = 300;
/// The frozen blob: slightly larger than the bit, and joined to its neighbours
/// and the rock with a generous smooth min, so a puddle freezes into one lumpy
/// surface instead of a pile of balls.
pub const STAMP_RADIUS: f32 = 0.6;
const STAMP_SMOOTH: f32 = 1.4;
const STAMP_SINK: f32 = 0.25;
/// Bits that may freeze in one tick. Each is a terrain edit to upload.
const FREEZE_PER_TICK: usize = 3;
/// A molten bit this long past its time and still in the air is dropped.
const LAVA_GIVE_UP: f32 = 6.0;
/// A bit leaves the muzzle touching the body of the worm that fired it. For this
/// many seconds it does not burn its own thrower; after that it is just lava.
const OWN_SAFE: f32 = 0.5;
/// How near the rock a bit must be to freeze onto it.
const FREEZE_REACH: f32 = 0.15;

impl World {
    /// Throw one bit of lava from `pos` with velocity `vel`, molten for about
    /// `seconds`. Each differs a little in how long it stays hot, so a puddle
    /// freezes in a ripple. Over `LAVA_MAX` the hose just stops.
    pub(crate) fn spawn_lava_bit(&mut self, pos: Vec3, vel: Vec3, seconds: f32, owner: u8) {
        if self.lava.len() >= LAVA_MAX {
            return;
        }
        let life = seconds * self.rng.range(0.85, 1.15);
        self.lava.push(LavaBit { pos, vel, age: 0.0, life, owner });
    }

    /// Every tick: flow, burn whoever is touched, freeze what is ready.
    pub(crate) fn step_lava(&mut self) {
        if self.lava.is_empty() {
            return;
        }
        let n = self.lava.len();

        // Forces on the velocity: gravity (which a gravity zone bends), and drag
        // that grows as the bit cools.
        for i in 0..n {
            let gravity = self.gravity_at(self.lava[i].pos);
            let b = &mut self.lava[i];
            b.age += DT;
            let cool = b.cooled();
            b.vel.y -= LAVA_GRAVITY * gravity * DT;
            b.vel *= 1.0 - (DRAG + COOL_DRAG * cool * cool) * DT;
        }

        // Viscosity: neighbours moving together drag each other along, which
        // is what makes it a goo and not a handful of marbles.
        for i in 0..n {
            for j in (i + 1)..n {
                let r = self.lava[j].pos - self.lava[i].pos;
                let d = r.length();
                if d >= SMOOTH_H || d < 1.0e-4 {
                    continue;
                }
                let dir = r / d;
                let approach = (self.lava[i].vel - self.lava[j].vel).dot(dir);
                if approach > 0.0 {
                    let impulse = dir * (DT * (1.0 - d / SMOOTH_H) * VISCOSITY * approach * 0.5);
                    self.lava[i].vel -= impulse;
                    self.lava[j].vel += impulse;
                }
            }
        }

        // Move.
        let before: Vec<Vec3> = self.lava.iter().map(|b| b.pos).collect();
        for b in &mut self.lava {
            b.vel = b.vel.clamp_length_max(MAX_SPEED);
            b.pos += b.vel * DT;
        }

        self.relax_lava();

        // Rock: push each bit out to its own radius. Its velocity then comes
        // from how far it really moved (position-based dynamics), which is what
        // turns "falling into the floor" into "sliding along it", and rubbing
        // on the rock slows it.
        for i in 0..n {
            let b = &mut self.lava[i];
            let d = self.grid.sample(b.pos);
            let mut touching = false;
            if d < LAVA_BIT_RADIUS {
                let normal = self.grid.gradient(b.pos);
                let normal = if normal == Vec3::ZERO { Vec3::Y } else { normal };
                b.pos += normal * (LAVA_BIT_RADIUS - d);
                touching = true;
            } else if d < LAVA_BIT_RADIUS + 0.05 {
                touching = true;
            }
            b.vel = ((b.pos - before[i]) / DT).clamp_length_max(MAX_SPEED);
            if touching {
                b.vel *= 1.0 - FRICTION * DT;
            }
        }

        self.scorch_with_lava();
        self.freeze_lava();
    }

    /// The fluid: double density relaxation (see the top of the file).
    fn relax_lava(&mut self) {
        let n = self.lava.len();
        for i in 0..n {
            let (mut density, mut near) = (0.0, 0.0);
            for j in 0..n {
                if j == i {
                    continue;
                }
                let q = self.lava[i].pos.distance(self.lava[j].pos) / SMOOTH_H;
                if q < 1.0 {
                    density += (1.0 - q) * (1.0 - q);
                    near += (1.0 - q) * (1.0 - q) * (1.0 - q);
                }
            }
            let mut pressure = STIFFNESS * (density - REST_DENSITY);
            if pressure < 0.0 {
                pressure *= COHESION;
            }
            let near_pressure = NEAR_STIFFNESS * near;
            let mut shift = Vec3::ZERO;
            for j in 0..n {
                if j == i {
                    continue;
                }
                let r = self.lava[j].pos - self.lava[i].pos;
                let d = r.length();
                let q = d / SMOOTH_H;
                if q >= 1.0 || d < 1.0e-4 {
                    continue;
                }
                let push = DT * DT * (pressure * (1.0 - q) + near_pressure * (1.0 - q) * (1.0 - q));
                let push = (r / d) * push.clamp(-MAX_PUSH, MAX_PUSH);
                // Equal and opposite: half on each of the pair.
                self.lava[j].pos += push * 0.5;
                shift -= push * 0.5;
            }
            self.lava[i].pos += shift;
        }
    }

    /// Molten rock burns whoever touches it, however briefly.
    fn scorch_with_lava(&mut self) {
        for j in 0..self.worms.len() {
            let worm = &self.worms[j];
            if !worm.alive {
                continue;
            }
            let touched = self.lava.iter().find(|b| {
                !(b.owner as usize == j && b.age < OWN_SAFE)
                    && BODY_SPHERES.iter().any(|&h| (worm.pos + Vec3::Y * h).distance(b.pos) < WORM_RADIUS + LAVA_BIT_RADIUS)
            });
            if let Some(b) = touched {
                let by = b.owner;
                self.ignite(j, by);
            }
        }
    }

    /// Cooled bits on rock turn into rock; the lost and the forgotten go away.
    fn freeze_lava(&mut self) {
        let top = self.grid.size_metres() + Vec3::splat(2.0);
        let mut frozen = 0;
        let mut i = 0;
        while i < self.lava.len() {
            let b = self.lava[i];
            let d = self.grid.sample(b.pos);
            let lost = b.pos.cmplt(Vec3::splat(-2.0)).any() || b.pos.cmpgt(top).any();
            let near_rock = d < LAVA_BIT_RADIUS + FREEZE_REACH;
            if lost || (b.age > b.life + LAVA_GIVE_UP && !near_rock) {
                self.lava.swap_remove(i);
                continue;
            }
            if b.age >= b.life && near_rock && frozen < FREEZE_PER_TICK && !self.worm_in_the_way(b.pos) {
                self.lava.swap_remove(i);
                self.stamp_lava(b.pos);
                frozen += 1;
                // A neighbour that the new rock swallowed freezes with it. Left
                // alone, the collision would shove it out of the rock at once
                // and spit it across the puddle.
                let mut j = 0;
                while j < self.lava.len() {
                    let o = self.lava[j];
                    if o.cooled() > 0.7 && self.grid.sample(o.pos) < LAVA_BIT_RADIUS * 0.7 && !self.worm_in_the_way(o.pos) {
                        self.lava.swap_remove(j);
                        self.stamp_lava(o.pos);
                    } else {
                        j += 1;
                    }
                }
                // The list was reshuffled: look at index `i` again.
                continue;
            }
            i += 1;
        }
    }

    /// Turn the lava at `pos` into rock: a smooth-added sphere, the same edit
    /// the Tractor sends.
    fn stamp_lava(&mut self, pos: Vec3) {
        // Sink the sphere a little into the rock under it: a bit rests a whole
        // bit-radius above the surface, and a ball centred there would make a
        // bump. Sunk, the layer freezes as a plateau with a rounded edge.
        let center = pos - self.grid.gradient(pos) * STAMP_SINK;
        let shape = Shape::Sphere { center, radius: STAMP_RADIUS };
        let op = Op::Smooth(STAMP_SMOOTH);
        self.grid.apply(shape, op);
        self.events.push(Event::Edit { shape, op });
        self.events.push(Event::LavaFroze { pos });
    }

    /// Rock on top of a worm would trap it (see `build_spot`), so lava waits
    /// until nobody is where it would set.
    fn worm_in_the_way(&self, at: Vec3) -> bool {
        self.worms.iter().any(|w| {
            let (a, b) = w.axis();
            w.alive && point_segment_distance(at, a, b) < WORM_RADIUS + STAMP_RADIUS + STAMP_SMOOTH * 0.25
        })
    }
}
