// Everything that hurts a worm or reshapes the terrain on purpose: firing,
// projectiles in flight, explosions, damage, dying and coming back. The rows of
// numbers are in `weapons.rs`; this file is the few rules that read them.
//
// These are methods of `World` (an `impl` block may live in any module of the
// crate), so `world.rs` stays about movement.

use crate::input::Input;
use crate::rng::Rng;
use crate::weapons::{Kind, Special, WeaponDef, WeaponState, ARSENAL, WEAPONS};
use crate::world::{body_axis, body_fits, Event, World, DT, WORM_HEIGHT, WORM_RADIUS};
use glam::{Quat, Vec3};
use sdf::{Op, Shape};

pub const MAX_HP: f32 = 100.0;
pub const RESPAWN_TIME: f32 = 3.0;
const SPAWN_INVULN: f32 = 2.0;
/// You hurt yourself half as much as others, but get thrown just as far, so
/// explosions work as a movement tool (rocket jumping) without being free.
const SELF_DAMAGE: f32 = 0.5;
/// A projectile is a point with this radius. It touches rock when the distance
/// field says it is closer than this, and a worm when it is this close to the
/// worm's skin.
const PROJECTILE_RADIUS: f32 = 0.12;
/// Largest move between two rock checks. A fast bolt moves a metre per tick, and
/// a thin ridge of rock is a few voxels (25 cm) thick: checking only the end of
/// the tick would let the bolt jump over it ("tunnelling").
const PROJECTILE_STEP: f32 = 0.25;
/// A beam hits a worm whose body is this close to its line, not only one it
/// crosses. Networking note: the host sees worms where they are now, but a
/// joiner aims at a picture that is ~100 ms of interpolation plus their own lag
/// in the past. An instant beam exactly as wide as a body would land far more
/// often for the host than for friends; a little slack evens that out. (The
/// full answer is lag compensation: the host rewinds to what the shooter saw.)
const BEAM_SLACK: f32 = 0.3;
/// A blast pushes up as well as away, by this much of the push. Without it a
/// worm standing on the ground gets pushed into the ground, which cancels the
/// push: a rocket at your feet should launch you, and a rocket beside you
/// should at least lift you off the floor, where friction would eat the speed.
const KNOCK_LIFT: f32 = 0.35;
/// How far along the barrel from the hand a shot starts. Deliberately inside
/// the drawn barrel: the gun hides it until it reaches the muzzle, so the bolt
/// is first seen leaving the tip.
const MUZZLE: f32 = 0.8;
/// The muzzle flash is placed further along the barrel than the shot starts, so
/// it sits at the tip instead of inside the barrel (where the gun would hide it).
const FLASH_AHEAD: f32 = 0.5;
/// Build lumps join the terrain with a rounded crease this wide, like clay.
const BUILD_SMOOTH: f32 = 0.4;
/// Extra room around a worm (and a spawn point) that a lump must leave free,
/// because the rounded join adds a little material beyond the lump's radius.
const BUILD_MARGIN: f32 = 0.15;
/// A bouncing projectile slower than this comes to rest (a dropped bomb).
const RESTING_SPEED: f32 = 1.0;

#[derive(Clone, Debug)]
pub struct Projectile {
    pub pos: Vec3,
    pub vel: Vec3,
    /// Index into `WEAPONS`.
    pub weapon: u8,
    /// Index of the worm that fired it.
    pub owner: u8,
    /// Seconds left to live.
    pub life: f32,
}

/// Where a lobbed shot will go, for the aim aid on screen. `points` is the
/// flight path (a point every few ticks) and `hit` is where it will land, with
/// the rock's `normal` there so the marker can lie on the slope.
#[derive(Clone, Debug, Default)]
pub struct AimPath {
    pub points: Vec<Vec3>,
    pub hit: Option<Vec3>,
    pub normal: Vec3,
}

impl World {
    /// Weapon timers, the weapon choice and the trigger of worm `i` (alive).
    pub(crate) fn step_weapons(&mut self, i: usize, input: &Input) {
        let worm = &mut self.worms[i];
        worm.invuln = (worm.invuln - DT).max(0.0);
        for w in &mut worm.weapons {
            w.cooldown = (w.cooldown - DT).max(0.0);
            // Each weapon reloads on its own timer, so you can switch away and
            // it keeps reloading.
            if w.reload > 0.0 {
                w.reload -= DT;
                if w.reload <= 0.0 {
                    w.reload = 0.0;
                    w.ammo = WEAPONS[w.id as usize].mag;
                }
            }
        }
        if let Some(slot) = worm.weapons.iter().position(|w| w.id == input.weapon) {
            worm.current = slot;
        }
        if input.fire {
            self.try_fire(i);
        }
    }

    /// A dead worm counts down to its respawn.
    pub(crate) fn step_dead(&mut self, i: usize) {
        self.worms[i].respawn_timer -= DT;
        if self.worms[i].respawn_timer <= 0.0 {
            self.respawn(i);
        }
    }

    /// Where worm `i`'s next shot leaves the gun, and which way it goes.
    ///
    /// The camera sits over the shoulder, so a shot aimed along the eye ray
    /// would stay a shoulder-width off the reticle. The gun therefore starts
    /// out at the hand and fires *at* the point under the crosshair, with the
    /// correction faded out at point-blank range.
    ///
    /// Shared with `aim_path`, so the trajectory the player sees is the path
    /// the shot will really take.
    fn shot_plan(&self, i: usize) -> (Vec3, Vec3) {
        let worm = &self.worms[i];
        let aim = worm.aim_dir();
        let eye = worm.eye();
        // How far the eye ray reaches before it hits rock. With nothing in
        // reach, the crosshair is far enough away that the correction is nil.
        let range = self.grid.raycast(eye, aim, 100.0).unwrap_or(100.0);
        let crosshair = eye + aim * range;
        // Aiming almost straight down at your own feet is the one case where
        // starting at the hand is wrong: the blast lands level with your foot and
        // beside you, so it shoves you sideways instead of up and a rocket jump
        // stops working. Fall back to the body's axis for that shot.
        let steep = -aim.y > 0.9 && range < 3.0;
        let hand = if steep { worm.hand() } else { worm.muzzle() };
        let muzzle = hand + aim * MUZZLE;
        let want = (crosshair - muzzle).normalize_or_zero();
        // Under 3 m the points are too close for convergence to make sense (a
        // rocket at your own feet would leave sideways), so fade it out.
        let blend = ((range - 1.0) / 2.0).clamp(0.0, 1.0);
        let shot = aim.lerp(want, blend).normalize_or_zero();
        let shot = if shot.length_squared() > 0.5 { shot } else { aim };
        (muzzle, shot)
    }

    /// The arc a lobbed shot will follow, and where it will land.
    ///
    /// Only weapons that are actually thrown in an arc report one: a blaster
    /// bolt or a rocket flies flat, and in R&C those get no trajectory either.
    /// The flight is stepped exactly as `step_projectiles` does it (minus the
    /// random spread), so the landing mark lands on the rock the shot will hit.
    pub fn aim_path(&self, i: usize) -> Option<AimPath> {
        let worm = self.worms.get(i)?;
        if !worm.alive {
            return None;
        }
        let id = worm.weapons[worm.current].id;
        let def = &WEAPONS[id as usize];
        // A lobbed shell, not a flat shot and not a bouncer: the arc is only
        // honest for a weapon that lands where it stops. (R&C draws this for
        // the lobbed weapons, and leaves the straight ones to the crosshair.)
        let lobbed = def.gravity > 0.0 && def.speed > 0.0 && def.speed <= 30.0;
        if def.kind != Kind::Projectile || !lobbed {
            return None;
        }
        let (muzzle, shot) = self.shot_plan(i);
        let mut pos = muzzle;
        let mut vel = shot * def.speed;
        let mut points = Vec::new();
        let mut hit = None;
        let ticks = (def.lifetime / DT).ceil() as usize;
        'flight: for _ in 0..ticks {
            vel.y -= def.gravity * DT;
            // The same sub-stepping the real projectile uses, so a fast shell
            // cannot tunnel through a thin wall in the preview but not in play.
            let n = ((vel.length() * DT) / PROJECTILE_STEP).ceil().max(1.0) as i32;
            let step = vel * DT / n as f32;
            for _ in 0..n {
                pos += step;
                if self.grid.sample(pos) < PROJECTILE_RADIUS {
                    hit = Some(pos);
                    break 'flight;
                }
            }
            // One point per tick: the viewer thins them to a fixed handful, so
            // a short lob still gets enough dots to read as a curve.
            points.push(pos);
        }
        let normal = match hit {
            Some(p) => {
                // The dots have to reach the mark, or the last one (up to three
                // ticks back) leaves a visible gap before the ring.
                points.push(p);
                let n = self.grid.gradient(p);
                if n.length_squared() > 1.0e-6 { n.normalize() } else { Vec3::Y }
            }
            None => Vec3::Y,
        };
        Some(AimPath { points, hit, normal })
    }

    fn try_fire(&mut self, i: usize) {
        let worm = &self.worms[i];
        let w = &worm.weapons[worm.current];
        if w.cooldown > 0.0 || w.reload > 0.0 || w.ammo == 0 {
            return;
        }
        let id = w.id;
        let def = &WEAPONS[id as usize];
        let (barrel, shot) = self.shot_plan(i);
        // Find the build spot before spending a charge: aiming at a wall too
        // close, or at a worm, should not cost you a lump.
        let build_at = match def.kind {
            Kind::Build => match self.build_spot(i, def) {
                Some(spot) => Some(spot),
                None => return,
            },
            _ => None,
        };

        // Shooting gives up spawn protection: it is there to let you get your
        // bearings, not to fight for two seconds without being hurt.
        let worm = &mut self.worms[i];
        worm.invuln = 0.0;
        let feet = worm.pos;
        let w = &mut worm.weapons[worm.current];
        w.ammo -= 1;
        w.cooldown = def.cooldown;
        if w.ammo == 0 {
            w.reload = def.reload; // auto-reload as soon as the magazine empties
        }

        // A weapon with no speed (the bomb) is dropped at your feet, not thrown.
        // A lump "fires" where it appears, so the flash marks the new rock.
        let muzzle = match build_at {
            Some(at) => at,
            None if def.kind == Kind::Projectile && def.speed == 0.0 => feet + Vec3::Y * 0.5,
            None => barrel,
        };
        self.events.push(Event::Shot { worm: i as u8, weapon: id, pos: muzzle + shot * FLASH_AHEAD });

        match def.kind {
            // A swarm is one shot that makes several projectiles, each with
            // its own random error.
            Kind::Projectile => {
                for _ in 0..def.pellets {
                    let dir = spread_dir(&mut self.rng, shot, def.spread);
                    self.projectiles.push(Projectile {
                        pos: muzzle,
                        vel: dir * def.speed,
                        weapon: id,
                        owner: i as u8,
                        life: def.lifetime,
                    });
                }
            }
            Kind::Beam => {
                let dir = spread_dir(&mut self.rng, shot, def.spread);
                self.fire_beam(i, def, muzzle, dir);
            }
            // A lump is an ordinary terrain edit, so it needs no new rules,
            // drawing or network message: joiners learn about it from the same
            // `Edit` event that reports a crater.
            Kind::Build => {
                if let Some(at) = build_at {
                    let shape = Shape::Sphere { center: at, radius: def.terrain };
                    let op = Op::Smooth(BUILD_SMOOTH);
                    self.grid.apply(shape, op);
                    self.events.push(Event::Edit { shape, op });
                }
            }
        }
    }

    /// Where a Build weapon puts its lump: on the first rock along the aim
    /// (sunk in by half its radius so it joins the surface), or floating at the
    /// end of the range (a stepping stone). None if the lump would touch a
    /// worm (rock on top of someone would trap them) or a spawn point (or a
    /// spawn could be walled up and a worm respawn inside it).
    fn build_spot(&self, i: usize, def: &WeaponDef) -> Option<Vec3> {
        let worm = &self.worms[i];
        let (eye, dir) = (worm.eye(), worm.aim_dir());
        let r = def.terrain;
        let at = match self.grid.raycast(eye, dir, def.range) {
            Some(t) => eye + dir * (t - r * 0.5).max(0.0),
            None => eye + dir * def.range,
        };
        let room = r + WORM_RADIUS + BUILD_MARGIN;
        let touches = |(a, b): (Vec3, Vec3)| point_segment_distance(at, a, b) < room;
        let near_worm = self.worms.iter().any(|w| w.alive && touches(w.axis()));
        let near_spawn = self.spawns.iter().any(|&s| touches(body_axis(s)));
        if near_worm || near_spawn {
            None
        } else {
            Some(at)
        }
    }

    /// Hitscan: no flying object, the line is resolved instantly.
    fn fire_beam(&mut self, owner: usize, def: &WeaponDef, origin: Vec3, dir: Vec3) {
        // 1) The beam ends at the first rock, and leaves it whole.
        let reach = self.grid.raycast(origin, dir, def.range).unwrap_or(def.range);
        let end = origin + dir * reach;
        self.events.push(Event::Beam { from: origin, to: end });

        // 2) Every worm within reach of that line gets hit: the beam pierces,
        //    so this is not a "first hit only" test. The body is three points
        //    (feet, middle, head); they are closer together than the body is wide.
        for v in 0..self.worms.len() {
            if v == owner || !self.worms[v].alive {
                continue;
            }
            let (a, b) = self.worms[v].axis();
            let nearest = [a, (a + b) * 0.5, b]
                .iter()
                .map(|&p| point_segment_distance(p, origin, end))
                .fold(f32::MAX, f32::min);
            if nearest < WORM_RADIUS + BEAM_SLACK {
                self.damage(v, def.damage, owner, dir * def.knockback);
            }
        }
    }

    fn respawn(&mut self, i: usize) {
        // The spawn point farthest from living enemies, with a little random
        // jitter so ties are not always broken the same way.
        let enemies: Vec<Vec3> =
            (0..self.worms.len()).filter(|&j| j != i && self.worms[j].alive).map(|j| self.worms[j].pos).collect();
        let mut best = (f32::MIN, self.spawns[0]);
        for k in 0..self.spawns.len() {
            let s = self.spawns[k];
            let nearest = enemies.iter().map(|e| e.distance(s)).fold(f32::MAX, f32::min);
            let score = nearest.min(60.0) + self.rng.range(0.0, 2.0);
            if score > best.0 {
                best = (score, s);
            }
        }
        let worm = &mut self.worms[i];
        worm.reset_motion(best.1);
        worm.hp = MAX_HP;
        worm.alive = true;
        worm.invuln = SPAWN_INVULN;
        worm.weapons = ARSENAL.iter().map(|&id| WeaponState::full(id)).collect();
        self.events.push(Event::Respawned { worm: i as u8 });
    }

    /// The single place where a worm loses health. Funnelling every source of
    /// damage through one function means self-damage, spawn protection, kill
    /// credit and scoring live in exactly one spot.
    fn damage(&mut self, victim: usize, amount: f32, source: usize, impulse: Vec3) {
        let worm = &mut self.worms[victim];
        if !worm.alive || worm.invuln > 0.0 {
            return;
        }
        let amount = if victim == source { amount * SELF_DAMAGE } else { amount };
        worm.hp -= amount;
        // Knockback is a real impulse; the movement code must not overwrite it
        // next tick, so `knockback` puts the worm into a tumble.
        if impulse != Vec3::ZERO {
            worm.knockback(impulse);
        }
        let pos = worm.pos;
        if amount > 0.0 {
            self.events.push(Event::Hit { victim: victim as u8, pos, damage: amount });
        }
        if worm.hp <= 0.0 {
            worm.alive = false;
            worm.hp = 0.0;
            worm.respawn_timer = RESPAWN_TIME;
            worm.deaths += 1;
            worm.rope = None;
            // Killing yourself costs a point; killing someone else earns one.
            self.worms[source].kills += if victim == source { -1 } else { 1 };
            self.events.push(Event::Died { victim: victim as u8, killer: source as u8, pos });
        }
    }

    // ------------------------------------------------------------ projectiles

    pub(crate) fn step_projectiles(&mut self) {
        // Take the list out so we can call `&mut self` methods while looping,
        // then put the survivors back.
        let old = std::mem::take(&mut self.projectiles);
        let mut keep = Vec::with_capacity(old.len());
        'next: for mut pr in old {
            let def = &WEAPONS[pr.weapon as usize];
            pr.life -= DT;
            if let Special::Homing(turn) = def.special {
                self.steer(&mut pr, turn);
            }
            pr.vel.y -= def.gravity * DT;

            // A Cluster shell flies through worms and bursts only at rock or at
            // the end of its fuse. Bursting on someone put all seven bomblets on
            // them at once: a one-shot kill up close, which is not what an area
            // weapon is for.
            let passes_worms = matches!(def.special, Special::Cluster(..));
            let n = ((pr.vel.length() * DT) / PROJECTILE_STEP).ceil().max(1.0) as i32;
            let mut step = pr.vel * DT / n as f32;
            for _ in 0..n {
                pr.pos += step;
                if self.grid.sample(pr.pos) < PROJECTILE_RADIUS {
                    if def.bounce == 0.0 {
                        self.impact(&pr, def, None);
                        continue 'next;
                    }
                    // Ricochet: undo the step, mirror the velocity in the surface
                    // there (the distance field's gradient IS the surface normal)
                    // and lose a part of the speed.
                    let normal = self.grid.gradient(pr.pos);
                    pr.pos -= step;
                    let normal = if normal == Vec3::ZERO { -step.normalize_or_zero() } else { normal };
                    let into = pr.vel.dot(normal);
                    if into < 0.0 {
                        pr.vel -= normal * (2.0 * into);
                    }
                    pr.vel *= def.bounce;
                    if pr.vel.length() < RESTING_SPEED {
                        pr.vel = Vec3::ZERO;
                    }
                    step = pr.vel * DT / n as f32;
                    // (No `continue`: a bomb at rest is always touching the ground,
                    // and must still go off when a worm touches it.)
                }
                let hit = (0..self.worms.len()).find(|&j| {
                    let worm = &self.worms[j];
                    let (a, b) = worm.axis();
                    !passes_worms
                        && j != pr.owner as usize
                        && worm.alive
                        && point_segment_distance(pr.pos, a, b) < WORM_RADIUS + PROJECTILE_RADIUS
                });
                if let Some(j) = hit {
                    self.impact(&pr, def, Some(j));
                    continue 'next;
                }
            }

            if pr.life <= 0.0 {
                // The end of the flight is an impact with nothing: shells burst,
                // orbs land and rockets blow up where they are.
                if def.blast_radius > 0.0 || matches!(def.special, Special::Cluster(..) | Special::Teleport) {
                    self.impact(&pr, def, None);
                }
                continue;
            }
            keep.push(pr);
        }
        // A burst this tick put its children into the (emptied) list: keep them.
        keep.append(&mut self.projectiles);
        self.projectiles = keep;
    }

    /// Homing: turn the velocity towards the nearest living enemy, by at most
    /// `turn` radians per second. The speed never changes, only the heading, so
    /// a missile can be out-run around a corner and it still dies on rock.
    ///
    /// (The rotation uses sin and cos, which are not guaranteed bit-identical
    /// across CPUs; see the note at the top of `world.rs`.)
    fn steer(&self, pr: &mut Projectile, turn: f32) {
        let target = (0..self.worms.len())
            .filter(|&j| j != pr.owner as usize && self.worms[j].alive)
            .map(|j| {
                let (a, b) = self.worms[j].axis();
                (a + b) * 0.5
            })
            .min_by(|a, b| a.distance_squared(pr.pos).total_cmp(&b.distance_squared(pr.pos)));
        let Some(target) = target else { return };
        let speed = pr.vel.length();
        if speed < 1.0e-3 {
            return;
        }
        let have = pr.vel / speed;
        let want = (target - pr.pos).normalize_or_zero();
        let axis = have.cross(want);
        let angle = axis.length().atan2(have.dot(want)); // 0 ..= PI, whichever way round
        if angle < 1.0e-4 {
            return;
        }
        // Straight away from the target: any axis turns it, pick one.
        let axis = if axis.length() > 1.0e-4 { axis.normalize() } else { have.any_orthonormal_vector() };
        pr.vel = Quat::from_axis_angle(axis, angle.min(turn * DT)) * have * speed;
    }

    /// The nearest spot to `pos`, walking back along `-dir`, where a sphere of
    /// `radius` fits. A projectile that hit rock is a few centimetres inside it.
    fn free_point(&self, pos: Vec3, dir: Vec3, radius: f32) -> Option<Vec3> {
        (0..20).map(|k| pos - dir * (0.1 * k as f32)).find(|&p| self.grid.sample(p) >= radius)
    }

    /// Cluster: the shell splits into `count` children flying off in random
    /// directions. The randomness comes from the sim's own `rng`, so every
    /// machine bursts the same way.
    fn burst(&mut self, pr: &Projectile, dir: Vec3, count: u8, child: u8) {
        let def = &WEAPONS[child as usize];
        let at = self.free_point(pr.pos, dir, PROJECTILE_RADIUS).unwrap_or(pr.pos);
        for _ in 0..count {
            let d = random_direction(&mut self.rng);
            self.projectiles.push(Projectile {
                pos: at,
                vel: d * def.speed * self.rng.range(0.6, 1.0),
                weapon: child,
                owner: pr.owner,
                life: def.lifetime * self.rng.range(0.7, 1.0),
            });
        }
    }

    /// Teleport: the owner appears where the orb landed, or swaps places with
    /// the worm it hit. If the orb landed in a spot too tight to stand in,
    /// nothing happens (a fizzle), which is better than getting stuck in rock.
    fn teleport(&mut self, pr: &Projectile, dir: Vec3, victim: Option<usize>) {
        let owner = pr.owner as usize;
        if !self.worms[owner].alive {
            return; // shot, then died before it landed
        }
        let from = self.worms[owner].pos;
        let to = match victim {
            Some(v) => {
                let to = self.worms[v].pos;
                self.worms[v].reset_motion(from);
                Some(to)
            }
            // Centre the body on the orb, then back off along the flight until it fits.
            None => (0..16)
                .map(|k| pr.pos - dir * (0.15 * k as f32) - Vec3::Y * (WORM_HEIGHT * 0.5))
                .find(|&feet| body_fits(&self.grid, feet)),
        };
        let Some(to) = to else { return };
        self.worms[owner].reset_motion(to);
        self.events.push(Event::Teleported { worm: owner as u8, from, to });
    }

    /// A projectile ended its flight against rock or a worm.
    fn impact(&mut self, pr: &Projectile, def: &WeaponDef, victim: Option<usize>) {
        let dir = pr.vel.normalize_or_zero();
        match def.special {
            Special::Cluster(count, child) => return self.burst(pr, dir, count, child),
            Special::Teleport => return self.teleport(pr, dir, victim),
            _ => {}
        }
        if def.blast_radius > 0.0 {
            // A shell that hit rock is a few centimetres inside it. Blow it up on
            // the side it came from, so the rock it hit is between the blast
            // and whoever hides behind it.
            let at = self.free_point(pr.pos, dir, PROJECTILE_RADIUS).unwrap_or(pr.pos);
            self.explode(at, def, pr.owner as usize);
            return;
        }
        if let Some(v) = victim {
            self.damage(v, def.damage, pr.owner as usize, dir * def.knockback);
        }
    }

    /// Damage falls off linearly with distance, and only reaches worms who
    /// can see the blast: rock is cover. That is what makes a built lump an
    /// answer to a rocket or a Nova. Worms are hurt before the crater is cut,
    /// so the rock that a blast destroys still shields you from that blast.
    fn explode(&mut self, pos: Vec3, def: &WeaponDef, owner: usize) {
        self.events.push(Event::Explosion { pos, radius: def.blast_radius });

        for v in 0..self.worms.len() {
            if !self.worms[v].alive {
                continue;
            }
            // Distance to the body's axis, minus its radius: close to a capsule's
            // true skin distance.
            let (a, b) = self.worms[v].axis();
            let nearest = closest_point_on_segment(pos, a, b);
            let d = pos.distance(nearest);
            let reach = def.blast_radius + WORM_RADIUS;
            if d >= reach {
                continue;
            }
            // Is there rock between the blast and the nearest part of the worm?
            if d > 0.05 && self.grid.raycast(pos, (nearest - pos) / d, d).is_some() {
                continue;
            }
            let falloff = 1.0 - d / reach;
            let away = ((a + b) * 0.5 - pos).normalize_or_zero() + Vec3::Y * KNOCK_LIFT;
            self.damage(v, def.blast_damage * falloff, owner, away.normalize() * def.knockback * falloff);
        }

        if def.terrain > 0.0 {
            let shape = Shape::Sphere { center: pos, radius: def.terrain };
            self.grid.apply(shape, Op::Carve);
            self.events.push(Event::Edit { shape, op: Op::Carve });
        }
    }
}

/// `dir` turned by a random angle up to `spread`: a point in a square around the
/// aim at distance 1, pushed out sideways by up to tan(spread). (Not a perfect
/// cone, which nobody can feel.)
fn spread_dir(rng: &mut Rng, dir: Vec3, spread: f32) -> Vec3 {
    if spread == 0.0 {
        return dir;
    }
    let reference = if dir.y.abs() > 0.99 { Vec3::X } else { Vec3::Y };
    let right = dir.cross(reference).normalize();
    let up = right.cross(dir);
    let k = spread.tan();
    (dir + right * rng.range(-k, k) + up * rng.range(-k, k)).normalize()
}

/// A random unit vector: pick points in a cube until one lands inside the ball
/// (which makes the direction uniform), then scale to length 1.
fn random_direction(rng: &mut Rng) -> Vec3 {
    for _ in 0..16 {
        let p = Vec3::new(rng.range(-1.0, 1.0), rng.range(-1.0, 1.0), rng.range(-1.0, 1.0));
        if p.length_squared() > 0.01 && p.length_squared() <= 1.0 {
            return p.normalize();
        }
    }
    Vec3::Y
}

fn closest_point_on_segment(p: Vec3, a: Vec3, b: Vec3) -> Vec3 {
    let ab = b - a;
    let len2 = ab.length_squared();
    let t = if len2 == 0.0 { 0.0 } else { ((p - a).dot(ab) / len2).clamp(0.0, 1.0) };
    a + ab * t
}

fn point_segment_distance(p: Vec3, a: Vec3, b: Vec3) -> f32 {
    p.distance(closest_point_on_segment(p, a, b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::weapons::{BLASTER, BOMB, BOMBLET, BOUNCER, BUILD, CLUSTER, NOVA, RAILGUN, ROCKET, SWARM, TELEPORT, TRACTOR};
    use glam::IVec3;
    use sdf::Grid;

    // Headless like the movement tests: a few worms in a 32 x 16 x 32 m box and a
    // list of inputs. Aiming is by angles; worm 0 looks along +z at yaw 0, and
    // `LEVEL` is the pitch that aims from its eye at the middle of a worm 10 m
    // ahead.
    const SIZE: IVec3 = IVec3::new(8, 4, 8);
    const LEVEL: f32 = -0.06;

    fn flat(p: Vec3) -> f32 {
        p.y - 4.0
    }

    /// Flat ground and a wall of rock beyond z = 24.
    fn walled(p: Vec3) -> f32 {
        (p.y - 4.0).min(24.0 - p.z)
    }

    /// Flat ground and a 2 m thick slab of rock across z = 20..22.
    fn slab(p: Vec3) -> f32 {
        (p.y - 4.0).min((p.z - 21.0).abs() - 1.0)
    }

    /// Worms standing on the given `(x, z)` columns, landed and still.
    fn arena(f: fn(Vec3) -> f32, spawns: &[(f32, f32)]) -> World {
        let mut w = World::new(Grid::from_fn(SIZE, f), spawns, 1);
        run(&mut w, 30);
        w
    }

    /// Step with no input and return the events of those ticks.
    fn run(w: &mut World, ticks: usize) -> Vec<Event> {
        let mut events = Vec::new();
        for _ in 0..ticks {
            w.step(&[]);
            events.extend(w.drain_events());
        }
        events
    }

    /// Worm 0 pulls the trigger of `weapon` for one tick, looking along (yaw, pitch).
    fn shoot_at(w: &mut World, weapon: u8, yaw: f32, pitch: f32) -> Vec<Event> {
        w.step(&[Input { fire: true, weapon, yaw, pitch, ..Input::default() }]);
        w.drain_events()
    }

    fn shoot(w: &mut World, weapon: u8, pitch: f32) -> Vec<Event> {
        shoot_at(w, weapon, 0.0, pitch)
    }

    fn explosions(events: &[Event]) -> usize {
        events.iter().filter(|e| matches!(e, Event::Explosion { .. })).count()
    }

    fn duel() -> World {
        arena(flat, &[(16.0, 16.0), (16.0, 26.0)])
    }

    #[test]
    fn a_blaster_bolt_hurts_the_worm_it_hits_and_only_that() {
        let mut w = duel();
        let mut events = shoot(&mut w, BLASTER, LEVEL);
        events.extend(run(&mut w, 30));
        assert!((w.worms[1].hp - 90.0).abs() < 1.0e-3, "a hit is 10 damage: {}", w.worms[1].hp);
        assert!(events.iter().any(|e| matches!(e, Event::Hit { victim: 1, .. })));

        let mut w = duel();
        shoot_at(&mut w, BLASTER, 1.0, LEVEL); // aimed well to the side
        run(&mut w, 30);
        assert_eq!(w.worms[1].hp, MAX_HP);
    }

    #[test]
    fn a_cluster_shows_its_arc_and_where_it_will_land() {
        let mut w = arena(flat, &[(16.0, 16.0)]);
        // Aim slightly down, so the shell arcs into the ground in front of the
        // worm rather than flying over the arena wall.
        w.worms[0].pitch = -0.18;
        w.worms[0].current = CLUSTER as usize;
        let path = w.aim_path(0).expect("a lobbed weapon has a path");
        assert!(path.points.len() >= 4, "an arc, not two dots: {}", path.points.len());
        let hit = path.hit.expect("aiming at the ground: it lands");
        assert!((hit.y - 4.0).abs() < 0.3, "the mark is on the ground: {hit:?}");
        let last = *path.points.last().unwrap();
        assert!((last - hit).length() < 0.15, "the dots lead to the mark: {last:?} {hit:?}");
        assert!(path.normal.y > 0.5, "flat ground points up: {:?}", path.normal);

        // The mark is where the shell really bursts (the cluster backs off a
        // little so its children do not start inside the rock).
        let mut burst = None;
        for _ in 0..90 {
            w.step(&[Input { fire: burst.is_none(), weapon: CLUSTER, pitch: -0.18, ..Input::default() }]);
            w.drain_events();
            if let Some(b) = w.projectiles.iter().find(|p| p.weapon == BOMBLET) {
                burst = Some(b.pos);
                break;
            }
        }
        let burst = burst.expect("the shell bursts");
        assert!((burst - hit).length() < 2.5, "the preview lands where the shot does: {burst:?} {hit:?}");
    }

    #[test]
    fn a_lob_aimed_at_a_wall_marks_the_wall() {
        let mut w = arena(walled, &[(16.0, 16.0)]);
        w.worms[0].current = CLUSTER as usize;
        let path = w.aim_path(0).expect("a lobbed weapon has a path");
        let hit = path.hit.expect("the wall is in the way");
        assert!(hit.z > 23.0, "the mark is on the wall: {hit:?}");
        assert!(path.normal.z < -0.5, "the ring faces back at the worm: {:?}", path.normal);
    }

    #[test]
    fn flat_shots_get_no_trajectory() {
        let mut w = arena(flat, &[(16.0, 16.0)]);
        for id in [BLASTER, ROCKET, RAILGUN, BOUNCER, BOMB, SWARM] {
            w.worms[0].current = id as usize;
            assert!(w.aim_path(0).is_none(), "weapon {id} flies flat or is dropped");
        }
        w.worms[0].current = CLUSTER as usize;
        assert!(w.aim_path(0).is_some(), "but a lobbed one has an arc");
    }

    #[test]
    fn a_rocket_carves_a_crater_where_it_lands() {
        let mut w = arena(walled, &[(16.0, 16.0)]);
        let inside_the_crater = Vec3::new(16.0, 5.5, 25.5);
        assert!(w.grid.sample(inside_the_crater) < 0.0, "solid rock before");
        w.grid.take_dirty();
        let mut events = shoot(&mut w, ROCKET, 0.0);
        events.extend(run(&mut w, 40));
        assert!(w.grid.sample(inside_the_crater) > 0.0, "a hole after");
        assert!(!w.grid.take_dirty().is_empty(), "the renderer is told which chunks changed");
        assert_eq!(explosions(&events), 1);
        assert!(events.iter().any(|e| matches!(e, Event::Edit { op: Op::Carve, .. })), "and the edit is reported");
        assert_eq!(w.worms[0].hp, MAX_HP, "8 m away is out of reach of a 3 m blast");
        assert!(w.projectiles.is_empty());
    }

    #[test]
    fn a_rocket_at_your_own_feet_hurts_half_and_launches_you() {
        let mut w = arena(flat, &[(16.0, 16.0)]);
        let ground = w.worms[0].pos.y;
        shoot(&mut w, ROCKET, -1.55); // straight down
        let mut top: f32 = 0.0;
        for _ in 0..120 {
            w.step(&[]);
            top = top.max(w.worms[0].pos.y - ground);
        }
        let hp = w.worms[0].hp;
        // 60 damage at the centre, halved for hurting yourself, less the falloff.
        assert!(hp > 62.0 && hp < 80.0, "self damage is about half: {hp}");
        assert!(top > 6.0, "a rocket jump: rose {top} m");
        assert_eq!(w.worms[0].kills, 0, "and it scores nothing");
    }

    #[test]
    fn the_railgun_pierces_worms_but_not_rock() {
        let mut w = arena(flat, &[(16.0, 16.0), (16.0, 22.0), (16.0, 28.0)]);
        shoot(&mut w, RAILGUN, -0.05);
        assert!((w.worms[1].hp - 30.0).abs() < 1.0e-3 && (w.worms[2].hp - 30.0).abs() < 1.0e-3, "both in line were hit");

        let mut w = arena(slab, &[(16.0, 16.0), (16.0, 26.0)]);
        let events = shoot(&mut w, RAILGUN, LEVEL);
        assert_eq!(w.worms[1].hp, MAX_HP, "the slab is cover");
        let to = events.iter().find_map(|e| if let Event::Beam { to, .. } = e { Some(to.z) } else { None });
        assert!(to.unwrap() < 20.5, "the beam ends at the rock: {to:?}");
        assert!(w.grid.sample(Vec3::new(16.0, 5.0, 20.5)) < 0.0, "and leaves it whole");
    }

    #[test]
    fn killing_scores_and_the_victim_comes_back_protected() {
        let mut w = duel();
        w.worms[1].hp = 5.0;
        let mut events = shoot(&mut w, BLASTER, LEVEL);
        events.extend(run(&mut w, 30));
        assert!(!w.worms[1].alive);
        assert_eq!((w.worms[0].kills, w.worms[1].deaths), (1, 1));
        assert!(events.iter().any(|e| matches!(e, Event::Died { victim: 1, killer: 0, .. })));

        let events = run(&mut w, (RESPAWN_TIME / DT) as usize + 5);
        assert!(events.iter().any(|e| matches!(e, Event::Respawned { worm: 1 })));
        let back = &w.worms[1];
        assert!(back.alive && back.hp == MAX_HP && back.invuln > 1.0);
        assert!(w.spawns.iter().any(|s| s.distance(back.pos) < 1.0), "at a spawn point: {:?}", back.pos);
    }

    #[test]
    fn spawn_protection_stops_damage_and_ends_when_you_shoot() {
        let mut w = duel();
        w.worms[1].invuln = 2.0;
        shoot(&mut w, BLASTER, LEVEL);
        run(&mut w, 30);
        assert_eq!(w.worms[1].hp, MAX_HP);

        w.worms[0].invuln = 2.0;
        shoot(&mut w, BLASTER, LEVEL);
        assert_eq!(w.worms[0].invuln, 0.0);
    }

    #[test]
    fn rock_between_a_blast_and_a_worm_is_cover() {
        let hurt = |f: fn(Vec3) -> f32| {
            // The worm stands just behind where the slab is.
            let mut w = arena(f, &[(16.0, 14.0), (16.0, 22.8)]);
            w.explode(Vec3::new(16.0, 5.0, 19.5), &WEAPONS[BOMB as usize], 0);
            MAX_HP - w.worms[1].hp
        };
        assert!(hurt(flat) > 20.0, "in the open it hurts: {}", hurt(flat));
        assert_eq!(hurt(slab), 0.0, "behind a slab of rock it does not");
    }

    #[test]
    fn a_bouncer_ricochets_off_rock_instead_of_ending() {
        let mut w = arena(walled, &[(16.0, 16.0)]);
        shoot(&mut w, BOUNCER, 0.0);
        run(&mut w, 30);
        assert_eq!(w.projectiles.len(), 1, "still flying");
        assert!(w.projectiles[0].vel.z < -20.0, "and now coming back: {:?}", w.projectiles[0].vel);
    }

    #[test]
    fn a_bomb_drops_to_the_ground_rests_and_goes_off_at_its_fuse() {
        let mut w = arena(flat, &[(16.0, 16.0), (16.0, 30.0)]);
        shoot(&mut w, BOMB, 0.0);
        run(&mut w, 30); // falls, hops once or twice, then creeps to rest
        let bomb = &w.projectiles[0];
        assert_eq!(bomb.vel, Vec3::ZERO, "at rest");
        assert!(bomb.pos.y > 4.0 && bomb.pos.y < 4.4, "on the ground: {:?}", bomb.pos);
        let hole = Vec3::new(16.0, 3.0, 16.0);
        assert!(w.grid.sample(hole) < 0.0);

        let events = run(&mut w, 50);
        assert!(w.projectiles.is_empty());
        assert_eq!(explosions(&events), 1);
        assert!(w.grid.sample(hole) > 0.0, "the crater is below where the bomb lay");
        assert!(w.worms[0].hp < 70.0, "standing on your own bomb hurts: {}", w.worms[0].hp);
    }

    #[test]
    fn a_bomb_goes_off_when_an_enemy_steps_on_it() {
        let mut w = arena(flat, &[(16.0, 16.0), (16.0, 30.0)]);
        shoot(&mut w, BOMB, 0.0);
        run(&mut w, 30);
        let at = w.projectiles[0].pos;
        w.worms[1].pos = Vec3::new(at.x + 0.3, 4.05, at.z);
        let events = run(&mut w, 2);
        assert_eq!(explosions(&events), 1, "long before the fuse");
        assert!(w.worms[1].hp < MAX_HP);
    }

    #[test]
    fn a_cluster_shell_bursts_into_seven_bomblets_that_each_explode() {
        let mut w = arena(flat, &[(16.0, 16.0), (16.0, 30.0)]);
        shoot(&mut w, CLUSTER, 0.3);
        let mut seen = 0;
        for _ in 0..120 {
            w.step(&[]);
            seen = w.projectiles.iter().filter(|p| p.weapon == BOMBLET).count();
            if seen > 0 {
                break;
            }
        }
        assert_eq!(seen, 7, "the burst");
        let events = run(&mut w, 60);
        assert_eq!(explosions(&events), 7);
        assert!(w.projectiles.is_empty());
    }

    #[test]
    fn a_cluster_shell_flies_through_worms() {
        let mut w = duel();
        w.worms[1].pos.z = 20.0;
        shoot(&mut w, CLUSTER, LEVEL);
        run(&mut w, 8); // 28 m/s: well past 4 m after 8 ticks
        assert!(w.projectiles.iter().any(|p| p.weapon == CLUSTER), "no burst on the worm yet");
        assert_eq!(w.worms[1].hp, MAX_HP);
    }

    #[test]
    fn a_swarm_missile_curls_towards_the_enemy() {
        let mut w = duel();
        // One missile heading 22 degrees away from the enemy, 10 m off.
        w.projectiles.push(Projectile {
            pos: Vec3::new(16.0, 4.95, 16.0),
            vel: Vec3::new(8.0, 0.0, 20.0),
            weapon: SWARM,
            owner: 0,
            life: 2.0,
        });
        let angle_to_target = |w: &World| {
            let p = &w.projectiles[0];
            let want = (w.worms[1].pos + Vec3::Y * 0.9 - p.pos).normalize();
            p.vel.normalize().dot(want).clamp(-1.0, 1.0).acos()
        };
        let before = angle_to_target(&w);
        run(&mut w, 8);
        let after = angle_to_target(&w);
        assert!(after < before * 0.5, "{before} -> {after}");
        assert!((w.projectiles[0].vel.length() - 21.5).abs() < 1.0, "turning never changes the speed");
    }

    #[test]
    fn a_swarm_volley_finds_the_target() {
        let mut w = duel();
        shoot(&mut w, SWARM, LEVEL);
        run(&mut w, 120);
        assert!(w.worms[1].hp < MAX_HP - 10.0, "some of the six arrived: {}", w.worms[1].hp);
    }

    #[test]
    fn a_teleport_orb_moves_the_owner_to_where_it_landed() {
        let mut w = arena(walled, &[(16.0, 16.0)]);
        let mut events = shoot(&mut w, TELEPORT, 0.0);
        events.extend(run(&mut w, 40));
        assert!(events.iter().any(|e| matches!(e, Event::Teleported { worm: 0, .. })));
        let worm = &w.worms[0];
        assert!(worm.pos.z > 21.0, "now by the wall: {:?}", worm.pos);
        assert!(crate::world::body_fits(&w.grid, worm.pos), "and not inside it");
    }

    #[test]
    fn a_teleport_orb_that_hits_a_worm_swaps_places() {
        let mut w = duel();
        let (a, b) = (w.worms[0].pos, w.worms[1].pos);
        shoot(&mut w, TELEPORT, LEVEL);
        for _ in 0..60 {
            w.step(&[]);
            if w.drain_events().iter().any(|e| matches!(e, Event::Teleported { .. })) {
                break;
            }
        }
        assert!(w.worms[0].pos.distance(b) < 1.0 && w.worms[1].pos.distance(a) < 1.0);
    }

    #[test]
    fn a_nova_clears_a_cavern() {
        let mut w = arena(walled, &[(16.0, 16.0)]);
        let deep = Vec3::new(16.0, 5.5, 30.0);
        assert!(w.grid.sample(deep) < 0.0);
        shoot(&mut w, NOVA, 0.0);
        run(&mut w, 60);
        assert!(w.grid.sample(deep) > 0.0, "7 m into the wall is open air");
    }

    #[test]
    fn the_block_weapon_adds_rock_where_you_aim_and_costs_a_charge() {
        let mut w = arena(flat, &[(16.0, 16.0)]);
        let dir = Vec3::new(0.0, (-0.5f32).sin(), (-0.5f32).cos());
        let eye = w.worms[0].eye();
        let t = w.grid.raycast(eye, dir, 14.0).unwrap();
        let lump = eye + dir * (t - 0.7);
        assert!(w.grid.sample(lump) > 0.0, "air before");
        let events = shoot(&mut w, BUILD, -0.5);
        assert!(w.grid.sample(lump) < -0.3, "rock after: {}", w.grid.sample(lump));
        assert_eq!(w.worms[0].weapons[BUILD as usize].ammo, 5);
        assert!(events.iter().any(|e| matches!(e, Event::Edit { op: Op::Smooth(_), .. })));
    }

    #[test]
    fn a_lump_is_never_built_into_a_worm() {
        let mut w = arena(flat, &[(16.0, 16.0)]);
        let before = w.grid.checksum();
        let events = shoot(&mut w, BUILD, -1.55); // at its own feet
        assert_eq!(w.grid.checksum(), before);
        assert_eq!(w.worms[0].weapons[BUILD as usize].ammo, 6, "and it was free");
        assert!(events.is_empty());
    }

    #[test]
    fn a_lump_is_never_built_on_a_spawn_point() {
        let mut w = arena(flat, &[(16.0, 16.0), (16.0, 22.0)]);
        w.worms[1].pos = Vec3::new(2.0, 4.05, 2.0); // the second worm walks off
        let before = w.grid.checksum();
        // Aimed at the ground where worm 1 spawned, 6 m ahead.
        shoot(&mut w, BUILD, -0.26);
        assert_eq!(w.grid.checksum(), before, "the spawn column stays free");
    }

    #[test]
    fn the_tractor_builds_a_lump_every_few_ticks_and_stops_short_of_your_face() {
        let mut w = arena(flat, &[(16.0, 16.0)]);
        let before = w.grid.checksum();
        for _ in 0..40 {
            shoot(&mut w, TRACTOR, LEVEL); // aimed far out, so the first lump floats 12 m away
        }
        // Held for 40 ticks it fires at ticks 0, 4, 8 ... 36: ten lumps.
        assert_eq!(w.worms[0].weapons[TRACTOR as usize].ammo, 50);
        assert_ne!(w.grid.checksum(), before);

        // Each lump lands on the last one, so the pile grows towards the worm,
        // and stops at the safe distance instead of burying it.
        for _ in 0..400 {
            shoot(&mut w, TRACTOR, LEVEL);
        }
        let (a, b) = w.worms[0].axis();
        let gap = w.grid.sample((a + b) * 0.5);
        assert!(gap > WORM_RADIUS, "never touching the worm: {gap}");
        assert!(w.worms[0].weapons[TRACTOR as usize].ammo > 0 || w.worms[0].weapons[TRACTOR as usize].reload > 0.0);
    }

    #[test]
    fn an_empty_magazine_reloads_and_a_cooldown_spaces_the_shots() {
        let mut w = arena(flat, &[(16.0, 16.0)]);
        shoot(&mut w, ROCKET, 0.5);
        shoot(&mut w, ROCKET, 0.5); // the next tick: still cooling down
        assert_eq!(w.projectiles.len(), 1);

        w.worms[0].weapons[ROCKET as usize].cooldown = 0.0;
        w.worms[0].weapons[ROCKET as usize].ammo = 1;
        shoot(&mut w, ROCKET, 0.5);
        assert_eq!(w.worms[0].weapons[ROCKET as usize].ammo, 0);
        assert!(w.worms[0].weapons[ROCKET as usize].reload > 0.0);
        w.worms[0].weapons[ROCKET as usize].cooldown = 0.0;
        let shots = w.projectiles.len();
        shoot(&mut w, ROCKET, 0.5);
        assert_eq!(w.projectiles.len(), shots, "no ammo, no shot");
        run(&mut w, 150);
        assert_eq!(w.worms[0].weapons[ROCKET as usize].ammo, 4);
    }

    #[test]
    fn an_unknown_weapon_id_changes_nothing() {
        let mut w = arena(flat, &[(16.0, 16.0)]);
        w.step(&[Input { weapon: ROCKET, ..Input::default() }]);
        assert_eq!(w.worms[0].weapons[w.worms[0].current].id, ROCKET);
        w.step(&[Input { weapon: 200, ..Input::default() }]);
        assert_eq!(w.worms[0].weapons[w.worms[0].current].id, ROCKET);
    }

    #[test]
    fn the_same_fight_plays_out_the_same_way_twice() {
        let fight = || {
            let mut w = arena(walled, &[(16.0, 16.0), (12.0, 20.0)]);
            for t in 0..400 {
                let weapon = [CLUSTER, SWARM, ROCKET, BOUNCER, BLASTER][(t / 80) % 5];
                let yaw = (t as f32 * 0.02).sin();
                w.step(&[Input { fire: true, weapon, yaw, pitch: LEVEL, ..Input::default() }]);
                w.drain_events();
            }
            let worms: Vec<_> = w.worms.iter().map(|w| (w.pos, w.hp, w.kills)).collect();
            (w.grid.checksum(), format!("{worms:?}"), w.projectiles.len())
        };
        assert_eq!(fight(), fight());
    }
}
