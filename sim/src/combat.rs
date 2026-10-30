// Everything that hurts a worm or reshapes the terrain on purpose: firing,
// projectiles in flight, explosions, damage, dying and coming back. The rows of
// numbers are in `weapons.rs`; this file is the few rules that read them.
//
// These are methods of `World` (an `impl` block may live in any module of the
// crate), so `world.rs` stays about movement.

use crate::input::Input;
use crate::rng::Rng;
use crate::weapons::{Kind, Special, WeaponDef, WeaponState, ARSENAL, LAVA, WEAPONS, WHIP};
use crate::world::{body_axis, body_fits, Action, Event, World, Zone, ZoneKind, DT, WORM_HEIGHT, WORM_RADIUS};
use glam::{Quat, Vec3};
use sdf::{Op, Shape};
use serde::{Deserialize, Serialize};

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
const FLASH_AHEAD: f32 = 0.2;
/// Build lumps join the terrain with a rounded crease this wide, like clay.
const BUILD_SMOOTH: f32 = 0.4;
/// Burning: a worm in touch with molten lava burns for `BURN_TIME` more seconds,
/// and takes `BURN_DAMAGE` every `BURN_TICK` seconds. Standing in lava kills in
/// about six seconds; running through it costs about a third of your health.
const BURN_TIME: f32 = 1.5;
const BURN_TICK: f32 = 0.25;
const BURN_DAMAGE: f32 = 4.0;
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
/// Aiming steeper than this (the sine of the pitch, about 30 degrees) lays the
/// block flat: a floor when looking down, a roof when looking up. The roof
/// starts this far above the eye and the floor ends this far below the feet.
const WALL_SLAB_SLOPE: f32 = 0.5;
const WALL_ROOF_GAP: f32 = 0.9;
const WALL_FLOOR_GAP: f32 = 0.15;
/// Keep the wall this clear of a worm or a spawn point.
const WALL_MARGIN: f32 = 0.12;
/// Round the join with the old terrain a little, but keep the box's faces flat
/// and its corners crisp: this is a wall, not a lump.
const WALL_SMOOTH: f32 = 0.16;
/// A mine ignores enemies for this long after it is dropped, so it cannot go
/// off in the face of whoever it was dropped next to before they can react.
const MINE_ARM: f32 = 0.8;
/// Height above the mine from which it checks for cover.
const MINE_EYE: f32 = 0.3;
/// A bouncing projectile slower than this comes to rest (a dropped bomb).
const RESTING_SPEED: f32 = 1.0;
/// How much slack the whip's hit test gives a worm's body around the lash. The
/// lash is thin and moves tens of metres per second, so this is deliberately
/// more generous than it looks.
const WHIP_HIT: f32 = 0.55;
/// How far to each side a horizontal swing starts and ends, in radians (75
/// degrees, so the whole arc is 150 degrees wide).
pub const WHIP_SWEEP: f32 = 1.3;
/// How far the swing plane may lean towards the aim pitch (30 degrees). Level by
/// default, so the arc keeps its wide, flat silhouette; the lean lets it catch a
/// worm on the next ledge.
pub const WHIP_LEAN: f32 = 0.52;
/// The length of the carried lash. A swing extends it to `WeaponDef::range`
/// and draws it back in again.
pub const WHIP_REST: f32 = 5.0;
/// Seconds one swing takes. Holding the trigger chains the next one straight
/// away, so this is also the whip's rhythm.
pub const WHIP_SWING_TIME: f32 = 0.30;
/// The part of the swing, as fractions of `WHIP_SWING_TIME`, in which the lash is
/// live: before it the arm is drawing back, after it the lash is recovering.
pub const WHIP_ACTIVE: (f32, f32) = (0.20, 0.85);
/// The overhead smash starts this far above the horizon (radians) and finishes
/// this far below it. The arc passes over the worm's head and comes down in
/// front, which is what makes the double-jump follow-up read as a slam.
pub const WHIP_SMASH_UP: f32 = 1.35;
pub const WHIP_SMASH_DOWN: f32 = -1.20;
/// How deep, in metres, the fastest part of a swing cuts into rock. The cut
/// tapers to nothing at both ends of the swing. (The smash still cuts a thin
/// slit, two voxels wide: a thinner one cannot be stored in a 25 cm grid.)
const WHIP_CUT_DEPTH: f32 = 0.8;
/// Radius of the flat disc the sweep drags through the rock, so the groove is
/// twice this wide.
const WHIP_CUT_RADIUS: f32 = 0.35;
const WHIP_SMASH_DEPTH: f32 = 2.0;
/// The overhead smash is a committed blow in the air, so it hits harder than a
/// swing of the chain.
const WHIP_SMASH_DAMAGE: f32 = 35.0;
/// Half the width of the slit.
const WHIP_KERF: f32 = 0.25;
/// A cut shallower than this is not worth an edit.
const WHIP_MIN_CUT: f32 = 0.3;
/// How many bearings one tick's slice of the swing is sampled at when it cuts.
/// The tip moves a couple of metres per tick; three samples keep the slit
/// following a curved wall instead of cutting corners.
const WHIP_CUT_STEPS: usize = 5;
/// How much of the way each new contact is pulled from the last one, 0..1. The
/// surface under the lash is bumpy, and a slit that followed every bump would be
/// a zigzag; a low-pass over the contacts keeps the bite and drops the jitter.
const WHIP_CUT_FOLLOW: f32 = 0.6;
/// Largest gap between two surface contacts that the slit will bridge. A larger
/// jump means the lash left one rock face and found another, so the cut should
/// stop rather than slice through the air between them.
const WHIP_CUT_BRIDGE: f32 = 3.0;

/// The two ways the plasma whip can be swung. The sim picks one from what the
/// worm is doing; the viewer rebuilds the same motion from the event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WhipStyle {
    /// A flat lash across the front, alternating direction with each swing.
    Sweep,
    /// The double-jump follow-up: an overhead arc that smashes down.
    Smash,
}

/// A swing in progress. Pattern: State. The whip is no longer resolved on the
/// tick the trigger is pulled; the worm holds a `Swing` that advances with the
/// sim, so a worm is hit (and the rock is cut) at the moment the lash passes it.
/// Everything the viewer needs to draw the same swing is in `Event::Whip`; it
/// does not read this.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Swing {
    pub style: WhipStyle,
    /// Which way the arc runs: a held trigger alternates it.
    pub flip: bool,
    /// Seconds since the swing began.
    pub t: f32,
    /// The horizontal heading, the worm's right, and the lean of the swing
    /// plane (radians), all fixed when the swing starts.
    pub fwd: Vec3,
    pub side: Vec3,
    pub lean: f32,
    /// Worms this swing has already hurt, one bit per worm: a lash hits each
    /// target once, however many ticks it spends passing through it.
    #[serde(skip)]
    pub(crate) hit: u32,
    /// Where the lash last met rock, and the way it was travelling, so the next
    /// contact can be joined to it into one slit.
    #[serde(skip)]
    pub(crate) cut: Option<(Vec3, Vec3)>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
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

/// How far through its arc the lash is at swing phase `p` (0..1): slow to wind
/// up, fast through the middle, slow to settle. Pattern: Easing (smoothstep).
pub fn whip_arc(p: f32) -> f32 {
    let p = p.clamp(0.0, 1.0);
    p * p * (3.0 - 2.0 * p)
}

/// The lash's length at phase `p`: it pays out during the wind-up, is fully
/// extended through the live part, and is drawn back in at the end.
pub fn whip_reach(range: f32, p: f32) -> f32 {
    let out = whip_arc(p / 0.25);
    let back = whip_arc((p - WHIP_ACTIVE.1) / (1.0 - WHIP_ACTIVE.1));
    WHIP_REST + (range - WHIP_REST) * out * (1.0 - back)
}

/// Unit direction of the lash at phase `p`. This is a pure function of the
/// swing's fixed frame, so the sim (hits and cuts) and the viewer (the arm and
/// the rope's target) walk exactly the same path.
pub fn whip_dir(style: WhipStyle, fwd: Vec3, side: Vec3, lean: f32, flip: bool, p: f32) -> Vec3 {
    let arc = whip_arc(p);
    match style {
        WhipStyle::Sweep => {
            // From one side to the other, always level but for the lean.
            let from = if flip { -1.0 } else { 1.0 };
            let (sy, cy) = (from * WHIP_SWEEP * (1.0 - 2.0 * arc)).sin_cos();
            let (sl, cl) = lean.sin_cos();
            (fwd * cy + side * sy) * cl + Vec3::Y * sl
        }
        WhipStyle::Smash => {
            // Over the shoulder and down in the facing plane.
            let (se, ce) = (WHIP_SMASH_UP + (WHIP_SMASH_DOWN - WHIP_SMASH_UP) * arc).sin_cos();
            fwd * ce + Vec3::Y * se
        }
    }
}

/// The frame a swing is built in, from the aim: the horizontal heading, the
/// worm's right, and the lean of the swing plane. When the aim is straight up
/// or down there is no heading in it, so fall back to the way the body faces.
pub fn whip_frame(aim: Vec3, face_yaw: f32) -> (Vec3, Vec3, f32) {
    let dir = if aim.length_squared() > 1.0e-8 { aim.normalize() } else { Vec3::Z };
    let flat = Vec3::new(dir.x, 0.0, dir.z);
    let fwd = if flat.length_squared() > 1.0e-6 {
        flat.normalize()
    } else {
        let (s, c) = face_yaw.sin_cos();
        Vec3::new(s, 0.0, c)
    };
    let side = fwd.cross(Vec3::Y).normalize_or_zero();
    let lean = dir.y.clamp(-1.0, 1.0).asin().clamp(-WHIP_LEAN, WHIP_LEAN);
    (fwd, side, lean)
}

/// Did the part of the swing between phases `pa` and `pb` pass through the
/// worm's body point `p`? The tip moves several metres per tick, so testing the
/// lash where it is now would let it skip over a worm: this tests the whole
/// wedge (or, for a smash, the whole slice of the overhead arc) it crossed.
fn in_whip_fan(p: Vec3, origin: Vec3, s: &Swing, pa: f32, pb: f32, range: f32) -> bool {
    let v = p - origin;
    let d = v.length();
    let slack = WORM_RADIUS + WHIP_HIT;
    if d > whip_reach(range, pa).max(whip_reach(range, pb)) + slack {
        return false;
    }
    if d <= 1.0e-4 {
        return true;
    }
    let (f, sd) = (v.dot(s.fwd), v.dot(s.side));
    // The slack at this distance, as an angle.
    let ang = (slack / d.max(0.5)).atan();
    match s.style {
        WhipStyle::Sweep => {
            // The body has height, so the vertical tolerance is its half-height
            // plus the lash's thickness, as an angle at this distance.
            let elev = (v.y / d).clamp(-1.0, 1.0).asin();
            if (elev - s.lean).abs() > ((WORM_HEIGHT * 0.55 + slack) / d).atan() {
                return false;
            }
            let from = if s.flip { -1.0 } else { 1.0 };
            let yaw = |p: f32| from * WHIP_SWEEP * (1.0 - 2.0 * whip_arc(p));
            let (a, b) = (yaw(pa), yaw(pb));
            let bearing = sd.atan2(f);
            bearing >= a.min(b) - ang && bearing <= a.max(b) + ang
        }
        WhipStyle::Smash => {
            if sd.abs() > slack {
                return false;
            }
            let elev = |p: f32| WHIP_SMASH_UP + (WHIP_SMASH_DOWN - WHIP_SMASH_UP) * whip_arc(p);
            let (a, b) = (elev(pa), elev(pb));
            let e = v.y.atan2(f);
            e >= a.min(b) - ang && e <= a.max(b) + ang
        }
    }
}

/// Where along the ray the surface really is. `Grid::raycast` stops within a
/// step of it (an eighth of a metre or more on a grazing ray), which is plenty
/// for a bullet but shows as a zigzag when a slit is cut along the contacts. A
/// few bisection steps between the last safe point and the hit pin it down.
fn refine_hit(grid: &sdf::Grid, origin: Vec3, dir: Vec3, hit: f32) -> f32 {
    let (mut lo, mut hi) = ((hit - sdf::VOX).max(0.0), hit);
    if grid.sample(origin + dir * lo) < 0.0 {
        return hit; // already inside at the start of the bracket: nothing to bisect
    }
    for _ in 0..5 {
        let mid = (lo + hi) * 0.5;
        if grid.sample(origin + dir * mid) < 0.0 {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    (lo + hi) * 0.5
}

/// Is the straight line from the emitter to a body point clear of rock? The
/// lash is a melee fan, but a wall still has to stop it. Close targets are
/// always clear: there is nothing to hide behind at point-blank range.
fn lash_clear(grid: &sdf::Grid, origin: Vec3, p: Vec3) -> bool {
    let v = p - origin;
    let d = v.length();
    if d < 0.35 {
        return true;
    }
    grid.raycast(origin, v / d, d - 0.15).is_none()
}


impl World {
    /// Weapon timers, the weapon choice and the trigger of worm `i` (alive).
    pub(crate) fn step_weapons(&mut self, i: usize, input: &Input) {
        let worm = &mut self.worms[i];
        worm.invuln = (worm.invuln - DT).max(0.0);
        if worm.on_ground {
            worm.smashed = false;
        }
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
        let smash_spent = worm.smashed;
        let w = &mut worm.weapons[current];
        // The whip is melee: it has no magazine, so holding the trigger simply
        // starts the next coil as soon as the last swing has recovered. After a
        // smash it waits for the landing, so one double jump is one stomp.
        let ready = w.cooldown <= 0.0
            && w.reload <= 0.0
            && (def.kind == Kind::Whip || w.ammo > 0)
            && !(def.kind == Kind::Whip && smash_spent);
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
        self.worms[i].swing = None;
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
        let def = &WEAPONS[worm.weapons[worm.current].id as usize];
        // A lobbed weapon is not aimed at the crosshair: it is aimed by the
        // landing mark. Its barrel points along the lofted launch direction,
        // and the convergence below would fight the arc, so skip it.
        if def.loft > 0.0 {
            let shot = def.launch_dir(aim);
            return (worm.muzzle() + shot * MUZZLE, shot);
        }
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
        let def = &WEAPONS[w.id as usize];
        // The whip is melee and infinite; everything else needs a round.
        if w.cooldown > 0.0 || w.reload > 0.0 || (def.kind != Kind::Whip && w.ammo == 0) {
            return;
        }
        let id = w.id;
        // A whip is not a gun: it leaves the hand along the aim instead of
        // converging from a muzzle to the crosshair. The extra 0.2 m puts the
        // first point at the emitter ring, where the viewer draws it.
        let (barrel, shot) = if def.kind == Kind::Whip {
            let aim = worm.aim_dir();
            (worm.muzzle() + aim * 0.2, aim)
        } else {
            self.shot_plan(i)
        };
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
        if def.kind != Kind::Whip && !self.infinite_ammo {
            w.ammo -= 1;
        }
        w.cooldown = def.cooldown;
        if def.kind != Kind::Whip && w.ammo == 0 {
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
        // The whip has its own event and its own crack; a muzzle flash at the
        // handle would make it read as a gun again.
        if def.kind != Kind::Whip {
            self.events.push(Event::Shot { worm: i as u8, weapon: id, pos: muzzle + shot * FLASH_AHEAD });
        }

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
                self.fire_beam(i, id, def, muzzle, dir);
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
            // A bit of lava leaves the muzzle like a bullet but is not a
            // projectile: it is fluid from the first tick (see `lava.rs`).
            Kind::Spray => {
                for _ in 0..def.pellets {
                    let dir = spread_dir(&mut self.rng, shot, def.spread);
                    self.spawn_lava_bit(muzzle, dir * def.speed, def.lifetime, i as u8);
                }
            }
            Kind::Whip => self.whip_start(i, muzzle, shot),
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
        let (centre, half) = if aim.y.abs() > WALL_SLAB_SLOPE {
            // Looking steeply up or down: the same slab lies flat instead, as a
            // floor under the feet or a roof over the head. Its width stays
            // across the view and its depth takes the wall's height, so it
            // reaches out ahead of the worm as well as around it.
            let y = if aim.y > 0.0 {
                worm.eye().y + WALL_ROOF_GAP + WALL_HALF_T
            } else {
                worm.pos.y - WALL_FLOOR_GAP - WALL_HALF_T
            };
            let at = worm.pos + dir * WALL_AHEAD;
            (Vec3::new(at.x, y, at.z), Vec3::new(WALL_HALF_W, WALL_HALF_T, WALL_HALF_H))
        } else {
            // Leave a little air in front of the wall instead of burying it in
            // the rock that is already there.
            let clear = self
                .grid
                .raycast(worm.eye(), dir, WALL_AHEAD + WALL_HALF_T + 0.5)
                .unwrap_or(f32::MAX);
            let ahead = WALL_AHEAD.min(clear - WALL_HALF_T - 0.1);
            if ahead < WALL_MIN_AHEAD {
                return None;
            }
            (
                worm.pos + dir * ahead + Vec3::Y * WALL_LIFT,
                Vec3::new(WALL_HALF_W, WALL_HALF_H, WALL_HALF_T),
            )
        };
        let shape = Shape::Box { centre, half, yaw: dir.x.atan2(dir.z) };
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

    /// Start a plasma-whip swing. On the ground it lashes across the front,
    /// alternating right-to-left and left-to-right with every swing; fired during
    /// the double jump it becomes an overhead smash that comes down in front.
    /// Nothing is hit yet: `step_swing` advances the swing and does that as the
    /// lash passes.
    fn whip_start(&mut self, owner: usize, origin: Vec3, aim: Vec3) {
        let worm = &self.worms[owner];
        let style = if worm.action == Action::DoubleJump && !worm.on_ground {
            WhipStyle::Smash
        } else {
            WhipStyle::Sweep
        };
        // Alternate the direction of a held whip. A smash always comes straight
        // down, so only a horizontal sweep flips.
        let flip = if style == WhipStyle::Sweep {
            let slot = worm.current;
            let swings = self.worms[owner].weapons[slot].swings;
            self.worms[owner].weapons[slot].swings = swings.wrapping_add(1);
            swings % 2 == 1
        } else {
            false
        };
        self.worms[owner].smashed = style == WhipStyle::Smash;
        let (fwd, side, lean) = whip_frame(aim, self.worms[owner].face_yaw);
        self.worms[owner].swing =
            Some(Swing { style, flip, t: 0.0, fwd, side, lean, hit: 0, cut: None });
        self.events.push(Event::Whip { worm: owner as u8, from: origin, fwd, side, lean, style, flip });
    }

    /// Advance worm `i`'s swing by one tick: hurt the worms the lash passed
    /// through since the last tick, and cut the rock it touched.
    pub(crate) fn step_swing(&mut self, i: usize) {
        let Some(mut s) = self.worms[i].swing.take() else { return };
        let worm = &self.worms[i];
        // Putting the whip away stops the swing.
        if WEAPONS[worm.weapons[worm.current].id as usize].kind != Kind::Whip {
            return;
        }
        let origin = worm.muzzle();
        let p0 = s.t / WHIP_SWING_TIME;
        s.t += DT;
        let p1 = (s.t / WHIP_SWING_TIME).min(1.0);
        // Only the middle of the swing is live; the rest is drawing back and
        // recovering.
        let (pa, pb) = (p0.max(WHIP_ACTIVE.0), p1.min(WHIP_ACTIVE.1));
        if pa < pb {
            self.whip_hit(i, &mut s, origin, pa, pb);
            self.whip_cut(&mut s, origin, pa, pb);
        }
        if s.t < WHIP_SWING_TIME {
            self.worms[i].swing = Some(s);
        }
    }

    /// Damage every worm the lash crossed between phases `pa` and `pb`, once per
    /// swing each.
    fn whip_hit(&mut self, owner: usize, s: &mut Swing, origin: Vec3, pa: f32, pb: f32) {
        let def = &WEAPONS[WHIP as usize];
        for v in 0..self.worms.len() {
            let bit = 1u32 << (v as u32 % 32);
            if v == owner || !self.worms[v].alive || s.hit & bit != 0 {
                continue;
            }
            let (a, b) = self.worms[v].axis();
            let body = [a, (a + b) * 0.5, b];
            let touch = body
                .iter()
                .any(|&p| in_whip_fan(p, origin, s, pa, pb, def.range) && lash_clear(&self.grid, origin, p));
            if touch {
                s.hit |= bit;
                let away = ((a + b) * 0.5 - origin).normalize_or_zero();
                // A smash drives its victim into the ground; a sweep sends it
                // away along the lash.
                let push = match s.style {
                    WhipStyle::Sweep => away,
                    WhipStyle::Smash => Vec3::new(away.x, -1.0, away.z).normalize_or_zero(),
                };
                let damage = if s.style == WhipStyle::Smash { WHIP_SMASH_DAMAGE } else { def.damage };
                self.damage(v, damage, WHIP, owner, push * def.knockback);
            }
        }
    }

    /// Cut the rock the lash met between phases `pa` and `pb`. Each contact is
    /// joined to the last by a `Blade`, a thin slab reaching into the rock, so
    /// the swing leaves one flat-walled slit along the wall. How deep it goes
    /// follows the lash's speed through the arc: it bites hardest in the middle
    /// and only scratches at the ends. Nothing here digs a round hole.
    fn whip_cut(&mut self, s: &mut Swing, origin: Vec3, pa: f32, pb: f32) {
        let def = &WEAPONS[WHIP as usize];
        let peak = if s.style == WhipStyle::Smash { WHIP_SMASH_DEPTH } else { WHIP_CUT_DEPTH };
        for k in 1..=WHIP_CUT_STEPS {
            let p = pa + (pb - pa) * k as f32 / WHIP_CUT_STEPS as f32;
            let dir = whip_dir(s.style, s.fwd, s.side, s.lean, s.flip, p);
            // The lash stops at the first rock: a wall gets a slit, not a fan
            // of boreholes.
            let Some(t) = self.grid.raycast(origin, dir, whip_reach(def.range, p)) else {
                s.cut = None;
                continue;
            };
            let raw = origin + dir * refine_hit(&self.grid, origin, dir, t);
            // Smooth the path of contacts (but not the first one, which has
            // nothing to follow). The bridge test below still sees the raw gap.
            let at = match s.cut {
                Some((prev, _)) => prev.lerp(raw, WHIP_CUT_FOLLOW),
                None => raw,
            };
            if let Some((prev, prev_dir)) = s.cut {
                let gap = raw.distance(prev);
                let u = (p - WHIP_ACTIVE.0) / (WHIP_ACTIVE.1 - WHIP_ACTIVE.0);
                let depth = peak * (std::f32::consts::PI * u).sin();
                if gap > 0.02 && gap <= WHIP_CUT_BRIDGE && depth >= WHIP_MIN_CUT {
                    // Start a hair in front of the surface so the slit opens
                    // cleanly instead of leaving a lip.
                    const LEAD: f32 = 0.15;
                    let shape = if s.style == WhipStyle::Smash {
                        let into = (prev_dir + dir).normalize_or_zero();
                        Shape::Blade {
                            a: prev - prev_dir * LEAD,
                            b: at - dir * LEAD,
                            dir: if into == Vec3::ZERO { dir } else { into },
                            depth: depth + LEAD,
                            half_thickness: WHIP_KERF,
                        }
                    } else {
                        // A flat disc pressed into the surface and dragged
                        // from the last contact to this one: the segments of
                        // one swing join into a single groove. The disc's
                        // axis is the surface normal, so it lies flat on a
                        // wall as well as on the ground.
                        let n = self.grid.gradient(at);
                        let axis = if n.length_squared() > 1.0e-6 { n.normalize() } else { -dir };
                        let sink = axis * (depth * 0.5 - LEAD);
                        Shape::Cylinder {
                            a: prev - sink,
                            b: at - sink,
                            axis,
                            radius: WHIP_CUT_RADIUS,
                            half_height: depth * 0.5 + LEAD * 0.5,
                        }
                    };
                    self.grid.apply(shape, Op::Carve);
                    self.events.push(Event::Edit { shape, op: Op::Carve });
                    // A crack where it bites, every other tick so a long cut
                    // does not flood the effects.
                    if (s.t / DT).round() as u32 % 2 == 0 {
                        let n = self.grid.gradient(at);
                        let normal = if n.length_squared() > 1.0e-6 { n.normalize() } else { -dir };
                        self.events.push(Event::Impact { weapon: WHIP, pos: at, normal });
                    }
                }
            }
            s.cut = Some((at, dir));
        }
    }

    /// Hitscan: no flying object, the line is resolved instantly.
    fn fire_beam(&mut self, owner: usize, weapon: u8, def: &WeaponDef, origin: Vec3, dir: Vec3) {
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
                self.damage(v, def.damage, weapon, owner, dir * def.knockback);
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
    fn damage(&mut self, victim: usize, amount: f32, weapon: u8, source: usize, impulse: Vec3) {
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
            // The blood follows the push: away from the shot, or up if the hit
            // had no knockback at all (a graze, a direct-energy weapon).
            let dir = if impulse.length_squared() > 1.0e-6 {
                impulse.normalize()
            } else {
                Vec3::Y
            };
            self.events.push(Event::Hit { victim: victim as u8, weapon, pos, damage: amount, dir });
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
            if self.worms[source].kills >= self.score_limit && self.winner.is_none() {
                self.winner = Some(source as u8);
                self.events.push(Event::MatchWon { worm: source as u8 });
            }
        }
    }

    // ------------------------------------------------------------ projectiles

    pub(crate) fn step_projectiles(&mut self) {
        // Take the list out so we can call `&mut self` methods while looping,
        // then put the survivors back.
        let old = std::mem::take(&mut self.projectiles);
        let blasts = std::mem::take(&mut self.blasts);
        let mut keep = Vec::with_capacity(old.len());
        'next: for mut pr in old {
            let def = &WEAPONS[pr.weapon as usize];
            pr.life -= DT;
            if let Special::Mine(radius) = def.special {
                if self.mine_triggered(&pr, def, radius, &blasts) {
                    self.impact(&pr, def, None);
                    continue 'next;
                }
            }
            if let Special::Homing(turn) = def.special {
                self.steer(&mut pr, turn);
            }
            pr.vel.y -= def.gravity * self.gravity_at(pr.pos) * DT;

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
                if def.blast_radius > 0.0 || matches!(def.special, Special::Cluster(..) | Special::Teleport | Special::Gravity(..)) {
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

    /// Mine: does it go off this tick? Either a blast from last tick reached it
    /// (armed or not, and rock in between is cover, as for worms), or, once
    /// armed, an enemy is within `radius` of it with a clear line. The owner can
    /// walk over their own mine; a blast still sets it off.
    fn mine_triggered(&self, pr: &Projectile, def: &WeaponDef, radius: f32, blasts: &[(Vec3, f32)]) -> bool {
        // Look from a little above the mine: it rests on the ground, and a ray
        // that starts in the rock would see it as cover.
        let eye = pr.pos + Vec3::Y * MINE_EYE;
        let blasted = blasts.iter().any(|&(at, reach)| {
            let d = at.distance(eye);
            d < reach && (d < 0.1 || self.grid.raycast(at, (eye - at) / d, d).is_none())
        });
        if blasted {
            return true;
        }
        if def.lifetime - pr.life < MINE_ARM {
            return false;
        }
        self.worms.iter().enumerate().any(|(j, worm)| {
            if j == pr.owner as usize || !worm.alive {
                return false;
            }
            let (a, b) = worm.axis();
            let near = closest_point_on_segment(pr.pos, a, b);
            near.distance(pr.pos) - WORM_RADIUS < radius && lash_clear(&self.grid, eye, near)
        })
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

    pub(crate) fn ignite(&mut self, victim: usize, by: u8) {
        let worm = &mut self.worms[victim];
        if worm.alive {
            worm.burn = BURN_TIME;
            worm.burn_by = by;
        }
    }

    /// Every tick: burning worms take damage on the `BURN_TICK` beat. (Touching
    /// lava is what sets them alight: see `scorch_with_lava`.)
    pub(crate) fn step_burning(&mut self) {
        for j in 0..self.worms.len() {
            let worm = &mut self.worms[j];
            if worm.burn <= 0.0 {
                continue;
            }
            if !worm.alive {
                worm.burn = 0.0;
                continue;
            }
            // A burn tick is a multiple of `BURN_TICK` seconds left being
            // crossed: it needs no timer of its own.
            let (before, after) = (worm.burn, worm.burn - DT);
            worm.burn = after.max(0.0);
            if (before / BURN_TICK).floor() != (after / BURN_TICK).floor() {
                let by = worm.burn_by as usize;
                self.damage(j, BURN_DAMAGE, LAVA, by, Vec3::ZERO);
            }
        }
    }

    fn start_zone(&mut self, kind: ZoneKind, centre: Vec3, radius: f32, seconds: f32, owner: u8) {
        self.zones.push(Zone { kind, centre, radius, left: seconds, owner });
        self.events.push(Event::ZoneStarted { kind, pos: centre, radius, seconds });
    }

    /// A projectile ended its flight against rock or a worm.
    fn impact(&mut self, pr: &Projectile, def: &WeaponDef, victim: Option<usize>) {
        let dir = pr.vel.normalize_or_zero();
        match def.special {
            Special::Cluster(count, child) => return self.burst(pr, dir, count, child),
            Special::Teleport => return self.teleport(pr, dir, victim),
            Special::Gravity(scale, radius, seconds) => {
                // On a worm or on rock, the zone is centred where the orb stopped.
                let at = self.free_point(pr.pos, dir, PROJECTILE_RADIUS).unwrap_or(pr.pos);
                return self.start_zone(ZoneKind::Gravity(scale), at, radius, seconds, pr.owner);
            }
            _ => {}
        }
        if def.blast_radius > 0.0 {
            // A shell that hit rock is a few centimetres inside it. Blow it up on
            // the side it came from, so the rock it hit is between the blast
            // and whoever hides behind it.
            let at = self.free_point(pr.pos, dir, PROJECTILE_RADIUS).unwrap_or(pr.pos);
            self.explode(at, pr.weapon, pr.owner as usize);
            return;
        }
        if let Some(v) = victim {
            self.damage(v, def.damage, pr.weapon, pr.owner as usize, dir * def.knockback);
        } else {
            // It ended on rock. A bullet that stops against a wall must still be
            // *seen* and *heard* stopping there: the viewer turns this into a
            // spark, a scorch and a tick of sound. The gradient of the distance
            // field is the surface normal; a degenerate one falls back to the
            // incoming direction.
            let at = self.free_point(pr.pos, dir, PROJECTILE_RADIUS).unwrap_or(pr.pos);
            let normal = {
                let n = self.grid.gradient(at);
                if n.length_squared() > 1.0e-6 {
                    n.normalize()
                } else {
                    -dir
                }
            };
            // A weapon with a terrain radius bites a hole where it lands. The
            // blaster is the everyday digging gun, so its bolt must move rock.
            if def.terrain > 0.0 {
                let shape = Shape::Sphere { center: at, radius: def.terrain };
                self.grid.apply(shape, Op::Carve);
                self.events.push(Event::Edit { shape, op: Op::Carve });
            }
            self.events.push(Event::Impact { weapon: pr.weapon, pos: at, normal });
        }
    }

    /// Damage falls off linearly with distance, and only reaches worms who
    /// can see the blast: rock is cover. That is what makes a built lump an
    /// answer to a rocket or a Nova. Worms are hurt before the crater is cut,
    /// so the rock that a blast destroys still shields you from that blast.
    fn explode(&mut self, pos: Vec3, weapon: u8, owner: usize) {
        let def = &WEAPONS[weapon as usize];
        // The gradient of the distance field is the surface normal. Only trust
        // it close to rock: a shell that burst against a worm in open air is
        // nowhere near a surface, and the ring should fall back to flat air.
        let normal = {
            let n = self.grid.gradient(pos);
            if self.grid.sample(pos).abs() < 1.5 && n.length_squared() > 1.0e-6 {
                n.normalize()
            } else {
                Vec3::Y
            }
        };
        self.events.push(Event::Explosion { weapon, pos, normal, radius: def.blast_radius });
        self.blasts.push((pos, def.blast_radius));

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
            self.damage(v, def.blast_damage * falloff, weapon, owner, away.normalize() * def.knockback * falloff);
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

pub(crate) fn point_segment_distance(p: Vec3, a: Vec3, b: Vec3) -> f32 {
    p.distance(closest_point_on_segment(p, a, b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::weapons::{
        BLASTER, BOMB, BOMBLET, BOUNCER, BUILD, CLUSTER, HEAVY, LAVA, MOON, NOVA, RAILGUN, ROCKET, SWARM, TELEPORT,
        TRACTOR, WHIP,
    };
    use glam::IVec3;
    use sdf::Grid;
    use std::f32::consts::FRAC_PI_2;

    // Headless like the movement tests: a few worms in a 32 x 16 x 32 m box and a
    // list of inputs. Aiming is by angles; worm 0 looks along +z at yaw 0, and
    // `LEVEL` is the pitch that aims from its eye at the middle of a worm 10 m
    // ahead.
    const SIZE: IVec3 = IVec3::new(8, 4, 8);
    const LEVEL: f32 = -0.06;

    fn flat(p: Vec3) -> f32 {
        p.y - 4.0
    }

    /// A 30 degree ramp rising towards +x. The slope that turns a horizontal
    /// knockback into an upward one.
    fn ramp(p: Vec3) -> f32 {
        let (s, c) = 30f32.to_radians().sin_cos();
        (p.y - 4.0 - p.x * s / c) * c
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
    fn hold_at(w: &mut World, weapon: u8, yaw: f32, pitch: f32, ticks: usize) -> Vec<Event> {
        let mut events = Vec::new();
        for t in 0..ticks {
            w.step(&[Input { fire: t == 0, weapon, yaw, pitch, ..Input::default() }]);
            events.extend(w.drain_events());
        }
        events
    }

    fn hold(w: &mut World, weapon: u8, pitch: f32, ticks: usize) -> Vec<Event> {
        hold_at(w, weapon, 0.0, pitch, ticks)
    }

    fn explosions(events: &[Event]) -> usize {
        events.iter().filter(|e| matches!(e, Event::Explosion { .. })).count()
    }

    fn duel() -> World {
        arena(flat, &[(16.0, 16.0), (16.0, 26.0)])
    }

    #[test]
    fn an_orb_starts_a_gravity_zone_where_it_lands() {
        let mut w = arena(flat, &[(16.0, 4.0)]);
        let events = hold(&mut w, MOON, 0.0, 90);
        let zone = events.iter().find_map(|e| match e {
            Event::ZoneStarted { kind, pos, radius, .. } => Some((*kind, *pos, *radius)),
            _ => None,
        });
        let (kind, pos, radius) = zone.expect("the orb landed and started a zone");
        assert_eq!(kind, ZoneKind::Gravity(0.3));
        assert!(pos.z > 8.0 && pos.y < 5.0, "it landed on the floor ahead: {pos}");
        assert_eq!(radius, 28.0);
        assert_eq!(w.zones.len(), 1);
    }

    #[test]
    fn a_shell_lobbed_inside_a_moon_zone_peaks_higher() {
        let peak = |zone: bool| {
            let mut w = arena(flat, &[(16.0, 4.0)]);
            if zone {
                w.zones.push(Zone { kind: ZoneKind::Gravity(0.3), centre: Vec3::new(16.0, 6.0, 16.0), radius: 30.0, left: 8.0, owner: 0 });
            }
            hold(&mut w, CLUSTER, 0.0, 1);
            let mut top = 0.0f32;
            for _ in 0..40 {
                w.step(&[]);
                top = w.projectiles.iter().map(|p| p.pos.y).fold(top, f32::max);
            }
            top
        };
        let (inside, outside) = (peak(true), peak(false));
        assert!(inside > outside + 2.0, "inside {inside} vs outside {outside}");
    }

    /// Worm 0 holds the lava gun's trigger down for `ticks` ticks, aiming a
    /// little above level (about 6 degrees) so the stream lands inside the arena.
    fn hose(w: &mut World, ticks: usize) -> Vec<Event> {
        let mut events = Vec::new();
        for _ in 0..ticks {
            w.step(&[Input { fire: true, weapon: LAVA, pitch: 0.1, ..Input::default() }]);
            events.extend(w.drain_events());
        }
        events
    }

    fn lava_ammo(w: &World) -> u16 {
        w.worms[0].weapons.iter().find(|s| s.id == LAVA).expect("everyone carries it").ammo
    }

    /// Three worms: 0 holds the gun, 1 and 2 stand away from the stream.
    fn lava_arena() -> World {
        arena(flat, &[(16.0, 4.0), (28.0, 28.0), (4.0, 28.0)])
    }

    #[test]
    fn holding_the_trigger_throws_a_bit_every_tick_and_letting_go_stops_it() {
        let mut w = lava_arena();
        let events = hose(&mut w, 30);
        let shots = events.iter().filter(|e| matches!(e, Event::Shot { weapon: LAVA, .. })).count();
        assert!((29..=30).contains(&shots), "60 Hz for half a second: {shots}");
        assert_eq!(w.lava.len(), shots);
        assert_eq!(lava_ammo(&w), 200 - shots as u16);
        run(&mut w, 30);
        assert_eq!(w.lava.len(), shots, "nothing more once the trigger is up");
        let farthest = w.lava.iter().map(|b| b.pos.z).fold(0.0, f32::max);
        assert!(farthest > 4.0 + 8.0, "the stream leaves the gun fast and far: {farthest}");
    }

    #[test]
    fn a_stream_lands_in_front_of_the_worm_and_pools_on_the_floor() {
        let mut w = lava_arena();
        hose(&mut w, 90);
        run(&mut w, 90);
        assert!(w.lava.len() >= 40, "{} bits", w.lava.len());
        assert!(w.lava.iter().all(|b| b.pos.y < 4.0 + 0.8), "all are on the floor by now");
        assert!(w.lava.iter().all(|b| w.grid.sample(b.pos) > crate::lava::LAVA_BIT_RADIUS * 0.5), "none sinks in");
        let reach = w.lava.iter().map(|b| b.pos.z).fold(0.0, f32::max);
        assert!(reach > 4.0 + 8.0, "a fire hose: it carries past 8 m even level: {reach}");
    }

    #[test]
    fn the_cheat_makes_every_shot_free() {
        let mut w = lava_arena();
        w.infinite_ammo = true;
        hose(&mut w, 300);
        assert_eq!(lava_ammo(&w), 200, "a magazine that never drains never reloads");
        w.infinite_ammo = false;
        hose(&mut w, 10);
        assert!(lava_ammo(&w) < 200, "and without the cheat it costs again");
    }

    #[test]
    fn frozen_lava_takes_up_about_the_room_the_liquid_did() {
        // Rock standing above the flat floor (top at y = 4), counted on a 0.1 m lattice.
        fn rock_volume(w: &World) -> f32 {
            let mut cells = 0;
            for ix in 60..260 {
                for iy in 1..60 {
                    for iz in 60..280 {
                        let p = Vec3::new(ix as f32 * 0.1, 4.0 + iy as f32 * 0.1, iz as f32 * 0.1);
                        if w.grid.sample(p) < 0.0 {
                            cells += 1;
                        }
                    }
                }
            }
            cells as f32 * 0.001
        }
        let mut w = lava_arena();
        let before = rock_volume(&w);
        let mut events = hose(&mut w, 60);
        events.extend(run(&mut w, 600));
        let bits = events.iter().filter(|e| matches!(e, Event::LavaFroze { .. })).count() as f32;
        let ball = 4.0 / 3.0 * std::f32::consts::PI * crate::lava::LAVA_BIT_RADIUS.powi(3);
        let per_bit = (rock_volume(&w) - before) / bits / ball;
        assert!(per_bit < 1.6, "{bits} bits froze into {} m3: {per_bit} balls of lava each", rock_volume(&w) - before);
    }

    #[test]
    fn lava_runs_downhill() {
        let mut w = arena(ramp, &[(28.0, 28.0)]);
        let at = Vec3::new(14.0, 4.0 + 14.0 * 30f32.to_radians().tan() + 0.6, 16.0);
        for k in 0..30 {
            w.spawn_lava_bit(at + Vec3::new(0.0, 0.0, (k % 6) as f32 * 0.5), Vec3::ZERO, 5.0, 0);
        }
        let start = w.lava.iter().map(|b| b.pos.x).sum::<f32>() / 30.0;
        run(&mut w, 120);
        let end = w.lava.iter().map(|b| b.pos.x).sum::<f32>() / w.lava.len() as f32;
        assert!(end < start - 1.0, "the ramp rises towards +x, so the lava ends lower: {start} -> {end}");
    }

    #[test]
    fn lava_freezes_into_rock_that_is_one_surface_with_no_gaps() {
        let mut w = lava_arena();
        let before = w.grid.checksum();
        let mut events = hose(&mut w, 90);
        events.extend(run(&mut w, 600));
        assert!(w.lava.is_empty(), "every bit froze or went: {} left", w.lava.len());
        assert!(w.grid.checksum() != before, "the terrain changed");
        let frozen: Vec<Vec3> = events
            .iter()
            .filter_map(|e| match e {
                Event::LavaFroze { pos } => Some(*pos),
                _ => None,
            })
            .collect();
        assert!(frozen.len() >= 35, "most of the bits froze onto the rock: {}", frozen.len());
        // Gaps: the midpoint of any two neighbouring bits must be inside rock
        // now. A pile of separate balls would leave air between them.
        let mut pairs = 0;
        for (i, a) in frozen.iter().enumerate() {
            for b in &frozen[i + 1..] {
                if a.distance(*b) < 0.9 {
                    pairs += 1;
                    let mid = (*a + *b) * 0.5;
                    assert!(w.grid.sample(mid) < 0.1, "a gap at {mid}: {}", w.grid.sample(mid));
                }
            }
        }
        assert!(pairs > 20, "the bits lay close together: {pairs} pairs");
    }

    /// Stop the stream, then stand worm 1 on the last bit that left the gun.
    fn stand_in_the_lava(w: &mut World) {
        let at = w.lava.last().expect("the hose threw some").pos;
        w.worms[1].reset_motion(Vec3::new(at.x, 4.05, at.z));
    }

    #[test]
    fn a_worm_in_the_lava_burns_and_the_thrower_gets_the_kill_but_a_far_one_is_fine() {
        let mut w = lava_arena();
        hose(&mut w, 40);
        run(&mut w, 40);
        stand_in_the_lava(&mut w);
        run(&mut w, 20);
        assert!(w.worms[1].burn > 0.0, "touching lava sets it alight");
        assert!(w.worms[1].hp < MAX_HP);
        // Standing in it while it cools costs a good part of the health; a worm
        // that was already hurt does not survive.
        w.worms[1].hp = 8.0;
        for _ in 0..600 {
            w.step(&[]);
            if !w.worms[1].alive {
                break;
            }
            w.worms[1].invuln = 0.0;
        }
        assert!(!w.worms[1].alive, "a hurt worm that stays dies");
        assert_eq!(w.worms[0].kills, 1);
        assert_eq!(w.worms[2].hp, MAX_HP, "far from the stream nothing burns");
        assert_eq!(w.worms[2].burn, 0.0);
    }

    #[test]
    fn the_hose_does_not_burn_the_worm_holding_it() {
        let mut w = lava_arena();
        hose(&mut w, 60);
        assert_eq!(w.worms[0].burn, 0.0, "the bits leave from the muzzle, touching the shooter");
        assert_eq!(w.worms[0].hp, MAX_HP);
    }

    #[test]
    fn lava_never_sets_rock_on_a_worm() {
        let mut w = lava_arena();
        hose(&mut w, 40);
        run(&mut w, 40);
        stand_in_the_lava(&mut w);
        for _ in 0..600 {
            w.step(&[]);
            w.worms[1].hp = MAX_HP;
        }
        assert!(body_fits(&w.grid, w.worms[1].pos), "the worm is not sealed in");
    }

    #[test]
    fn the_magazine_is_200_bits_and_the_world_never_holds_more_than_the_cap() {
        let mut w = arena(flat, &[(16.0, 16.0)]);
        let events = hose(&mut w, 300);
        let shots = events.iter().filter(|e| matches!(e, Event::Shot { weapon: LAVA, .. })).count();
        assert_eq!(shots, 200, "then it reloads");
        for _ in 0..400 {
            w.spawn_lava_bit(Vec3::new(16.0, 8.0, 16.0), Vec3::ZERO, 5.0, 0);
        }
        assert_eq!(w.lava.len(), crate::lava::LAVA_MAX);
    }

    #[test]
    fn a_heavy_orb_pulls_harder() {
        let mut w = arena(flat, &[(16.0, 4.0)]);
        hold(&mut w, HEAVY, 0.0, 90);
        assert_eq!(w.zones.len(), 1);
        assert_eq!(w.zones[0].kind, ZoneKind::Gravity(2.5));
    }

    #[test]
    fn a_shot_that_hits_rock_reports_the_strike() {
        let mut w = arena(flat, &[(16.0, 16.0)]);
        // Aim well down so the bolt buries itself in the floor ahead of the worm.
        let events = hold(&mut w, BLASTER, -1.0, 30);
        let impact = events
            .iter()
            .find_map(|e| {
                if let Event::Impact { weapon, pos, normal } = e {
                    Some((*weapon, *pos, *normal))
                } else {
                    None
                }
            })
            .expect("a bolt that stops on rock reports where");
        assert_eq!(impact.0, BLASTER);
        assert!(impact.1.y > 3.0 && impact.1.y < 5.0, "it struck near the floor: {}", impact.1.y);
        assert!(impact.2.y > 0.5, "the floor points up: {:?}", impact.2);
        // The everyday gun also digs: a blaster bolt leaves a hole wide enough
        // to see. A radius barely larger than one voxel (0.25 m) looked like no
        // damage at all on the chunky terrain.
        let carve = events
            .iter()
            .find_map(|e| match e {
                Event::Edit { shape: Shape::Sphere { radius, .. }, op: Op::Carve } => Some(*radius),
                _ => None,
            })
            .expect("the bolt bites the rock it hits");
        assert!(carve >= 0.45, "the hole reads on the 0.25 m voxel grid: {carve} m");
    }

    #[test]
    fn a_blaster_bolt_hurts_the_worm_it_hits_and_only_that() {
        let mut w = duel();
        let mut events = shoot(&mut w, BLASTER, LEVEL);
        events.extend(run(&mut w, 30));
        assert!((w.worms[1].hp - 88.0).abs() < 1.0e-3, "a hit is 12 damage: {}", w.worms[1].hp);
        assert!(events.iter().any(|e| matches!(e, Event::Hit { victim: 1, .. })));
        // The hit carries the push, so the viewer's blood follows the shot.
        let push = events
            .iter()
            .find_map(|e| match e {
                Event::Hit { dir, .. } => Some(*dir),
                _ => None,
            })
            .expect("a hit");
        assert!(push.z > 0.5, "the blast pushes away from the shooter: {push:?}");

        let mut w = duel();
        shoot_at(&mut w, BLASTER, 1.0, LEVEL); // aimed well to the side
        run(&mut w, 30);
        assert_eq!(w.worms[1].hp, MAX_HP);
    }

    #[test]
    fn a_cluster_shows_its_arc_and_where_it_will_land() {
        // A bigger arena than the others: the mortar's minimum arc now carries
        // it farther than the 32 m test box, and the point is to land on ground.
        let mut w = World::new(Grid::from_fn(IVec3::new(16, 4, 16), flat), &[(16.0, 16.0)], 1);
        run(&mut w, 30);
        // Aim slightly down: the launch loft turns that into a still-useful arc.
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
    fn the_cluster_is_a_mortar_that_launches_upward() {
        let mut w = arena(flat, &[(16.0, 16.0)]);
        w.worms[0].current = CLUSTER as usize;
        // Even a level or downward crosshair must not fire a flat shell: the
        // weapon's whole identity is the arc.
        for pitch in [0.0, -0.5] {
            w.worms[0].pitch = pitch;
            let (_, shot) = w.shot_plan(0);
            assert!(shot.y > 0.15, "a mortar arcs upward: {shot:?}");
        }
        // And the preview is the same arc the shell will fly.
        w.worms[0].pitch = 0.0;
        let path = w.aim_path(0).expect("a mortar shows its arc");
        let peak = path.points.iter().map(|p| p.y).fold(0.0, f32::max);
        let ground = w.worms[0].pos.y;
        assert!(peak > ground + 3.0, "the arc climbs: peak {peak} from {ground}");
    }

    #[test]
    fn a_blast_and_a_hit_name_the_weapon_that_caused_them() {
        // The viewer, the audio and the camera each author their response per
        // weapon, so the event has to carry which one it was.
        let mut w = arena(flat, &[(16.0, 16.0)]);
        let mut events = shoot(&mut w, ROCKET, -0.2);
        events.extend(run(&mut w, 60));
        assert!(events.iter().any(|e| matches!(e, Event::Explosion { weapon: ROCKET, .. })));
        let ground = events
            .iter()
            .find_map(|e| if let Event::Explosion { normal, .. } = e { Some(*normal) } else { None })
            .expect("the rocket exploded");
        assert!(ground.y > 0.5, "the ring lies on the ground: {ground:?}");

        let mut w = arena(flat, &[(16.0, 16.0)]);
        let mut events = hold(&mut w, NOVA, -0.2, 90);
        events.extend(run(&mut w, 90));
        assert!(events.iter().any(|e| matches!(e, Event::Explosion { weapon: NOVA, .. })));

        // A cluster is one shell, but seven bomblet blasts.
        let mut w = arena(flat, &[(16.0, 16.0), (16.0, 30.0)]);
        let mut events = shoot(&mut w, CLUSTER, 0.3);
        for _ in 0..120 {
            w.step(&[]);
            events.extend(w.drain_events());
            if w.projectiles.iter().any(|p| p.weapon == BOMBLET) {
                break;
            }
        }
        events.extend(run(&mut w, 60));
        let pops: Vec<u8> = events
            .iter()
            .filter_map(|e| if let Event::Explosion { weapon, .. } = e { Some(*weapon) } else { None })
            .collect();
        assert_eq!(pops.len(), 12);
        assert!(pops.iter().all(|&id| id == BOMBLET), "the children, not the shell: {pops:?}");

        // A direct hit says which gun did it, not only how hard.
        let mut w = duel();
        let mut events = shoot(&mut w, BLASTER, LEVEL);
        events.extend(run(&mut w, 30));
        assert!(events.iter().any(|e| matches!(e, Event::Hit { weapon: BLASTER, .. })));

        let mut w = arena(flat, &[(16.0, 16.0), (16.0, 26.0)]);
        let events = shoot(&mut w, RAILGUN, LEVEL);
        assert!(events.iter().any(|e| matches!(e, Event::Hit { weapon: RAILGUN, .. })));

        let mut w = arena(flat, &[(16.0, 16.0), (16.0, 20.0)]);
        w.worms[0].current = WHIP as usize;
        let events = hold(&mut w, WHIP, LEVEL, 24);
        assert!(events.iter().any(|e| matches!(e, Event::Hit { weapon: WHIP, .. })));
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
    fn the_kill_that_reaches_the_score_limit_wins_the_match() {
        let mut w = duel();
        w.score_limit = 2;
        w.worms[0].kills = 1;
        w.worms[1].hp = 5.0;
        let mut events = shoot(&mut w, BLASTER, LEVEL);
        events.extend(run(&mut w, 30));
        assert_eq!(w.winner, Some(0));
        let won: Vec<_> = events.iter().filter(|e| matches!(e, Event::MatchWon { worm: 0 })).collect();
        assert_eq!(won.len(), 1, "announced once");
    }

    #[test]
    fn one_kill_short_of_the_limit_wins_nothing() {
        let mut w = duel();
        w.score_limit = 2;
        w.worms[1].hp = 5.0;
        shoot(&mut w, BLASTER, LEVEL);
        run(&mut w, 30);
        assert_eq!(w.worms[0].kills, 1);
        assert_eq!(w.winner, None);
    }

    #[test]
    fn nobody_wins_by_killing_themselves() {
        // Suicide costs a point, so it can only move a score away from the limit.
        let mut w = duel();
        w.score_limit = 0;
        w.worms[1].hp = 5.0;
        w.damage(1, 50.0, BLASTER, 1, Vec3::ZERO);
        assert_eq!(w.worms[1].kills, -1);
        assert_eq!(w.winner, None, "-1 is below a limit of 0");
    }

    #[test]
    fn a_won_match_is_frozen_but_the_clock_runs() {
        let mut w = duel();
        w.winner = Some(0);
        w.worms[1].hp = 0.0;
        w.worms[1].alive = false;
        w.worms[1].respawn_timer = 0.5;
        let (pos, tick) = (w.worms[0].pos, w.tick);
        let mut events = shoot(&mut w, BLASTER, LEVEL);
        events.extend(run(&mut w, (RESPAWN_TIME / DT) as usize));
        assert!(events.is_empty(), "no shots, no respawns: {events:?}");
        assert!(w.projectiles.is_empty());
        assert!(!w.worms[1].alive, "the dead stay dead");
        assert_eq!(w.worms[0].pos, pos);
        assert!(w.tick > tick, "snapshots are labelled with the tick, so it keeps counting");
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
            w.explode(Vec3::new(16.0, 5.0, 19.5), BOMB, 0);
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
    fn a_mine_drops_to_the_ground_and_lies_there_without_a_fuse() {
        let mut w = arena(flat, &[(16.0, 16.0), (16.0, 40.0)]);
        shoot(&mut w, BOMB, 0.0);
        run(&mut w, 30); // falls, hops once or twice, then creeps to rest
        let mine = &w.projectiles[0];
        assert_eq!(mine.vel, Vec3::ZERO, "at rest");
        assert!(mine.pos.y > 4.0 && mine.pos.y < 4.4, "on the ground: {:?}", mine.pos);

        // The owner stands right on it, the enemy is far away: nothing happens.
        let events = run(&mut w, 120);
        assert_eq!(w.projectiles.len(), 1, "still waiting");
        assert_eq!(explosions(&events), 0);
    }

    #[test]
    fn a_mine_goes_off_when_an_enemy_comes_close_but_not_before_it_is_armed() {
        let mut w = arena(flat, &[(16.0, 16.0), (16.0, 40.0)]);
        shoot(&mut w, BOMB, 0.0);
        run(&mut w, 60); // past the arming time
        let at = w.projectiles[0].pos;
        // 2 m away: inside the trigger radius, nowhere near touching.
        w.worms[1].pos = Vec3::new(at.x + 2.0, 4.05, at.z);
        w.worms[1].vel = Vec3::ZERO;
        let events = run(&mut w, 3);
        assert_eq!(explosions(&events), 1, "set off by proximity");
        assert!(w.worms[1].hp < MAX_HP);

        // A fresh mine ignores that same enemy while it is still arming.
        let mut w = arena(flat, &[(16.0, 16.0), (16.0, 40.0)]);
        shoot(&mut w, BOMB, 0.0);
        run(&mut w, 5);
        let at = w.projectiles[0].pos;
        w.worms[1].pos = Vec3::new(at.x + 2.0, 4.05, at.z);
        w.worms[1].vel = Vec3::ZERO;
        let events = run(&mut w, 5);
        assert_eq!(explosions(&events), 0, "not armed yet");
    }

    #[test]
    fn a_blast_sets_off_a_mine_but_rock_in_between_protects_it() {
        let blast_next_to_mine = |f: fn(Vec3) -> f32, at: Vec3| {
            let mut w = arena(f, &[(16.0, 16.0), (16.0, 40.0)]);
            shoot(&mut w, BOMB, 0.0);
            run(&mut w, 30);
            let mine = w.projectiles[0].pos;
            w.explode(mine + at, ROCKET, 1);
            let events = run(&mut w, 3);
            events
                .iter()
                .filter(|e| matches!(e, Event::Explosion { weapon: BOMB, .. }))
                .count()
        };
        assert_eq!(blast_next_to_mine(flat, Vec3::new(0.0, 0.3, 2.0)), 1, "the mine goes off with it");
        assert_eq!(blast_next_to_mine(flat, Vec3::new(0.0, 0.3, 40.0)), 0, "out of reach it does not");
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
    fn a_cluster_shell_bursts_into_twelve_bomblets_that_each_explode() {
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
        assert_eq!(seen, 12, "the burst");
        let events = run(&mut w, 60);
        assert_eq!(explosions(&events), 12);
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
    fn the_block_weapon_lies_flat_when_aimed_steeply_up_or_down() {
        for pitch in [0.7_f32, -0.7] {
            let mut w = arena(flat, &[(16.0, 16.0)]);
            let events = shoot(&mut w, BUILD, pitch);
            let shape = events
                .iter()
                .find_map(|e| match e {
                    Event::Edit { shape: shape @ Shape::Box { .. }, .. } => Some(*shape),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("a slab is placed at pitch {pitch}"));
            let Shape::Box { centre, half, .. } = shape else { unreachable!() };
            assert!(half.y < half.z && half.y < half.x, "flat, not upright: {half:?}");
            let worm = &w.worms[0];
            if pitch > 0.0 {
                assert!(centre.y > worm.eye().y, "a roof over the head: {}", centre.y);
                assert!(w.grid.sample(centre) < -0.2, "solid at its centre");
            } else {
                assert!(centre.y < worm.pos.y, "a floor under the feet: {}", centre.y);
            }
            let (a, b) = worm.axis();
            for p in [a, (a + b) * 0.5, b] {
                assert!(shape.distance(p) > 0.0, "the slab is not on the worm: {p:?}");
            }
        }
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
    fn a_whip_sweep_crosses_the_front_from_one_side_to_the_other() {
        // The worm faces +Z, so its right (`side`) is -X.
        let (fwd, side, lean) = whip_frame(Vec3::Z, 0.0);
        assert_eq!((fwd, side, lean), (Vec3::Z, Vec3::NEG_X, 0.0));
        let start = whip_dir(WhipStyle::Sweep, fwd, side, lean, false, 0.0);
        let mid = whip_dir(WhipStyle::Sweep, fwd, side, lean, false, 0.5);
        let end = whip_dir(WhipStyle::Sweep, fwd, side, lean, false, 1.0);
        assert!(start.x < -0.9 && end.x > 0.9, "it runs right to left: {start:?} {end:?}");
        assert!(mid.z > 0.99, "and passes straight ahead in the middle: {mid:?}");
        // The mirrored swing runs back the other way.
        let back = whip_dir(WhipStyle::Sweep, fwd, side, lean, true, 0.0);
        assert!(back.x > 0.9, "a flipped swing starts on the left: {back:?}");
        // The lash is level, and its length follows the swing.
        assert!(start.y.abs() < 1.0e-5 && end.y.abs() < 1.0e-5);
        assert_eq!(whip_reach(9.5, 0.0), WHIP_REST);
        assert!((whip_reach(9.5, 0.5) - 9.5).abs() < 1.0e-4, "fully paid out in the live part");
        assert!(whip_reach(9.5, 1.0) <= WHIP_REST + 1.0e-4, "and drawn back in at the end");
    }

    #[test]
    fn the_swing_plane_leans_towards_the_aim_but_only_so_far() {
        let up = |deg: f32| whip_frame(Vec3::new(0.0, deg.to_radians().sin(), deg.to_radians().cos()), 0.0).2;
        assert!((up(10.0) - 10f32.to_radians()).abs() < 1.0e-4, "a small aim is followed");
        assert_eq!(up(80.0), WHIP_LEAN, "a steep one is clamped");
        assert_eq!(up(-80.0), -WHIP_LEAN);
        let (fwd, side, lean) = whip_frame(Vec3::Y, 1.0);
        assert!(fwd.is_finite() && side.is_finite() && lean == WHIP_LEAN, "straight up still has a heading");
    }

    #[test]
    fn a_whip_smash_goes_overhead_and_comes_down_in_front() {
        let (fwd, side, lean) = whip_frame(Vec3::Z, 0.0);
        let start = whip_dir(WhipStyle::Smash, fwd, side, lean, false, 0.0);
        let end = whip_dir(WhipStyle::Smash, fwd, side, lean, false, 1.0);
        assert!(start.y > 0.9, "it starts over the worm: {start:?}");
        assert!(end.y < -0.8 && end.z > 0.3, "and comes down in front: {end:?}");
    }

    #[test]
    fn the_plasma_whip_hurts_a_worm_it_lashes() {
        let mut w = arena(flat, &[(16.0, 16.0), (16.0, 20.0)]);
        w.worms[0].current = WHIP as usize;
        let events = hold(&mut w, WHIP, LEVEL, 24);
        assert!(w.worms[1].hp < MAX_HP, "the lash reached it: {}", w.worms[1].hp);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::Whip { style: WhipStyle::Sweep, .. })),
            "and the viewer is told to draw a left-to-right sweep"
        );
    }

    #[test]
    fn a_whip_hit_along_a_slope_does_not_send_the_victim_skating_away() {
        // The lash knocks the victim up the hill. The slope turns the push into
        // upward speed, which used to keep it airborne for the whole tumble and
        // far beyond it: the worm skated along the surface with no friction.
        let mut w = World::new(Grid::from_fn(IVec3::new(16, 8, 8), ramp), &[(14.0, 16.0), (17.0, 16.0)], 1);
        run(&mut w, 180);
        assert!(w.worms.iter().all(|v| v.on_ground), "both stand on the hill");
        w.worms[0].current = WHIP as usize;
        let start = w.worms[1].pos;
        // Swing uphill at the worm above.
        hold_at(&mut w, WHIP, FRAC_PI_2, 0.2, 30);
        assert!(w.worms[1].hp < MAX_HP, "the lash landed");
        run(&mut w, 60);
        let moved = (w.worms[1].pos - start).length();
        assert!(w.worms[1].on_ground, "the victim is standing, not skating");
        assert!(moved < 3.0, "a hit shoves it a little, not to the end of the map: {moved} m");
    }

    #[test]
    fn the_plasma_whip_draws_back_before_it_cracks() {
        let mut w = arena(flat, &[(16.0, 16.0), (16.0, 20.0)]);
        // The first part of the swing is the draw-back: the lash is still on
        // the side and the target straight ahead is untouched.
        let mut events = hold(&mut w, WHIP, LEVEL, 6);
        assert_eq!(w.worms[1].hp, MAX_HP, "nothing has been hit yet");
        assert!(events.iter().any(|e| matches!(e, Event::Whip { .. })), "but the swing has begun");
        assert!(w.worms[0].swing.is_some());
        // Then the lash comes through the middle and lands.
        events.extend(hold(&mut w, WHIP, LEVEL, 18));
        assert!(w.worms[1].hp < MAX_HP, "then the crack lands");
    }

    #[test]
    fn a_swing_hits_a_worm_once_however_long_it_passes_through() {
        let mut w = arena(flat, &[(16.0, 16.0), (16.0, 20.0)]);
        let events = hold(&mut w, WHIP, LEVEL, 18);
        let hits = events.iter().filter(|e| matches!(e, Event::Hit { weapon: WHIP, .. })).count();
        assert_eq!(hits, 1, "one swing, one hit");
        assert_eq!(w.worms[1].hp, MAX_HP - WEAPONS[WHIP as usize].damage);
    }

    #[test]
    fn the_whip_hits_a_worm_as_the_lash_passes_not_before() {
        // The swing starts on the right (-X) and ends on the left (+X), so the
        // worm on the right is reached before the one on the left.
        let mut w = arena(flat, &[(16.0, 16.0), (12.5, 18.0), (19.5, 18.0)]);
        let mut right = None;
        let mut left = None;
        for t in 0..18 {
            w.step(&[Input { fire: t == 0, weapon: WHIP, yaw: 0.0, pitch: LEVEL, ..Input::default() }]);
            w.drain_events();
            if right.is_none() && w.worms[1].hp < MAX_HP {
                right = Some(t);
            }
            if left.is_none() && w.worms[2].hp < MAX_HP {
                left = Some(t);
            }
        }
        let (right, left) = (right.expect("the right-hand worm is hit"), left.expect("the left-hand one too"));
        assert!(right + 3 < left, "the lash sweeps right to left: {right} then {left}");
    }

    #[test]
    fn the_whip_sweeps_a_long_arc_in_front() {
        // One worm near the far end of the reach, one well out to the left.
        let mut w = arena(flat, &[(16.0, 16.0), (16.0, 25.0), (19.5, 22.06)]);
        w.worms[0].current = WHIP as usize;
        hold(&mut w, WHIP, LEVEL, 24);
        assert!(w.worms[1].hp < MAX_HP, "the full 9 m reach is hit: {}", w.worms[1].hp);
        assert!(w.worms[2].hp < MAX_HP, "and the sweep covers the left side: {}", w.worms[2].hp);
    }

    #[test]
    fn the_plasma_whip_digs_into_the_rock() {
        let mut w = arena(flat, &[(16.0, 16.0)]);
        let before = w.grid.checksum();
        let events = hold_at(&mut w, WHIP, 0.0, -0.7, 24); // down at the ground
        assert_ne!(w.grid.checksum(), before, "the rock is dug");
        assert!(events.iter().any(|e| matches!(e, Event::Edit { op: Op::Carve, .. })));
    }

    #[test]
    fn the_whip_drags_a_flat_cylinder_along_a_wall_without_tunnelling() {
        // Flat ground and a wall 8 m ahead. Aim level, so the swing crosses the
        // wall in one plane.
        let mut w = arena(walled, &[(16.0, 16.0)]);
        w.worms[0].current = WHIP as usize;
        let hand_y = w.worms[0].muzzle().y;
        assert!(w.grid.sample(Vec3::new(16.0, hand_y, 27.0)) < 0.0, "the wall is solid before the swing");
        let events = hold_at(&mut w, WHIP, 0.0, 0.0, 24);
        let blades: Vec<(Vec3, Vec3, f32, f32)> = events
            .iter()
            .filter_map(|e| match e {
                Event::Edit { shape: Shape::Cylinder { a, b, radius, half_height, .. }, op: Op::Carve } => {
                    Some((*a, *b, *half_height * 2.0, *radius))
                }
                _ => None,
            })
            .collect();
        assert!(blades.len() >= 4, "the swing leaves a slit, not a dot: {}", blades.len());
        assert!(
            events.iter().all(|e| !matches!(e, Event::Edit { shape: Shape::Sphere { .. } | Shape::Capsule { .. }, .. })),
            "and nothing round is dug"
        );
        let mut min = Vec3::splat(f32::MAX);
        let mut max = Vec3::splat(f32::MIN);
        for (a, b, ..) in &blades {
            min = min.min(*a).min(*b);
            max = max.max(*a).max(*b);
        }
        assert!(max.y - min.y < 1.0, "the slit is level: y {}..{}", min.y, max.y);
        assert!(max.x - min.x > 4.0, "it runs along the wall: x {}..{}", min.x, max.x);
        let deepest = blades.iter().map(|b| b.2).fold(0.0, f32::max);
        assert!(deepest > 0.5 && deepest < 1.5, "it presses in under a metre: {deepest}");
        assert!(blades.iter().all(|b| b.3 <= 0.6), "and stays narrow");
        // The wall is 8 m thick on this map; the slit never goes through.
        assert!(w.grid.sample(Vec3::new(16.0, hand_y, 27.0)) < 0.0, "no tunnel through the wall");
        // Rock just above the slit is untouched.
        assert!(w.grid.sample(Vec3::new(16.0, hand_y + 1.0, 25.0)) < 0.0, "rock above the slit stays");
    }

    #[test]
    fn a_swing_is_deterministic() {
        let swing = || {
            let mut w = arena(walled, &[(16.0, 16.0), (12.0, 20.0)]);
            for t in 0..120 {
                w.step(&[Input { fire: true, weapon: WHIP, yaw: (t as f32 * 0.03).sin(), pitch: LEVEL, ..Input::default() }]);
                w.drain_events();
            }
            (w.grid.checksum(), w.worms[1].hp)
        };
        assert_eq!(swing(), swing());
    }

    #[test]
    fn the_whip_is_melee_and_keeps_swinging_while_held() {
        let mut w = arena(flat, &[(16.0, 16.0), (16.0, 24.0)]);
        w.worms[0].current = WHIP as usize;
        let mut swings = 0;
        let mut shots = 0;
        for _ in 0..150 {
            w.step(&[Input { fire: true, weapon: WHIP, yaw: 0.0, pitch: LEVEL, ..Input::default() }]);
            for e in w.drain_events() {
                match e {
                    Event::Whip { .. } => swings += 1,
                    Event::Shot { weapon: WHIP, .. } => shots += 1,
                    _ => {}
                }
            }
        }
        assert!(swings >= 2, "holding the trigger keeps the lash moving: {swings}");
        assert_eq!(shots, 0, "the whip is not a gun: no muzzle flash event");
        let state = &w.worms[0].weapons[WHIP as usize];
        assert_eq!(state.ammo, 0, "the whip has no magazine");
        assert_eq!(state.reload, 0.0, "and never reloads");
    }

    #[test]
    fn a_held_whip_alternates_left_and_right() {
        let mut w = arena(flat, &[(16.0, 16.0), (16.0, 24.0)]);
        w.worms[0].current = WHIP as usize;
        let mut flips = Vec::new();
        for _ in 0..200 {
            w.step(&[Input { fire: true, weapon: WHIP, yaw: 0.0, pitch: LEVEL, ..Input::default() }]);
            for e in w.drain_events() {
                if let Event::Whip { style: WhipStyle::Sweep, flip, .. } = e {
                    flips.push(flip);
                }
            }
        }
        assert!(flips.len() >= 3, "several swings while held: {flips:?}");
        for pair in flips.windows(2) {
            assert_ne!(pair[0], pair[1], "each swing mirrors the last: {flips:?}");
        }
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
        let events = hold(&mut w, WHIP, LEVEL, 24);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::Whip { style: WhipStyle::Smash, .. })),
            "the follow-up smashes downwards"
        );
    }

    #[test]
    fn holding_the_trigger_in_the_double_jump_smashes_only_once_per_airtime() {
        let mut w = arena(flat, &[(16.0, 16.0)]);
        w.worms[0].current = WHIP as usize;
        let height = w.worms[0].pos.y + 6.0;
        let mut whips = 0;
        for _ in 0..60 {
            // Hover in the double jump: the test is about the trigger, not the fall.
            let worm = &mut w.worms[0];
            worm.pos.y = height;
            worm.vel = Vec3::ZERO;
            worm.on_ground = false;
            worm.action = Action::DoubleJump;
            w.step(&[Input { fire: true, weapon: WHIP, pitch: LEVEL, ..Input::default() }]);
            whips += w.drain_events().iter().filter(|e| matches!(e, Event::Whip { .. })).count();
        }
        assert_eq!(whips, 1, "one stomp, however long the button is held");
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
