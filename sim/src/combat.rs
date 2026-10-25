// Everything that hurts a worm or reshapes the terrain on purpose: firing,
// projectiles in flight, explosions, damage, dying and coming back. The rows of
// numbers are in `weapons.rs`; this file is the few rules that read them.
//
// These are methods of `World` (an `impl` block may live in any module of the
// crate), so `world.rs` stays about movement.

use crate::input::Input;
use crate::rng::Rng;
use crate::weapons::{Kind, Special, WeaponDef, WeaponState, ARSENAL, WEAPONS};
use crate::world::{body_axis, body_fits, Action, Event, World, DT, WORM_HEIGHT, WORM_RADIUS};
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
/// The Block wall: a thick upright rectangle thrown up just ahead of the worm.
/// The centre is `WALL_AHEAD` metres in front, unless the first rock is closer,
/// in which case it is nudged back so the wall still stands proud of the rock.
const WALL_AHEAD: f32 = 2.00;
/// Below this the wall would grow through the worm (or into the gun), so the
/// shot is free. The near face is `WALL_MIN_AHEAD - WALL_HALF_T` metres from
/// the worm, which keeps it just beyond the drawn gun's tip.
const WALL_MIN_AHEAD: f32 = 1.60;
/// 8.75 m wide and 6 m tall: big enough to hide a whole worm and the ground it
/// stands on, which is what makes it read as a shield rather than a lump.
const WALL_HALF_W: f32 = 4.375;
const WALL_HALF_H: f32 = 3.00;
const WALL_HALF_T: f32 = 0.30;
/// Centre height above the feet. The bottom sinks below the ground, so a small
/// dip or slope ahead cannot leave a gap for a bolt to fly through.
const WALL_LIFT: f32 = 2.45;
/// Keep the wall this clear of a worm or a spawn point.
const WALL_MARGIN: f32 = 0.12;
/// Round the join with the old terrain a little, but keep the box's faces flat
/// and its corners crisp: this is a wall, not a lump.
const WALL_SMOOTH: f32 = 0.16;
/// A bouncing projectile slower than this comes to rest (a dropped bomb).
const RESTING_SPEED: f32 = 1.0;
/// How much slack the whip's hit test gives a worm's body around the lash (or,
/// for a sweep, around the fan the swing passes through). The drawn chain is
/// thin, so this is deliberately more generous than it looks.
const WHIP_HIT: f32 = 0.55;
/// How far to each side a horizontal lash sweeps, in radians. Together the two
/// halves cover most of the ground in front of the worm.
pub const WHIP_SWEEP: f32 = 0.70;
/// The overhead smash starts this far above the horizon (radians) and finishes
/// this far below it. The arc passes over the worm's head and comes down in
/// front, which is what makes the double-jump follow-up read as a slam.
const WHIP_SMASH_UP: f32 = 1.35;
const WHIP_SMASH_DOWN: f32 = -1.20;
/// How many points the plasma whip's lash is sampled at. Fixed so the event
/// that tells the viewer what to draw and the path the sim scars are literally
/// the same array, not two implementations that can drift apart.
pub const WHIP_POINTS: usize = 18;

/// The two ways the plasma whip can be swung. The sim picks one from what the
/// worm is doing; the viewer rebuilds the same curve from the event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WhipStyle {
    /// A flat lash across the front, from the worm's left to its right.
    Sweep,
    /// The double-jump follow-up: an overhead arc that smashes down.
    Smash,
}

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

/// Where a shot will go, for the aim aid on screen.
///
/// Three shapes share this, because they answer the same question — "where is
/// this shot going to land, and what will it do there?":
///
/// * a lobbed shell fills `points` with its arc and `hit` with its landing mark;
/// * a straight blast shell leaves `points` empty and sets `blast`, so only the
///   explosion's footprint is drawn at the mark;
/// * a ricochet marks `hit` and adds `bounce`, a short stub showing the way the
///   shell leaves the surface. The real shell keeps bouncing; only the first
///   one is shown, because after that every pebble sends it somewhere else.
///
/// `normal` is the rock's normal at the mark, so the ring can lie on a slope.
#[derive(Clone, Debug, Default)]
pub struct AimPath {
    pub points: Vec<Vec3>,
    pub hit: Option<Vec3>,
    pub normal: Vec3,
    /// Radius of the blast the shot makes at the mark, in metres. Zero means a
    /// plain landing mark (the Cluster's shell is its own marker).
    pub blast: f32,
    /// Where a ricochet leaves the surface after the first hit: `(from, to)`.
    pub bounce: Option<(Vec3, Vec3)>,
}

/// The lash of a plasma whip.
///
/// A sweep is the pose the lash ends in: it reaches along the aim to its full
/// length, bowed to the worm's left. The viewer swings that pose in from the
/// left, which is what makes the blow read as a left-to-right whip crack while
/// the tip still lands where the player is aiming. A smash instead rises over
/// the worm's head and comes down in front, for the follow-up to a double jump.
///
/// The sim samples this once to carve the scar and to resolve a smash, and the
/// viewer calls it with the same arguments to draw the identical curve.
/// `phase` adds a small travelling ripple so no two lashes look mechanically
/// identical without changing the pose.
pub fn whip_path(
    from: Vec3,
    dir: Vec3,
    side: Vec3,
    phase: f32,
    length: f32,
    style: WhipStyle,
) -> [Vec3; WHIP_POINTS] {
    let dir = if dir.length_squared() > 1.0e-8 { dir.normalize() } else { Vec3::Z };
    // Keep `side` perpendicular even if the caller passes an imprecise one.
    let side = side - dir * side.dot(dir);
    let side = if side.length_squared() > 1.0e-6 {
        side.normalize()
    } else {
        dir.any_orthonormal_vector()
    };
    // The horizontal heading the swing is built around. A worm aiming straight
    // up or down has no heading in `dir`, so recover one from `side` (the
    // body's right); if that is degenerate too, look along +Z.
    let flat = Vec3::new(dir.x, 0.0, dir.z);
    let fwd = if flat.length_squared() > 1.0e-6 {
        flat.normalize()
    } else {
        let f = Vec3::Y.cross(side);
        if f.length_squared() > 1.0e-6 { f.normalize() } else { Vec3::Z }
    };
    let mut points = [Vec3::ZERO; WHIP_POINTS];
    for (k, p) in points.iter_mut().enumerate() {
        let t = k as f32 / (WHIP_POINTS - 1) as f32;
        match style {
            WhipStyle::Sweep => {
                // A forward bow with a sine ripple along it: the lash starts at
                // the hand, curves out to the worm's left, and returns to the
                // aim line at the tip, so it keeps its full reach where the
                // crosshair points and still leaves a whip-shaped scar.
                let bow = -0.11 * length * (t * std::f32::consts::PI).sin();
                let wave = (phase + t * std::f32::consts::TAU * 1.5).sin()
                    * 0.05
                    * length
                    * (t * std::f32::consts::PI).sin();
                *p = from + dir * (length * t) + side * (bow + wave);
            }
            WhipStyle::Smash => {
                // Over the shoulder and down: the elevation falls from near
                // straight up to well below the horizon as the lash extends.
                let elev = WHIP_SMASH_UP + (WHIP_SMASH_DOWN - WHIP_SMASH_UP) * t;
                let radial = length * t;
                let (se, ce) = elev.sin_cos();
                let ripple = (phase + t * std::f32::consts::TAU * 1.4).sin() * 0.030 * length * t * t;
                *p = from + fwd * (ce * radial) + Vec3::Y * (se * radial) + side * ripple;
            }
        }
    }
    points
}

/// Is `p` inside the fan a horizontal sweep passes through?
///
/// The drawn lash is one thin pose, but the swing visits every bearing between
/// its start and its end. A target in that fan is hit, with the lash's own
/// thickness (and the target's body) as slack. The fan is centred on the aim's
/// heading and elevation, so aiming down still sweeps the ground below.
fn in_whip_sweep(p: Vec3, origin: Vec3, fwd: Vec3, pitch: f32, reach: f32) -> bool {
    let v = p - origin;
    let d = v.length();
    let slack = WORM_RADIUS + WHIP_HIT;
    if d > reach + slack {
        return false;
    }
    if d <= 1.0e-4 {
        return true;
    }
    // The body has height, so the vertical tolerance is the half-height plus
    // the lash's thickness, expressed as an angle at this distance.
    let elev = (v.y / d).clamp(-1.0, 1.0).asin();
    let cone = ((WORM_HEIGHT * 0.55 + slack) / d).atan();
    if (elev - pitch).abs() > cone {
        return false;
    }
    // And the swing spans the left-to-right arc around the heading.
    let flat = Vec3::new(v.x, 0.0, v.z);
    if flat.length_squared() > 1.0e-6 {
        let bearing = flat.normalize().dot(fwd).clamp(-1.0, 1.0).acos();
        if bearing > WHIP_SWEEP + (slack / d).atan() {
            return false;
        }
    }
    true
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
            // A charge belongs to the weapon in hand: putting it away cancels
            // it, so a swap is also a way to take a shot back.
            if slot != worm.current {
                worm.weapons[worm.current].charge = 0.0;
            }
            worm.current = slot;
        }
        // The wind-up of the weapon in hand, if it has one. It starts on the
        // trigger and then runs on its own: a charge is a commitment, so letting
        // go of the button does not take the shot back. (`--fire` relies on
        // this: one tick of trigger is enough to start it.)
        let current = worm.current;
        let def = &WEAPONS[worm.weapons[current].id as usize];
        let w = &mut worm.weapons[current];
        let ready = w.cooldown <= 0.0 && w.reload <= 0.0 && w.ammo > 0;
        let mut fire = false;
        if ready && (input.fire || w.charge > 0.0) {
            w.charge += DT;
            if w.charge >= def.windup {
                w.charge = 0.0;
                fire = true;
            }
        } else {
            w.charge = 0.0;
        }
        if fire {
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

    /// The aim aid a weapon should draw, and where it lands.
    ///
    /// Not every weapon gets one. The aid exists to answer a question the
    /// crosshair cannot: "where will this go, and what will it do there?" A
    /// blaster bolt flies along the reticle, so a line on top of it is noise. A
    /// lobbed shell, a ricochet and a big blast all make that question worth
    /// answering. `World` owns the rule, so the viewer never has to guess.
    pub fn aim_path(&self, i: usize) -> Option<AimPath> {
        let worm = self.worms.get(i)?;
        if !worm.alive {
            return None;
        }
        let id = worm.weapons[worm.current].id;
        let def = &WEAPONS[id as usize];
        let (muzzle, shot) = self.shot_plan(i);

        // A thrown shell: the arc is the weapon, so show it and the landing
        // mark. The Cluster is the only one slow enough and heavy enough for
        // the arc to be readable (the Bomb is dropped, the Bouncer ricochets).
        let lobbed = def.kind == Kind::Projectile
            && def.gravity > 0.0
            && def.speed > 0.0
            && def.speed <= 30.0;
        if lobbed {
            return self.lob_path(def, muzzle, shot);
        }

        // A ricocheting shell: show the first surface it will hit and the way
        // it leaves. Simulating the whole chain would make every bump a
        // different answer, and the skill of reading a bounce is the point.
        let ricochet = def.kind == Kind::Projectile && def.bounce > 0.0 && def.speed > 0.0;
        if ricochet {
            return self.bounce_path(def, muzzle, shot);
        }

        // A straight shell with a blast (Rocket, Nova): no arc, because it
        // really does fly where the reticle points, but its explosion size at
        // the mark. Spending the Nova's one round is a decision worth making
        // with the 8 m footprint in view. Swarm is excluded: its pellets home
        // and fan out, so a single mark would be a lie.
        let flat_blast = def.kind == Kind::Projectile
            && def.blast_radius > 0.0
            && def.gravity == 0.0
            && def.speed > 0.0
            && def.pellets == 1
            && matches!(def.special, Special::None);
        if flat_blast {
            return self.blast_mark(def, muzzle, shot);
        }

        None
    }

    /// The arc a lobbed shot will follow, and where it will land.
    ///
    /// The flight is stepped exactly as `step_projectiles` does it (minus the
    /// random spread), so the landing mark lands on the rock the shot will hit.
    fn lob_path(&self, def: &WeaponDef, muzzle: Vec3, shot: Vec3) -> Option<AimPath> {
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
        Some(AimPath { points, hit, normal, ..Default::default() })
    }

    /// The Bouncer's first ricochet: where it hits and the short leg it leaves
    /// along. The flight and the mirror come from the same maths as
    /// `step_projectiles`, so the stub points where the shell will really go.
    fn bounce_path(&self, def: &WeaponDef, muzzle: Vec3, shot: Vec3) -> Option<AimPath> {
        let mut pos = muzzle;
        let mut vel = shot * def.speed;
        let ticks = (def.lifetime / DT).ceil() as usize;
        for _ in 0..ticks {
            vel.y -= def.gravity * DT;
            let n = ((vel.length() * DT) / PROJECTILE_STEP).ceil().max(1.0) as i32;
            let step = vel * DT / n as f32;
            for _ in 0..n {
                pos += step;
                if self.grid.sample(pos) < PROJECTILE_RADIUS {
                    let n = self.grid.gradient(pos);
                    let normal = if n.length_squared() > 1.0e-6 {
                        n.normalize()
                    } else {
                        -vel.normalize_or_zero()
                    };
                    // `step_projectiles` bounces from the last position outside
                    // the rock, not from the one that touched it, so the mark
                    // (and the ring on it) sits on the outside of the surface.
                    pos -= step;
                    let into = vel.dot(normal);
                    if into < 0.0 {
                        vel -= normal * (2.0 * into);
                    }
                    vel *= def.bounce;
                    if vel.length() < 0.5 {
                        return None; // would come to rest on the spot: no leg to draw
                    }
                    let to = pos + vel.normalize() * 2.0;
                    return Some(AimPath {
                        hit: Some(pos),
                        normal,
                        bounce: Some((pos, to)),
                        ..Default::default()
                    });
                }
            }
        }
        None
    }

    /// The blast a straight shell will make at the reticle. `gravity` is zero
    /// for these weapons, so the flight is the straight line and the reach is
    /// simply speed times fuse.
    fn blast_mark(&self, def: &WeaponDef, muzzle: Vec3, shot: Vec3) -> Option<AimPath> {
        let reach = def.speed * def.lifetime;
        let hit = muzzle + shot * self.grid.raycast(muzzle, shot, reach)?;
        let n = self.grid.gradient(hit);
        let normal = if n.length_squared() > 1.0e-6 { n.normalize() } else { -shot };
        Some(AimPath { hit: Some(hit), normal, blast: def.blast_radius, ..Default::default() })
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
        // Find the terrain spot before spending a charge: aiming at a wall too
        // close, or at a worm, should not cost you a lump or a wall.
        let placed: Option<(Vec3, Option<Shape>)> = match def.kind {
            Kind::Build => match self.build_spot(i, def) {
                Some(spot) => Some((spot, None)),
                None => return,
            },
            Kind::Wall => match self.wall_place(i) {
                Some((shape, at)) => Some((at, Some(shape))),
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
        // A lump or a wall "fires" where it appears, so the flash marks the new
        // rock.
        let (muzzle, wall) = match placed {
            Some((at, wall)) => (at, wall),
            None if def.kind == Kind::Projectile && def.speed == 0.0 => (feet + Vec3::Y * 0.5, None),
            None => (barrel, None),
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
                let shape = Shape::Sphere { center: muzzle, radius: def.terrain };
                let op = Op::Smooth(BUILD_SMOOTH);
                self.grid.apply(shape, op);
                self.events.push(Event::Edit { shape, op });
            }
            // A wall is one box-shaped edit. Same `Edit` event as a lump, so
            // the existing terrain pipeline and networking carry it for free.
            Kind::Wall => {
                let shape = wall.expect("a Wall shot has a shape");
                let op = Op::Smooth(WALL_SMOOTH);
                self.grid.apply(shape, op);
                self.events.push(Event::Edit { shape, op });
            }
            Kind::Whip => self.whip_strike(i, def, muzzle, shot),
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

    /// Where the Block weapon throws its wall, and which way it faces.
    ///
    /// The wall is a vertical rectangle whose face points along the horizontal
    /// aim, so it is always between the worm and what it is looking at. It is
    /// placed a fixed distance ahead (or a little nearer, to stand clear of a
    /// rock face) and sunk slightly into the ground, so there is no gap under
    /// it. None if there is no room between the worm and the rock, or if the
    /// box would grow through a worm or a spawn point.
    fn wall_place(&self, i: usize) -> Option<(Shape, Vec3)> {
        let worm = &self.worms[i];
        // Aim flat at the wall: looking up or down should not tip the shield
        // over. Aiming straight up or down has no horizontal direction, so fall
        // back to where the body faces.
        let aim = worm.aim_dir();
        let flat = Vec3::new(aim.x, 0.0, aim.z);
        let dir = if flat.length_squared() > 1.0e-6 {
            flat.normalize()
        } else {
            let (s, c) = worm.face_yaw.sin_cos();
            Vec3::new(s, 0.0, c)
        };
        // Leave a little air in front of the wall instead of burying it in the
        // rock that is already there.
        let clear = self
            .grid
            .raycast(worm.eye(), dir, WALL_AHEAD + WALL_HALF_T + 0.5)
            .unwrap_or(f32::MAX);
        let ahead = WALL_AHEAD.min(clear - WALL_HALF_T - 0.1);
        if ahead < WALL_MIN_AHEAD {
            return None;
        }
        let centre = worm.pos + dir * ahead + Vec3::Y * WALL_LIFT;
        let shape = Shape::Box {
            centre,
            half: Vec3::new(WALL_HALF_W, WALL_HALF_H, WALL_HALF_T),
            yaw: dir.x.atan2(dir.z),
        };
        // Never materialise rock through a living worm (including the owner) or
        // a spawn point: a wall that clips a body would shove it out, and one
        // over a spawn could wall a respawn in.
        let free = |p: Vec3| shape.distance(p) > WALL_MARGIN;
        let body_free = |feet: Vec3| {
            let (a, b) = body_axis(feet);
            free(a) && free((a + b) * 0.5) && free(b)
        };
        let worms_free = self.worms.iter().filter(|w| w.alive).all(|w| {
            let (a, b) = w.axis();
            free(a) && free((a + b) * 0.5) && free(b)
        });
        let spawns_free = self.spawns.iter().all(|&s| body_free(s));
        if !worms_free || !spawns_free {
            return None;
        }
        Some((shape, centre))
    }

    /// The plasma whip. On the ground it lashes from left to right across the
    /// front; fired during the double jump it becomes an overhead smash that
    /// comes down in front. It damages every worm the lash passes close to and
    /// carves its own path into any rock it crosses.
    fn whip_strike(&mut self, owner: usize, def: &WeaponDef, origin: Vec3, aim: Vec3) {
        let worm = &self.worms[owner];
        let style = if worm.action == Action::DoubleJump && !worm.on_ground {
            WhipStyle::Smash
        } else {
            WhipStyle::Sweep
        };
        let dir = if aim.length_squared() > 1.0e-8 { aim.normalize() } else { Vec3::Z };
        // The swing is built around the worm's horizontal aim; when it looks
        // straight up or down, fall back to the way the body faces.
        let flat = Vec3::new(dir.x, 0.0, dir.z);
        let fwd = if flat.length_squared() > 1.0e-6 {
            flat.normalize()
        } else {
            let (s, c) = worm.face_yaw.sin_cos();
            Vec3::new(s, 0.0, c)
        };
        let side = fwd.cross(Vec3::Y).normalize_or_zero();
        let pitch = dir.y.clamp(-1.0, 1.0).asin();
        let phase = self.rng.range(0.0, std::f32::consts::TAU);
        let path = whip_path(origin, dir, side, phase, def.range, style);
        self.events.push(Event::Whip {
            from: origin,
            dir,
            side,
            phase,
            length: def.range,
            style,
        });

        // A lash is a melee sweep. A sweep catches every worm in the fan the
        // swing crosses; a smash follows the overhead arc it draws. The radius
        // is generous because the drawn chain is thin and the lash moves.
        for v in 0..self.worms.len() {
            if v == owner || !self.worms[v].alive {
                continue;
            }
            let (a, b) = self.worms[v].axis();
            let body = [a, (a + b) * 0.5, b];
            let touch = match style {
                // The swing sweeps the whole fan, not only the pose it ends
                // in, so a target anywhere in the arc is caught.
                WhipStyle::Sweep => body
                    .iter()
                    .any(|&p| in_whip_sweep(p, origin, fwd, pitch, def.range)),
                // The overhead arc is the blow itself.
                WhipStyle::Smash => body.iter().any(|&p| {
                    path.windows(2)
                        .any(|w| point_segment_distance(p, w[0], w[1]) < WORM_RADIUS + WHIP_HIT)
                }),
            };
            if touch {
                let away = ((a + b) * 0.5 - origin).normalize_or_zero();
                // A smash drives its victim into the ground; a sweep sends it
                // away along the lash.
                let push = match style {
                    WhipStyle::Sweep => away,
                    WhipStyle::Smash => Vec3::new(away.x, -1.0, away.z).normalize_or_zero(),
                };
                self.damage(v, def.damage, owner, push * def.knockback);
            }
        }

        // The scar. `Carve` only changes voxels near the lash, so a strike
        // through open air costs nothing; where it crosses rock it burns a
        // groove along the same curve the viewer draws.
        let op = Op::Carve;
        for w in path.windows(2) {
            let shape = Shape::Capsule { a: w[0], b: w[1], radius: def.terrain };
            self.grid.apply(shape, op);
            self.events.push(Event::Edit { shape, op });
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
    use crate::weapons::{
        BLASTER, BOMB, BOMBLET, BOUNCER, BUILD, CLUSTER, NOVA, RAILGUN, ROCKET, SWARM, TELEPORT, TRACTOR, WHIP,
    };
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

    /// Pull the trigger once and keep the weapon in hand for `ticks`, which is
    /// what a real client does (the chosen weapon is sent every tick). A charge
    /// that has started runs on its own, so the fire bit is only set on the
    /// first tick.
    fn hold(w: &mut World, weapon: u8, pitch: f32, ticks: usize) -> Vec<Event> {
        let mut events = Vec::new();
        for t in 0..ticks {
            w.step(&[Input { fire: t == 0, weapon, pitch, ..Input::default() }]);
            events.extend(w.drain_events());
        }
        events
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
    fn guns_whose_shot_follows_the_reticle_get_no_aim_aid() {
        let mut w = arena(flat, &[(16.0, 16.0)]);
        for id in [BLASTER, RAILGUN, BOMB, SWARM, TELEPORT, BUILD, TRACTOR, WHIP] {
            w.worms[0].current = id as usize;
            assert!(w.aim_path(0).is_none(), "weapon {id} needs no aid");
        }
    }

    #[test]
    fn the_shots_that_leave_the_reticle_get_an_aim_aid() {
        let mut w = arena(flat, &[(16.0, 16.0)]);
        // Straight blast shells need something on the ground in front of them;
        // the lobbed and ricocheting ones arc down onto it by themselves.
        w.worms[0].pitch = -0.3;
        for id in [CLUSTER, BOUNCER, ROCKET, NOVA] {
            w.worms[0].current = id as usize;
            assert!(w.aim_path(0).is_some(), "weapon {id} does need one");
        }
    }

    #[test]
    fn a_rocket_and_a_nova_mark_their_blast_at_the_impact() {
        let mut w = arena(flat, &[(16.0, 16.0)]);
        w.worms[0].pitch = -0.3; // aim at the ground in front
        for (id, radius) in [(ROCKET, 3.0), (NOVA, 8.0)] {
            w.worms[0].current = id as usize;
            let path = w.aim_path(0).expect("a blast is worth marking");
            assert!(path.points.is_empty(), "it flies straight, so no arc");
            assert_eq!(path.blast, radius, "the ring is the blast radius");
            assert_eq!(path.bounce, None);
            let hit = path.hit.expect("the ground is in the way");
            assert!(hit.y > 3.9 && hit.y < 4.2, "the mark is on the ground: {hit:?}");
            assert!(path.normal.y > 0.5, "the ring lies on the ground: {:?}", path.normal);
        }
    }

    #[test]
    fn a_bouncer_marks_the_first_ricochet() {
        let mut w = arena(walled, &[(16.0, 16.0)]);
        w.worms[0].current = BOUNCER as usize;
        let path = w.aim_path(0).expect("a ricochet is worth marking");
        assert!(path.points.is_empty(), "one bounce, not a whole predicted path");
        assert_eq!(path.blast, 0.0);
        let hit = path.hit.expect("the wall is in the way");
        assert!(hit.z > 23.0, "the mark is on the wall: {hit:?}");
        assert!(path.normal.z < -0.5, "the ring faces back at the worm: {:?}", path.normal);
        let (from, to) = path.bounce.expect("and the shell leaves the wall");
        assert!((from - hit).length() < 1.0e-4, "the stub starts at the mark");
        let away = (to - from).normalize();
        assert!(away.dot(path.normal) > 0.2, "the stub comes back off the wall: {away:?} {:?}", path.normal);
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
        run(&mut w, 8); // still in flight: passing through a worm does not burst it
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
        // It has to wind up (1.2 s) before it flies, so give it the time.
        hold(&mut w, NOVA, 0.0, 120);
        assert!(w.grid.sample(deep) > 0.0, "7 m into the wall is open air");
    }

    #[test]
    fn a_nova_charges_before_it_fires_and_a_started_charge_is_a_commitment() {
        let mut w = arena(walled, &[(16.0, 16.0)]);
        // A third of the wind-up: the charge builds and nothing has fired.
        let mut events = hold(&mut w, NOVA, 0.0, 24);
        let t = w.worms[0].charge_fraction();
        assert!(t > 0.2 && t < 1.0, "the charge is building: {t}");
        assert!(!events.iter().any(|e| matches!(e, Event::Shot { .. })), "not yet");
        assert!(w.projectiles.is_empty(), "nothing in the air");

        // Let go: the charge is a commitment and completes on its own.
        events.extend(hold(&mut w, NOVA, 0.0, 60));
        assert!(events.iter().any(|e| matches!(e, Event::Shot { weapon: NOVA, .. })), "the charge completes");
        assert_eq!(w.worms[0].weapons[NOVA as usize].charge, 0.0, "and is spent");
    }

    #[test]
    fn a_blaster_fires_on_the_click_and_a_swap_cancels_a_charge() {
        let mut w = arena(flat, &[(16.0, 16.0)]);
        let events = shoot(&mut w, BLASTER, LEVEL);
        assert!(
            events.iter().any(|e| matches!(e, Event::Shot { weapon: BLASTER, .. })),
            "a weapon with no wind-up fires on the click"
        );

        // Start charging the Nova, then put it away: the shot is off.
        hold(&mut w, NOVA, 0.0, 30);
        assert!(w.worms[0].charge_fraction() > 0.2);
        let mut events = hold(&mut w, BLASTER, LEVEL, 90);
        assert!(
            !events.iter().any(|e| matches!(e, Event::Shot { weapon: NOVA, .. })),
            "a swap takes the charge back"
        );
        assert_eq!(w.worms[0].weapons[NOVA as usize].charge, 0.0);
        assert!(
            events.iter().any(|e| matches!(e, Event::Shot { weapon: BLASTER, .. })),
            "and the new weapon still fires"
        );
    }

    #[test]
    fn the_block_weapon_throws_up_a_thick_wall_in_front() {
        let mut w = arena(flat, &[(16.0, 16.0)]);
        let ammo = w.worms[0].weapons[BUILD as usize].ammo;
        let events = shoot(&mut w, BUILD, 0.0);
        assert_eq!(w.worms[0].weapons[BUILD as usize].ammo, ammo - 1, "one charge, one wall");
        let shape = events
            .iter()
            .find_map(|e| match e {
                Event::Edit { shape: shape @ Shape::Box { .. }, .. } => Some(*shape),
                _ => None,
            })
            .expect("the wall is a box edit");
        let Shape::Box { centre, half, .. } = shape else { unreachable!() };
        assert!((centre.z - (w.worms[0].pos.z + WALL_AHEAD)).abs() < 0.05, "just in front");
        assert!(w.grid.sample(centre) < -0.2, "solid at the centre");
        assert!(w.grid.sample(centre + Vec3::X * (half.x * 0.5)) < -0.2, "solid out to the side");
        assert!(w.grid.sample(centre + Vec3::X * (half.x + 0.6)) > 0.0, "air beside it");
        assert!(w.grid.sample(centre + Vec3::Y * (half.y + 0.4)) > 0.0, "air above it");
        // Thick enough to stop a bolt, and it must not contain its maker.
        assert!((half.z * 2.0) >= 0.5, "a thick wall: {} m", half.z * 2.0);
        let (a, b) = w.worms[0].axis();
        for p in [a, (a + b) * 0.5, b] {
            assert!(shape.distance(p) > 0.0, "the wall is not on the worm: {p:?}");
        }
    }

    #[test]
    fn a_block_wall_stops_a_shot_from_the_front() {
        let mut w = arena(flat, &[(16.0, 16.0), (16.0, 24.0)]);
        shoot(&mut w, BUILD, 0.0);
        let hp = w.worms[0].hp;
        // Worm 1 turns around and fires through where the wall now stands.
        w.step(&[
            Input::default(),
            Input { fire: true, weapon: RAILGUN, yaw: std::f32::consts::PI, pitch: LEVEL, ..Input::default() },
        ]);
        let events = w.drain_events();
        assert_eq!(w.worms[0].hp, hp, "the wall is cover");
        let end = events.iter().find_map(|e| if let Event::Beam { to, .. } = e { Some(*to) } else { None }).unwrap();
        assert!(end.z > 17.0 && end.z < 19.0, "the beam ends on the wall: {end:?}");
    }

    #[test]
    fn a_wall_is_never_thrown_through_a_worm() {
        let mut w = arena(flat, &[(16.0, 16.0), (16.0, 17.6)]);
        let before = w.grid.checksum();
        let events = shoot(&mut w, BUILD, 0.0);
        assert_eq!(w.grid.checksum(), before, "no wall through the other worm");
        assert_eq!(w.worms[0].weapons[BUILD as usize].ammo, 4, "and it was free");
        assert!(events.is_empty());
    }

    #[test]
    fn a_wall_is_never_thrown_over_a_spawn_point() {
        let mut w = arena(flat, &[(16.0, 16.0), (16.0, 17.6)]);
        w.worms[1].pos = Vec3::new(2.0, 4.05, 2.0); // the second worm walks off
        let before = w.grid.checksum();
        shoot(&mut w, BUILD, 0.0);
        assert_eq!(w.grid.checksum(), before, "the spawn column stays free");
    }

    #[test]
    fn a_whip_sweep_bows_left_and_reaches_the_aim_at_full_length() {
        let path = whip_path(Vec3::ZERO, Vec3::Z, Vec3::NEG_X, 0.0, 9.5, WhipStyle::Sweep);
        assert_eq!(path[0], Vec3::ZERO, "it starts at the emitter");
        // The worm faces +Z, so its left is +X.
        assert!(path.iter().any(|p| p.x > 0.5), "the lash bows to the left");
        let last = path[WHIP_POINTS - 1];
        assert!((last - Vec3::Z * 9.5).length() < 1.0e-2, "the tip reaches the aim at full length");
        assert!(path.iter().all(|p| p.is_finite()));
    }

    #[test]
    fn a_whip_smash_goes_overhead_and_comes_down_in_front() {
        let path = whip_path(Vec3::ZERO, Vec3::Z, Vec3::NEG_X, 0.0, 9.5, WhipStyle::Smash);
        assert_eq!(path[0], Vec3::ZERO, "it starts at the emitter");
        assert!(path.iter().any(|p| p.y > 1.0), "it rises over the worm");
        let last = path[WHIP_POINTS - 1];
        assert!((last.length() - 9.5).abs() < 1.0e-2, "and reaches the lash length");
        assert!(last.y < -6.0, "the tip is driven into the ground");
        assert!(last.z > 2.0, "well in front of the worm");
        assert!(path.iter().all(|p| p.is_finite()));
    }

    #[test]
    fn the_plasma_whip_hurts_a_worm_it_lashes() {
        let mut w = arena(flat, &[(16.0, 16.0), (16.0, 20.0)]);
        w.worms[0].current = WHIP as usize;
        let events = shoot(&mut w, WHIP, LEVEL);
        assert!(w.worms[1].hp < MAX_HP, "the lash reached it: {}", w.worms[1].hp);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::Whip { style: WhipStyle::Sweep, .. })),
            "and the viewer is told to draw a left-to-right sweep"
        );
    }

    #[test]
    fn the_whip_sweeps_a_long_arc_in_front() {
        // One worm near the far end of the reach, one well out to the left.
        let mut w = arena(flat, &[(16.0, 16.0), (16.0, 25.0), (19.5, 22.06)]);
        w.worms[0].current = WHIP as usize;
        shoot(&mut w, WHIP, LEVEL);
        assert!(w.worms[1].hp < MAX_HP, "the full 9 m reach is hit: {}", w.worms[1].hp);
        assert!(w.worms[2].hp < MAX_HP, "and the sweep covers the left side: {}", w.worms[2].hp);
    }

    #[test]
    fn the_plasma_whip_leaves_a_scar_in_the_rock() {
        let mut w = arena(flat, &[(16.0, 16.0)]);
        let before = w.grid.checksum();
        let events = shoot_at(&mut w, WHIP, 0.0, -0.7); // down at the ground
        assert_ne!(w.grid.checksum(), before, "the rock is scarred");
        assert!(events.iter().any(|e| matches!(e, Event::Edit { op: Op::Carve, .. })));
    }

    #[test]
    fn a_whip_fired_during_the_double_jump_becomes_an_overhead_smash() {
        let mut w = arena(flat, &[(16.0, 16.0)]);
        w.worms[0].current = WHIP as usize;
        // Put the worm in the air in the double-jump action: the movement code
        // keeps that action until it lands.
        w.worms[0].pos.y += 6.0;
        w.worms[0].on_ground = false;
        w.worms[0].action = Action::DoubleJump;
        let events = shoot(&mut w, WHIP, LEVEL);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::Whip { style: WhipStyle::Smash, .. })),
            "the follow-up smashes downwards"
        );
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
