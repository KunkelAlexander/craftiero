// Turns the sim's worms into the capsules the renderer draws. The sim knows
// nothing about colours or how a worm looks; this is the only place that does.
//
// The worm is a stack of overlapping capsules with alternating shades, so it
// reads as a segmented body, plus a tail, a bulbous head, two eyes, tiny arms
// and the gun. There are no legs: it gets around by undulating, which is also
// what most of the animation below is about.

use crate::renderer::{DrawPrim, Sprite, WormSdf};
use glam::Vec3;
use sim::weapons::{
    BLASTER, BOMB, BOUNCER, BUILD, CLUSTER, NOVA, RAILGUN, ROCKET, SWARM, TELEPORT, TRACTOR, WEAPONS, WHIP,
};
use sim::{whip_arc, whip_dir, whip_reach, Action, WhipStyle, Worm, MAX_HP, WHIP_REST, WHIP_SWING_TIME, WORM_HEIGHT};
use std::f32::consts::TAU;

/// How tall the preview sculpt is, in its own units. The game worm is 1.8 m;
/// the SDF is scaled by `WORM_HEIGHT / SDF_MODEL` so it fills the same box.
const SDF_MODEL: f32 = 1.52;
/// The hands the SDF arms are built around, in the preview's local frame. The
/// shader pastes these in as its rest pose; the live hands are derived from
/// them below. The sim's muzzle offset (`HAND_RIGHT`/`HAND_FORWARD`/
/// `HAND_HEIGHT` in `sim/src/world.rs`) mirrors the right hand, so a shot
/// starts at the drawn gun; move both together if the pose changes.
pub(crate) const SDF_HAND_R: Vec3 = Vec3::new(0.108, 0.868, 0.682);
pub(crate) const SDF_HAND_L: Vec3 = Vec3::new(0.100, 0.870, 0.850);

/// One colour per player (as picked in a paint program).
const COLOURS: [Vec3; 4] = [
    Vec3::new(0.93, 0.42, 0.33), // coral
    Vec3::new(0.33, 0.58, 0.92), // blue
    Vec3::new(0.96, 0.80, 0.30), // yellow
    Vec3::new(0.45, 0.78, 0.40), // green
];
const EYE: Vec3 = Vec3::new(1.0, 0.97, 0.9);
const IRIS: Vec3 = Vec3::new(0.13, 0.42, 0.55);
const PUPIL: Vec3 = Vec3::new(0.04, 0.03, 0.06);
const MOUTH: Vec3 = Vec3::new(0.20, 0.07, 0.09);
const ROPE: Vec3 = Vec3::new(0.92, 0.86, 0.70);

/// Per-worm animation state. The sim is fixed-step and has no business knowing
/// how a worm looks, so the viewer remembers the action's age, the gait phase
/// and whether the worm just landed. That is what lets the double jump be
/// exactly one flip and a landing be a single squash.
#[derive(Clone, Copy, Debug)]
pub struct Anim {
    action: Action,
    /// Seconds since `action` changed.
    since: f32,
    /// Free-running clock for breathing and blinking.
    clock: f32,
    /// Gait phase; advances faster the quicker the worm runs.
    phase: f32,
    /// Set on the update where the worm touches down.
    landed: bool,
    was_grounded: bool,
    /// Seconds since this worm last fired, for the gun's recoil and the face's
    /// reaction. Starts old, so a freshly spawned worm does not kick.
    fire: f32,
    /// Seconds since this worm's current whip swing began, and the swing's
    /// fixed frame (the same one the sim hits and cuts in, from `Event::Whip`).
    /// The lash is carried by the worm every frame, so this is what turns the
    /// carried rope into a swing without spawning a whole new weapon effect.
    whip: f32,
    whip_style: WhipStyle,
    /// Which way the current swing crosses: false is right-to-left, true is
    /// left-to-right. The sim alternates it on a held trigger, and the body
    /// animation mirrors with it.
    whip_flip: bool,
    whip_fwd: Vec3,
    whip_side: Vec3,
    whip_lean: f32,
    /// The lash itself: a cosmetic rope that follows the hand.
    rope: Rope,
    /// Seconds since this worm was last hurt, and how hard. The flash and the
    /// flinch are drawn from this; the sim already moved the body.
    hurt: f32,
    hurt_damage: f32,
}

/// How long a hit flashes white, and how long the body keeps flinching. The
/// flash is deliberately shorter than a sim tick's worth of reaction: it reads
/// as an impact, not as a status effect.
const HURT_FLASH: f32 = 0.10;
const HURT_FLINCH: f32 = 0.40;
/// A health bar only appears over a worm that has actually lost health. It is
/// vertical so it stays readable from every camera angle: the third-person view
/// is rarely square to the body's right.
const BAR_STEP: f32 = 0.105;
const BAR_WIDTH: f32 = 0.30;
const BAR_LOW: Vec3 = Vec3::new(0.95, 0.14, 0.11);
const BAR_MID: Vec3 = Vec3::new(1.0, 0.72, 0.14);
const BAR_GOOD: Vec3 = Vec3::new(0.42, 0.94, 0.32);

impl Default for Anim {
    fn default() -> Anim {
        Anim {
            action: Action::Idle,
            since: 1.0,
            clock: 1.0,
            phase: 0.0,
            landed: false,
            was_grounded: true,
            fire: 1.0,
            whip: 1.0,
            whip_style: WhipStyle::Sweep,
            whip_flip: false,
            whip_fwd: Vec3::Z,
            whip_side: Vec3::NEG_X,
            whip_lean: 0.0,
            rope: Rope::default(),
            hurt: 10.0,
            hurt_damage: 0.0,
        }
    }
}

impl Anim {
    /// Advance the clocks. `dt` is real frame time, not a sim tick, so the
    /// animation stays smooth on a display that is not exactly 60 Hz.
    pub fn update(&mut self, worm: &Worm, dt: f32) {
        let dt = dt.clamp(0.0, 0.1);
        if worm.action != self.action {
            self.action = worm.action;
            self.since = 0.0;
        } else {
            self.since = (self.since + dt).min(10.0);
        }
        self.landed = !self.was_grounded && worm.on_ground;
        self.was_grounded = worm.on_ground;
        self.clock = (self.clock + dt) % 1000.0;
        self.fire = (self.fire + dt).min(10.0);
        self.whip = (self.whip + dt).min(10.0);
        self.hurt = (self.hurt + dt).min(10.0);
        // Running makes the undulation quicker; idle keeps a slow ripple.
        let gait = if worm.on_ground {
            3.0 + worm.forward_vel.abs() * 0.55
        } else {
            1.5
        };
        self.phase = (self.phase + dt * gait) % TAU;
    }

    /// The worm pulled the trigger: reset the recoil clock.
    pub fn fired(&mut self) {
        self.fire = 0.0;
    }

    /// This worm was just hurt. `damage` scales the flash and the flinch; the
    /// sim has already applied the health loss and the knockback.
    pub fn hurt(&mut self, damage: f32) {
        self.hurt = 0.0;
        self.hurt_damage = damage.max(0.0);
    }

    /// A plasma-whip swing began: the carried lash follows it. The frame is
    /// the sim's, so the drawn swing and the cut it makes agree; `flip` mirrors
    /// the swing so a held trigger alternates direction.
    pub fn struck(&mut self, style: WhipStyle, flip: bool, fwd: Vec3, side: Vec3, lean: f32) {
        self.whip = 0.0;
        self.whip_style = style;
        self.whip_flip = flip;
        self.whip_fwd = fwd;
        self.whip_side = side;
        self.whip_lean = lean;
    }
}

// --- the plasma lash --------------------------------------------------------

/// How many nodes the lash rope has. More nodes bend more smoothly and cost a
/// little more to step and to draw (one capsule and one glow sprite each).
const LASH_NODES: usize = 18;
/// Which nodes the swing trail remembers, and how many snapshots of them.
const TRAIL_NODES: [usize; 6] = [3, 6, 9, 12, 15, 17];
const TRAIL_SNAPS: usize = 6;
/// Seconds between trail snapshots, and how long one stays visible.
const TRAIL_EVERY: f32 = 0.02;
const TRAIL_LIFE: f32 = 0.14;

/// The lash as a rope. Pattern: Verlet integration. Each node remembers where
/// it was, so its velocity is implied by the difference and never stored, and a
/// distance constraint between neighbours keeps the rope from stretching. The
/// hand pins node 0; everything else lags and whips on its own.
///
/// It is cosmetic. The sim owns the swing (hits and cuts follow the idealised
/// lash line, `sim::whip_dir`); the rope is only pulled towards that line, more
/// softly the further from the hand, so the tip trails the handle and snaps
/// round at the end of the arc, the way a real whip does.
#[derive(Clone, Copy, Debug)]
struct Rope {
    pos: [Vec3; LASH_NODES],
    prev: [Vec3; LASH_NODES],
    live: bool,
    /// Older shapes of the rope, for the glowing trail a swing leaves.
    trail: [[Vec3; 6]; TRAIL_SNAPS],
    trail_age: [f32; TRAIL_SNAPS],
    trail_clock: f32,
}

impl Default for Rope {
    fn default() -> Rope {
        Rope {
            pos: [Vec3::ZERO; LASH_NODES],
            prev: [Vec3::ZERO; LASH_NODES],
            live: false,
            trail: [[Vec3::ZERO; 6]; TRAIL_SNAPS],
            trail_age: [TRAIL_LIFE; TRAIL_SNAPS],
            trail_clock: 0.0,
        }
    }
}

/// Where a swing wants the lash this frame: its direction, its reach in metres,
/// and how strongly it is pulling (0 at rest, 1 mid-swing).
#[derive(Clone, Copy)]
struct SwingPull {
    dir: Vec3,
    reach: f32,
    weight: f32,
}

/// What the worm's walk lends the carried lash. `phase` is the gait phase
/// (it ticks slowly when idle and quickly when running), `run` is 0..1.
#[derive(Clone, Copy)]
struct Carry {
    phase: f32,
    run: f32,
}

/// The carried lash is a light, loose thing, not a hose. These four numbers are
/// what make it so; they interact, so tune them together by eye.
/// Radians of delay from the hand to the tip in the carried wave: the tip is
/// always a little behind the handle, which is what makes it curl.
const CARRY_WAVE: f32 = 2.4;
/// How firmly each node is pulled to its moving target (1/s). It has to be near
/// the sway's own speed (a run is about 8 rad/s), or the rope is a low-pass
/// filter that smooths the sway away. The tip is looser than the root, so it
/// follows with extra lag and overshoots a little.
const CARRY_PULL: f32 = 10.0;
/// How much of that pull the tip gives up (0 = as firm as the root).
const CARRY_TIP_LOOSE: f32 = 0.35;
/// Velocity kept per step. Lower than the swing's 0.996 so the sway settles
/// instead of ringing.
const CARRY_DAMP: f32 = 0.985;
/// Half the swing's gravity: a lash of plasma should float, not droop.
const CARRY_GRAVITY: f32 = 7.0;

impl Rope {
    /// The carried pose: a loose curl out in front. Each step of the walk sends
    /// a wave down the lash, so the tip swings left and right in front of the
    /// worm and lags the handle (an S-curve, not a swinging pole). Standing
    /// still keeps a small, slow version of the same sway, so it is never dead.
    /// It is only a soft pull, so the rope still swings with the worm; it just
    /// never lies in the dirt or knots up.
    fn rest_point(anchor: Vec3, face: Vec3, right: Vec3, len: f32, t: f32, carry: Carry) -> Vec3 {
        // Fractions of the lash length. `* t` pins the root to the hand and lets
        // the sway grow towards the tip.
        let amp = 0.16 + 0.60 * carry.run;
        let wave = carry.phase - t * CARRY_WAVE;
        let side = amp * wave.sin() * t;
        let lift = 0.08 * (t * std::f32::consts::PI).sin() - 0.10 * t * t + 0.04 * carry.run * (2.0 * wave).sin() * t;
        // Less forward than sideways: the lash must not just hang in front of the
        // worm. The two together stay inside the rope's length (0.5² + 0.76²).
        anchor + face * (len * 0.45 * t) + right * (len * side) + Vec3::Y * (len * lift)
    }

    #[allow(clippy::too_many_arguments)]
    fn step(
        &mut self,
        anchor: Vec3,
        face: Vec3,
        right: Vec3,
        carry: Carry,
        swing: Option<SwingPull>,
        rs: f32,
        dt: f32,
    ) {
        let rest_len = WHIP_REST * rs;
        let (len, w) = match swing {
            Some(s) => (s.reach.max(WHIP_REST) * rs, s.weight),
            None => (rest_len, 0.0),
        };
        let n1 = (LASH_NODES - 1) as f32;
        // A fresh rope, or one left far behind by a teleport, starts at rest.
        if !self.live || self.pos[0].distance(anchor) > 4.0 {
            for k in 0..LASH_NODES {
                let p = Rope::rest_point(anchor, face, right, rest_len, k as f32 / n1, carry);
                self.pos[k] = p;
                self.prev[k] = p;
            }
            self.live = true;
        }
        // Light when carried, heavy and crisp in a swing: blend by how hard the
        // swing is pulling, so the hand-over in both directions has no jolt.
        let damp = CARRY_DAMP + (0.996 - CARRY_DAMP) * w;
        let gravity = CARRY_GRAVITY + (14.0 - CARRY_GRAVITY) * w;
        let steps = ((dt * 120.0).ceil() as usize).clamp(1, 6);
        let h = dt / steps as f32;
        for _ in 0..steps {
            self.pos[0] = anchor;
            self.prev[0] = anchor;
            for k in 1..LASH_NODES {
                let t = k as f32 / n1;
                // Inertia and gravity: the rope falls and swings on its own.
                let vel = (self.pos[k] - self.prev[k]) * damp;
                self.prev[k] = self.pos[k];
                self.pos[k] += vel + Vec3::NEG_Y * (gravity * h * h);
                // The pulls. At rest a gentle one towards the moving carried
                // curl, looser towards the tip; in a swing a stiff one towards
                // the lash line near the hand and a loose one at the tip, which
                // is what makes the tip trail.
                let mut target = Rope::rest_point(anchor, face, right, rest_len, t, carry);
                let mut rate = CARRY_PULL * (1.0 - CARRY_TIP_LOOSE * t);
                if let Some(s) = swing {
                    let line = anchor + s.dir * (s.reach * rs * t);
                    target = target.lerp(line, s.weight);
                    rate += (34.0 - 25.0 * t - rate) * s.weight;
                }
                self.pos[k] += (target - self.pos[k]) * (1.0 - (-rate * h).exp());
            }
            // Keep the segments from stretching. Slack is allowed (the rope can
            // bunch up), stretch is not.
            let seg = len / n1;
            for _ in 0..6 {
                for k in 0..LASH_NODES - 1 {
                    let d = self.pos[k + 1] - self.pos[k];
                    let dist = d.length();
                    if dist > seg && dist > 1.0e-5 {
                        let corr = d * ((dist - seg) / dist);
                        if k == 0 {
                            self.pos[1] -= corr;
                        } else {
                            self.pos[k] += corr * 0.5;
                            self.pos[k + 1] -= corr * 0.5;
                        }
                    }
                }
            }
        }
        // The trail: a few older shapes of the rope while a swing is live.
        for a in &mut self.trail_age {
            *a += dt;
        }
        self.trail_clock += dt;
        if w > 0.2 && self.trail_clock >= TRAIL_EVERY {
            self.trail_clock = 0.0;
            self.trail.rotate_right(1);
            self.trail_age.rotate_right(1);
            for (j, &k) in TRAIL_NODES.iter().enumerate() {
                self.trail[0][j] = self.pos[k];
            }
            self.trail_age[0] = 0.0;
        }
    }
}

/// How far into its swing the lash is, 0..1, or `None` when the worm is just
/// carrying it.
fn swing_phase(anim: &Anim) -> Option<f32> {
    (anim.whip < WHIP_SWING_TIME).then(|| anim.whip / WHIP_SWING_TIME)
}

/// The swing as the body feels it. `cock` is how far the lash is drawn back
/// (up over the shoulder, early in the swing); `strike` how far the arm is
/// thrown into it; `yaw` where across the front the lash is, -1..1 with
/// positive on the worm's right; `on` is 1 through the swing and 0 at rest.
#[derive(Clone, Copy)]
struct Swung {
    cock: f32,
    strike: f32,
    yaw: f32,
    on: f32,
    /// The landing after the blow: 0 until the lash has crossed, a bump near the
    /// end of the swing where the body settles. It is the squash that sells the
    /// weight of the strike (Disney's follow-through).
    follow: f32,
}

impl Swung {
    const REST: Swung = Swung { cock: 0.0, strike: 0.0, yaw: 0.0, on: 0.0, follow: 0.0 };
}

/// How far the whip hand runs across the body with the swing, in metres. The
/// hand goes where the lash goes: out to the lash's side at the start of a
/// sweep, across the front at the end. Flip the sign if it ever looks backwards.
const WHIP_ARM_THROW: f32 = 0.85;
/// How far the free arm counter-swings, opposite the whip hand, in metres.
const WHIP_FREE_ARM_THROW: f32 = 0.45;
/// How far the whole body lunges into the blow, in metres of world space.
const WHIP_LUNGE: f32 = 0.35;

fn swung(anim: &Anim) -> Swung {
    let Some(p) = swing_phase(anim) else {
        return Swung::REST;
    };
    let cock = smooth(p / 0.12) * (1.0 - smooth((p - 0.12) / 0.20));
    let strike = smooth((p - 0.10) / 0.30) * (1.0 - smooth((p - 0.65) / 0.35));
    let yaw = match anim.whip_style {
        WhipStyle::Sweep => (if anim.whip_flip { -1.0 } else { 1.0 }) * (1.0 - 2.0 * whip_arc(p)),
        WhipStyle::Smash => 0.0,
    };
    // Ramps in and out at the ends of the swing, so a chain flows through the
    // rest pose without the body snapping.
    let on = smooth(p / 0.10) * (1.0 - smooth((p - 0.90) / 0.10));
    let follow = smooth((p - 0.55) / 0.20) * (1.0 - smooth((p - 0.80) / 0.20));
    Swung { cock, strike, yaw, on, follow }
}

// --- weapons --------------------------------------------------------------
//
// Only the viewer knows what a weapon looks like. Each one is an oversized,
// distinct silhouette built from the same three analytic primitives, so a piece
// costs one cheap closed-form raycast and needs no new shader code.
const GUNMETAL: Vec3 = Vec3::new(0.33, 0.35, 0.44);
const DARKMETAL: Vec3 = Vec3::new(0.17, 0.18, 0.24);
const STEEL: Vec3 = Vec3::new(0.52, 0.56, 0.66);
const OLIVE: Vec3 = Vec3::new(0.38, 0.45, 0.25);
const RUST: Vec3 = Vec3::new(0.56, 0.27, 0.16);
const BRASS: Vec3 = Vec3::new(0.80, 0.62, 0.26);
const HIVE: Vec3 = Vec3::new(0.30, 0.18, 0.34);
const ORANGE: Vec3 = Vec3::new(0.88, 0.50, 0.16);
const BOMBSHELL: Vec3 = Vec3::new(0.16, 0.15, 0.17);
const CYAN: Vec3 = Vec3::new(0.35, 0.92, 1.0);
const LIME: Vec3 = Vec3::new(0.55, 0.95, 0.30);
const ICE: Vec3 = Vec3::new(0.80, 0.92, 1.0);
const MAGENTA: Vec3 = Vec3::new(0.90, 0.35, 0.85);
const VIOLET: Vec3 = Vec3::new(0.55, 0.42, 1.0);
const EMBER: Vec3 = Vec3::new(1.0, 0.55, 0.16);
const TOXIC: Vec3 = Vec3::new(0.72, 1.0, 0.22);
const HOTSPOT: Vec3 = Vec3::new(1.0, 0.98, 0.85);
/// The plasma whip's energy: hot violet, close to the magenta of the Swarm
/// without being the same weapon's colour.
const PLASMA: Vec3 = Vec3::new(0.36, 0.13, 1.0);
/// The white-hot core of the lash, used sparingly: a whip made entirely of it
/// reads as a flat ribbon.
const PLASMA_TIP: Vec3 = Vec3::new(0.88, 0.78, 1.0);
/// The near-black knots that hang off the ribbon's edge, like the reference's
/// electric speckles. They are deliberately darker than any lit rock.
const LASH_DARK: Vec3 = Vec3::new(0.055, 0.018, 0.13);

/// A local frame for laying out a gun. `t` runs along the barrel from the hand,
/// `x` is sideways and `y` is up; every length is in metres before `s`, so a
/// whole silhouette is tuned by editing literals.
#[derive(Clone, Copy)]
struct Gun {
    hand: Vec3,
    fwd: Vec3,
    side: Vec3,
    up: Vec3,
    s: f32,
    back: Vec3,
}

impl Gun {
    fn new(hand: Vec3, fwd: Vec3, back: f32, rise: f32, scale: f32) -> Gun {
        // A zero aim would collapse every offset, so fall back to straight ahead.
        let fwd = if fwd.length_squared() > 1.0e-8 { fwd.normalize() } else { Vec3::Z };
        let mut side = fwd.cross(Vec3::Y);
        if side.length_squared() < 1.0e-6 {
            side = Vec3::X; // aiming straight up: any perpendicular will do
        }
        let side = side.normalize();
        let up = side.cross(fwd).normalize();
        // Muzzle rise: the whole gun pivots about the hand, so the barrel tips
        // up while the stock comes back. Without it the gun only slides, which
        // reads as a prop on rails.
        let (s, c) = rise.sin_cos();
        let (fwd, up) = (fwd * c + up * s, up * c - fwd * s);
        Gun { hand, fwd, side, up, s: scale, back: -fwd * back }
    }

    fn at(&self, t: f32, x: f32, y: f32) -> Vec3 {
        self.hand + (self.fwd * t + self.side * x + self.up * y) * self.s + self.back
    }

    /// A tube along the barrel: barrels, coils, prongs, fins.
    fn tube(&self, a: (f32, f32, f32), b: (f32, f32, f32), r: f32, col: Vec3) -> DrawPrim {
        DrawPrim::cylinder(
            self.at(a.0, a.1, a.2),
            self.at(b.0, b.1, b.2),
            (r * self.s).max(0.004),
            col,
            false,
        )
    }

    /// A hard-edged box: receivers, grips, sights, fins.
    fn plate(&self, c: (f32, f32, f32), h: (f32, f32, f32), col: Vec3) -> DrawPrim {
        DrawPrim::cuboid(
            self.at(c.0, c.1, c.2),
            Vec3::new(h.0.max(0.002), h.1.max(0.002), h.2.max(0.002)) * self.s,
            self.fwd,
            col,
            false,
        )
    }

    /// A sphere: warheads, bombs, eyes of a weapon.
    fn ball(&self, c: (f32, f32, f32), r: f32, col: Vec3) -> DrawPrim {
        let p = self.at(c.0, c.1, c.2);
        DrawPrim::capsule(p, p + self.up * (0.004 * self.s), (r * self.s).max(0.004), col, false)
    }

    /// A very short fat tube: a collar, coil or muzzle ring. Cheaper than a hoop.
    fn flange(&self, t: f32, r: f32, thick: f32, col: Vec3) -> DrawPrim {
        self.tube((t - thick * 0.5, 0.0, 0.0), (t + thick * 0.5, 0.0, 0.0), r, col)
    }

    /// An open hoop across the barrel, from `n` tangential tubes (an n-gon with
    /// overlapping joints). Reads as a portal ring or a beam emitter mouth.
    fn hoop(&self, t: f32, radius: f32, r: f32, n: usize, col: Vec3, out: &mut Vec<DrawPrim>) {
        let step = TAU / n as f32;
        for k in 0..n {
            let a0 = k as f32 * step;
            let a1 = a0 + step * 1.12; // overlap, so no gap shows at a joint
            out.push(self.tube(
                (t, radius * a0.cos(), radius * a0.sin()),
                (t, radius * a1.cos(), radius * a1.sin()),
                r,
                col,
            ));
        }
    }

    /// A cone from `t0` to `t1`, `r0` to `r1`, from stacked straight tubes: the
    /// raycast primitive has no cone, and a few steps read as one from any angle.
    fn cone(&self, t0: f32, t1: f32, r0: f32, r1: f32, n: usize, col: Vec3, out: &mut Vec<DrawPrim>) {
        for k in 0..n {
            let u0 = k as f32 / n as f32;
            let u1 = (k + 1) as f32 / n as f32;
            let mid = (u0 + u1) * 0.5;
            let r = (r0 + (r1 - r0) * mid).max(0.004);
            out.push(self.tube(
                (t0 + (t1 - t0) * u0, 0.0, 0.0),
                (t0 + (t1 - t0) * u1, 0.0, 0.0),
                r,
                col,
            ));
        }
    }
}

/// How hard each weapon kicks: metres of travel back through the hand and
/// radians of muzzle climb at the instant of the shot. Heavy weapons kick slow
/// and far; the blaster barely moves, which is what makes the big ones feel big.
fn recoil_style(id: u8) -> (f32, f32) {
    match id {
        NOVA => (0.20, 0.20),
        ROCKET => (0.16, 0.16),
        RAILGUN => (0.14, 0.11),
        CLUSTER => (0.11, 0.09),
        SWARM => (0.09, 0.06),
        BOUNCER => (0.07, 0.06),
        TELEPORT => (0.07, 0.06),
        // The lash is not a gun: the whole strike is the animation, and a
        // second recoil on the handle fights it.
        WHIP => (0.0, 0.0),
        BLASTER => (0.06, 0.05),
        BOMB => (0.05, 0.04),
        BUILD | TRACTOR => (0.035, 0.02),
        _ => (0.05, 0.04),
    }
}

/// The recoil envelope: 1 at the shot, then a damped bounce back through rest
/// and gone in half a second. A plain linear decay reads as the gun being
/// pulled away; a spring reads as a kick.
fn recoil_curve(age: f32) -> f32 {
    if age >= 0.50 {
        return 0.0;
    }
    (-age * 9.0).exp() * (age * 20.0).cos()
}

/// The lash the weapon draws: the worm's rope, how hot it glows, and how far the
/// arm has drawn it back.
#[derive(Clone, Copy)]
struct Lash<'a> {
    rope: &'a Rope,
    /// 0..1 glow: dim while carried, bright mid-swing.
    heat: f32,
}

/// Smoothstep, for poses and for the lash's travel. Linear motion over a few
/// frames reads as a slide; a smooth one reads as a swing.
fn smooth(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// One weapon: an oversized, distinct silhouette made of the same primitives
/// as the body. `kick_back` and `kick_rise` are the current recoil (metres and
/// radians); they do not affect where the sim starts the shot, only how the
/// gun is drawn at this instant.
fn weapon(
    id: u8,
    hand: Vec3,
    fwd: Vec3,
    kick_back: f32,
    kick_rise: f32,
    rs: f32,
    colour: Vec3,
    clock: f32,
    lash: Lash,
    glow: &mut Vec<Sprite>,
    out: &mut Vec<DrawPrim>,
) {
    // A mortar is carried pointing up: the same loft the sim uses to launch its
    // shell, so the barrel and the arc agree.
    let fwd = WEAPONS[id as usize].launch_dir(fwd);
    // A touch larger than the worm's scale. R&C guns are chunky; a weapon that
    // is small next to the character reads as a toy.
    let g = Gun::new(hand, fwd, kick_back, kick_rise, rs * 1.25);
    // The worm's own colour survives as a small stripe, so you can still tell
    // whose gun is whose.
    let trim = colour;

    match id {
        BLASTER => {
            // A chunky ray pistol: receiver, glowing energy cell underneath,
            // collared barrel, bright muzzle.
            out.push(g.plate((0.00, 0.0, -0.16), (0.05, 0.07, 0.12), DARKMETAL));
            out.push(g.plate((0.20, 0.0, 0.03), (0.09, 0.10, 0.22), GUNMETAL));
            out.push(g.plate((0.18, 0.10, 0.03), (0.05, 0.012, 0.16), trim));
            out.push(g.plate((0.28, 0.0, 0.15), (0.055, 0.035, 0.09), DARKMETAL));
            out.push(g.tube((0.02, 0.0, -0.15), (0.26, 0.0, -0.15), 0.055, CYAN).glowing());
            out.push(g.tube((0.32, 0.0, 0.0), (0.86, 0.0, 0.0), 0.055, DARKMETAL));
            out.push(g.flange(0.46, 0.088, 0.05, GUNMETAL));
            out.push(g.flange(0.64, 0.085, 0.045, GUNMETAL));
            out.push(g.flange(0.84, 0.09, 0.03, GUNMETAL));
            out.push(g.tube((0.86, 0.0, 0.0), (0.94, 0.0, 0.0), 0.08, CYAN).glowing());
            out.push(g.ball((0.93, 0.0, 0.0), 0.05, HOTSPOT).glowing());
        }
        ROCKET => {
            // A bazooka: a fat tube with a blast flare behind, four fins and a
            // loaded rocket poking out of the muzzle.
            g.cone(-0.20, 0.04, 0.24, 0.19, 2, GUNMETAL, out);
            // The exhaust glow lives *outside* the flare: an ember ring around
            // its mouth and a short jet trailing out of it. Drawing it inside
            // the bell (or behind the worm's arm) hid most of it, which read as
            // a broken sprite rather than a hot weapon.
            for k in 0..10 {
                let a0 = k as f32 * TAU / 10.0;
                let a1 = a0 + TAU / 10.0 * 1.12; // overlap: no gap at a joint
                out.push(
                    g.tube(
                        (-0.21, 0.255 * a0.cos(), 0.255 * a0.sin()),
                        (-0.21, 0.255 * a1.cos(), 0.255 * a1.sin()),
                        0.028,
                        EMBER,
                    )
                    .glowing(),
                );
            }
            for k in 0..3 {
                let t0 = -0.24 - 0.11 * k as f32;
                let t1 = t0 - 0.12;
                let r = 0.115 - 0.028 * k as f32;
                out.push(g.tube((t0, 0.0, 0.0), (t1, 0.0, 0.0), r, EMBER).glowing());
            }
            // The exhaust ring and jet are self-lit, so bloom finds them; no
            // extra billboard is needed (it only painted a glow onto the worm's
            // back, which read as a clipping bug).
            // Charged bands and a hot vent strip on *top* of the tube. The
            // shoulder camera looks down at the weapon, and the worm's arm
            // covers the exhaust, so the glow has to live on the visible side
            // as well, or most of it disappears behind the body.
            out.push(g.flange(0.32, 0.205, 0.045, EMBER).glowing());
            out.push(g.flange(0.74, 0.205, 0.045, EMBER).glowing());
            out.push(g.plate((0.53, 0.0, 0.198), (0.22, 0.018, 0.014), EMBER).glowing());
            out.push(g.tube((0.02, 0.0, 0.0), (1.02, 0.0, 0.0), 0.19, OLIVE));
            out.push(g.flange(1.00, 0.215, 0.06, DARKMETAL));
            g.cone(1.06, 1.44, 0.14, 0.02, 3, EMBER, out);
            out.push(g.ball((1.46, 0.0, 0.0), 0.06, HOTSPOT).glowing());
            out.push(g.plate((0.92, 0.21, 0.0), (0.03, 0.13, 0.16), DARKMETAL));
            out.push(g.plate((0.92, -0.21, 0.0), (0.03, 0.13, 0.16), DARKMETAL));
            out.push(g.plate((0.92, 0.0, 0.21), (0.13, 0.03, 0.16), DARKMETAL));
            out.push(g.plate((0.92, 0.0, -0.21), (0.13, 0.03, 0.16), DARKMETAL));
            out.push(g.plate((0.05, 0.0, -0.24), (0.06, 0.08, 0.13), DARKMETAL));
            out.push(g.plate((0.55, 0.0, -0.25), (0.05, 0.07, 0.11), GUNMETAL));
            out.push(g.plate((0.35, 0.0, 0.20), (0.09, 0.03, 0.06), trim));
        }
        RAILGUN => {
            // A very long twin-rail rifle with glowing coils and a backpack.
            out.push(g.plate((0.18, 0.0, 0.02), (0.075, 0.09, 0.26), GUNMETAL));
            out.push(g.plate((0.42, 0.0, 0.16), (0.035, 0.045, 0.16), DARKMETAL));
            out.push(g.ball((0.58, 0.0, 0.16), 0.035, VIOLET).glowing());
            out.push(g.tube((0.30, 0.06, 0.0), (1.55, 0.06, 0.0), 0.022, STEEL));
            out.push(g.tube((0.30, -0.06, 0.0), (1.55, -0.06, 0.0), 0.022, STEEL));
            out.push(g.flange(0.55, 0.10, 0.05, VIOLET).glowing());
            out.push(g.flange(0.85, 0.10, 0.05, VIOLET).glowing());
            out.push(g.flange(1.15, 0.10, 0.05, VIOLET).glowing());
            out.push(g.tube((1.55, 0.06, 0.0), (1.80, 0.06, 0.0), 0.03, VIOLET).glowing());
            out.push(g.tube((1.55, -0.06, 0.0), (1.80, -0.06, 0.0), 0.03, VIOLET).glowing());
            out.push(g.plate((-0.18, 0.0, 0.0), (0.12, 0.16, 0.18), DARKMETAL));
            out.push(g.ball((-0.18, 0.0, 0.10), 0.05, VIOLET).glowing());
            out.push(g.plate((0.02, 0.0, -0.20), (0.05, 0.08, 0.12), DARKMETAL));
            out.push(g.plate((0.18, 0.0, 0.11), (0.06, 0.015, 0.14), trim));
        }
        BOUNCER => {
            // A spring cannon: a coil of fat rings and a bright ball in a cradle.
            out.push(g.tube((0.02, 0.0, 0.0), (0.42, 0.0, 0.0), 0.17, RUST));
            for k in 0..5 {
                out.push(g.flange(0.48 + k as f32 * 0.09, 0.155, 0.035, LIME).glowing());
            }
            out.push(g.tube((0.90, 0.13, 0.0), (1.02, 0.13, 0.0), 0.035, DARKMETAL));
            out.push(g.tube((0.90, -0.13, 0.0), (1.02, -0.13, 0.0), 0.035, DARKMETAL));
            out.push(g.ball((1.16, 0.0, 0.0), 0.20, LIME));
            out.push(g.plate((0.04, 0.0, -0.22), (0.05, 0.08, 0.12), DARKMETAL));
            out.push(g.plate((0.20, 0.0, 0.17), (0.09, 0.02, 0.07), trim));
        }
        BOMB => {
            // A landmine held out flat on an arm, like a tray: the squat disc,
            // its dome, a glowing trigger light and studs round the rim, the
            // same parts as the one that lies on the ground.
            out.push(g.plate((0.08, 0.0, 0.02), (0.07, 0.10, 0.15), RUST));
            // (Gun coordinates are forward, side, up.)
            out.push(g.tube((0.24, 0.0, -0.02), (0.56, 0.0, -0.10), 0.04, DARKMETAL));
            out.push(g.tube((0.74, 0.0, -0.14), (0.74, 0.0, 0.0), 0.31, BOMBSHELL));
            out.push(g.tube((0.74, 0.0, -0.08), (0.74, 0.0, -0.03), 0.325, OLIVE));
            out.push(g.tube((0.74, 0.0, 0.0), (0.74, 0.0, 0.06), 0.19, OLIVE));
            out.push(g.ball((0.74, 0.0, 0.08), 0.055, EMBER).glowing());
            for k in 0..4 {
                let a = k as f32 * TAU / 4.0 + TAU / 8.0;
                out.push(g.ball((0.74 + a.cos() * 0.24, a.sin() * 0.24, 0.01), 0.04, BOMBSHELL));
            }
            out.push(g.plate((0.02, 0.0, -0.20), (0.05, 0.08, 0.12), DARKMETAL));
            out.push(g.plate((0.10, 0.09, 0.02), (0.03, 0.012, 0.10), trim));
        }
        CLUSTER => {
            // A gatling drum: six fat barrels around a hot hub.
            out.push(g.tube((0.02, 0.0, 0.0), (0.44, 0.0, 0.0), 0.25, DARKMETAL));
            out.push(g.flange(-0.02, 0.21, 0.05, EMBER).glowing());
            out.push(g.flange(0.44, 0.26, 0.05, GUNMETAL));
            for k in 0..6 {
                let a = k as f32 * TAU / 6.0;
                let (x, y) = (a.cos() * 0.155, a.sin() * 0.155);
                out.push(g.tube((0.44, x, y), (1.00, x, y), 0.055, BRASS));
            }
            out.push(g.ball((0.46, 0.0, 0.0), 0.09, EMBER).glowing());
            out.push(g.plate((0.25, 0.0, 0.29), (0.06, 0.04, 0.16), GUNMETAL));
            out.push(g.plate((0.02, 0.0, -0.29), (0.05, 0.09, 0.12), DARKMETAL));
            out.push(g.plate((0.22, 0.0, -0.26), (0.10, 0.02, 0.10), trim));
        }
        SWARM => {
            // A hive pod: seven launch tubes in a hexagonal cluster.
            out.push(g.plate((0.32, 0.0, 0.0), (0.26, 0.26, 0.32), HIVE));
            for k in 0..6 {
                let a = k as f32 * TAU / 6.0;
                let (x, y) = (a.cos() * 0.15, a.sin() * 0.15);
                out.push(g.tube((0.64, x, y), (0.94, x, y), 0.05, MAGENTA));
            }
            out.push(g.tube((0.64, 0.0, 0.0), (0.94, 0.0, 0.0), 0.05, MAGENTA));
            out.push(g.ball((0.95, 0.0, 0.0), 0.06, MAGENTA).glowing());
            out.push(g.tube((0.30, 0.0, 0.30), (0.24, 0.0, 0.46), 0.015, DARKMETAL));
            out.push(g.ball((0.23, 0.0, 0.47), 0.035, MAGENTA).glowing());
            out.push(g.plate((0.04, 0.0, -0.28), (0.05, 0.09, 0.12), DARKMETAL));
            out.push(g.plate((0.32, 0.265, 0.0), (0.012, 0.12, 0.16), trim));
        }
        TELEPORT => {
            // A portal caster: a big open hoop with a hot core on a stubby pistol.
            out.push(g.plate((0.10, 0.0, 0.0), (0.07, 0.10, 0.14), GUNMETAL));
            out.push(g.tube((0.22, 0.0, 0.0), (0.40, 0.0, 0.0), 0.055, DARKMETAL));
            g.hoop(0.58, 0.40, 0.062, 8, VIOLET, out);
            out.push(g.tube((0.58, 0.0, 0.0), (0.58, 0.34, 0.18), 0.032, DARKMETAL));
            out.push(g.tube((0.58, 0.0, 0.0), (0.58, -0.34, -0.18), 0.032, DARKMETAL));
            out.push(g.ball((0.58, 0.0, 0.0), 0.15, HOTSPOT).glowing());
            out.push(g.plate((0.06, 0.0, -0.20), (0.05, 0.08, 0.12), DARKMETAL));
        }
        NOVA => {
            // A warhead launcher: the biggest silhouette in the game, a 0.9 m
            // bomb with a glowing equator and four fins.
            out.push(g.tube((0.00, 0.0, 0.0), (0.38, 0.0, 0.0), 0.14, GUNMETAL));
            out.push(g.flange(0.36, 0.18, 0.06, DARKMETAL));
            out.push(g.ball((0.72, 0.0, 0.0), 0.44, ICE));
            out.push(g.flange(0.72, 0.50, 0.035, CYAN).glowing());
            g.cone(1.08, 1.38, 0.17, 0.02, 3, ICE, out);
            out.push(g.plate((0.16, 0.26, 0.0), (0.03, 0.14, 0.13), DARKMETAL));
            out.push(g.plate((0.16, -0.26, 0.0), (0.03, 0.14, 0.13), DARKMETAL));
            out.push(g.plate((0.16, 0.0, 0.26), (0.14, 0.03, 0.13), DARKMETAL));
            out.push(g.plate((0.16, 0.0, -0.26), (0.14, 0.03, 0.13), DARKMETAL));
            out.push(g.plate((0.02, 0.0, -0.22), (0.05, 0.09, 0.12), DARKMETAL));
            out.push(g.plate((0.16, 0.0, 0.17), (0.08, 0.02, 0.06), trim));
        }
        BUILD => {
            // A constructor: an orange body, a canister, and an open glowing
            // emitter ring with a hot core, so a block is clearly going there.
            out.push(g.plate((0.18, 0.0, 0.02), (0.10, 0.11, 0.22), ORANGE));
            out.push(g.tube((0.02, 0.0, 0.21), (0.40, 0.0, 0.21), 0.075, TOXIC));
            g.hoop(0.50, 0.20, 0.035, 6, TOXIC, out);
            out.push(g.ball((0.50, 0.0, 0.0), 0.08, HOTSPOT).glowing());
            out.push(g.tube((0.46, 0.19, 0.0), (0.62, 0.23, 0.0), 0.022, TOXIC).glowing());
            out.push(g.tube((0.46, -0.19, 0.0), (0.62, -0.23, 0.0), 0.022, TOXIC).glowing());
            out.push(g.plate((0.02, 0.0, -0.20), (0.05, 0.08, 0.12), DARKMETAL));
            out.push(g.plate((0.16, 0.11, 0.02), (0.012, 0.06, 0.12), trim));
        }
        TRACTOR => {
            // A beam projector: a flared cone ending in a glowing hoop.
            out.push(g.plate((0.14, 0.0, 0.02), (0.09, 0.10, 0.18), RUST));
            g.cone(0.30, 0.80, 0.07, 0.34, 3, DARKMETAL, out);
            g.hoop(0.80, 0.34, 0.03, 6, TOXIC, out);
            out.push(g.ball((0.60, 0.0, 0.0), 0.12, TOXIC).glowing());
            out.push(g.plate((0.02, 0.0, -0.20), (0.05, 0.08, 0.12), DARKMETAL));
            out.push(g.plate((0.14, 0.0, 0.11), (0.05, 0.012, 0.10), trim));
        }
        WHIP => {
            // A plasma lash: a stubby handle, a bright core and two prongs.
            out.push(g.plate((0.12, 0.0, 0.02), (0.06, 0.09, 0.15), DARKMETAL));
            out.push(g.tube((0.02, 0.0, 0.0), (0.32, 0.0, 0.0), 0.06, GUNMETAL));
            out.push(g.flange(0.32, 0.095, 0.04, PLASMA).glowing());
            out.push(g.tube((0.32, 0.10, 0.0), (0.46, 0.16, 0.0), 0.018, PLASMA).glowing());
            out.push(g.tube((0.32, -0.10, 0.0), (0.46, -0.16, 0.0), 0.018, PLASMA).glowing());
            out.push(g.ball((0.38, 0.0, 0.0), 0.10, HOTSPOT).glowing());
            out.push(g.plate((0.04, 0.0, -0.19), (0.05, 0.08, 0.12), DARKMETAL));
            out.push(g.plate((0.16, 0.10, 0.02), (0.012, 0.05, 0.10), trim));
            // The lash is carried, not spawned: the same rope hangs from the
            // hand at rest and is thrown out by a swing. See `lash_draw`.
            lash_draw(&g, lash, rs, clock, glow, out);
        }
        _ => {
            // BOMBLET is never held; unknown ids draw nothing.
        }
    }
}

/// The plasma lash: one continuous piece of energy, carried by the worm.
///
/// It is never spawned by a shot and it is never put away. It is the worm's
/// `Rope`: at rest it hangs from the hand and sways with the body, and a swing
/// pulls it round the front so the tip trails and cracks. The picture is a chain
/// of tapering glowing capsules with a soft halo, plus a trail of faint glow
/// from the last few moments of the swing.
fn lash_draw(g: &Gun, l: Lash, rs: f32, clock: f32, glow: &mut Vec<Sprite>, out: &mut Vec<DrawPrim>) {
    let path = &l.rope.pos;
    let heat = l.heat;

    // The trail: older shapes of the rope, fainter the older they are. Sprites,
    // not capsules, so a busy frame keeps its budget.
    for (snap, &age) in l.rope.trail.iter().zip(&l.rope.trail_age) {
        let fade = 1.0 - age / TRAIL_LIFE;
        if fade <= 0.0 {
            continue;
        }
        for (j, &p) in snap.iter().enumerate() {
            glow.push(Sprite::ball(
                p,
                (0.15 - 0.012 * j as f32).max(0.05) * rs,
                PLASMA.lerp(PLASMA_TIP, 0.25),
                0.22 * fade * fade,
                p.x * 1.3 + p.z * 0.7,
                0.22,
            ));
        }
    }

    for k in 0..LASH_NODES - 1 {
        let t = k as f32 / (LASH_NODES - 1) as f32;
        // A travelling pulse of brighter segments, like current running down
        // the lash, so it looks alive even held still.
        let pulse = (clock * 7.0 - t * 9.0).sin() * 0.5 + 0.5;
        let base = if k % 3 == 0 { PLASMA } else { lighten(PLASMA, 1.15) };
        // The body stays blue-violet even at the crack; only a faint travelling
        // pulse lightens it. A fully white lash loses its silhouette.
        let whiten = (0.02 + 0.08 * heat + 0.03 * pulse).clamp(0.0, 0.13);
        // The last fifth goes dark, so the lash ends in a thin electric point
        // instead of a pale bulb. Any bright terminal pixel reads as a sword
        // tip, which is exactly what this weapon is not.
        let tip_ink = smooth((t - 0.78) / 0.22);
        let col = base.lerp(PLASMA_TIP, whiten).lerp(LASH_DARK, tip_ink * 0.80);
        // Taper all the way to a point. There is deliberately no bright tip
        // ball: the lash ends in a thin, dark-edged flick.
        let r = (0.014 + 0.105 * (1.0 - t).powf(0.65)) * rs * (1.0 + 0.25 * heat);
        // The ink outline on a glowing capsule is the dark jagged edge of the
        // ribbon. The halo below softens it into plasma instead of a black rim.
        out.push(DrawPrim::capsule(path[k], path[k + 1], r, col, false).glowing());
        // Dark electric knots on the outside of the ribbon, alternating sides.
        // They sit clear of the core or the glow swallows them.
        if k >= 2 && k % 2 == 0 {
            let side = (k as f32 * 2.399).sin();
            let knot = path[k] + g.up * (side * r * 1.75);
            out.push(
                DrawPrim::capsule(knot, knot + (path[k + 1] - path[k]) * 0.16, r * 0.26, LASH_DARK, false)
                    .inkless(),
            );
        }
        // Soft plasma around the core. A halo on every segment keeps the lash a
        // continuous ribbon instead of a string of beads. The last few segments
        // get none: they are the thin dark flick at the end.
        if k + 3 < LASH_NODES {
            let p = path[k];
            let halo = (r * 2.4).max(0.10);
            glow.push(Sprite::ball(
                p,
                halo,
                PLASMA.lerp(PLASMA_TIP, 0.04 + 0.12 * heat),
                0.09 + 0.24 * heat,
                p.x * 1.7 + p.z * 0.9,
                halo * 0.9,
            ));
        }
    }
}

/// The capsules for `worms`, each drawn at `positions[i]` (the sim's position
/// smoothed between two ticks, see `Viewer::frame`), plus the glow sprites the
/// plasma whip emits. Most rigs produce no sprites; the whip is the exception.
pub fn draw(worms: &[Worm], positions: &[Vec3], anims: &mut [Anim], dt: f32) -> (Vec<DrawPrim>, Vec<Sprite>, Vec<WormSdf>) {
    let mut out = Vec::new();
    let mut glow = Vec::new();
    let mut sdfs = Vec::new();
    for (i, (worm, &pos)) in worms.iter().zip(positions).enumerate() {
        if !worm.alive {
            continue; // (the death flash is an effect, see `effects.rs`)
        }
        let mut anim = anims.get(i).copied().unwrap_or_default();
        // A hit flashes the body white for a blink, scaled by how hard it was:
        // a blaster tick is a flicker, a Nova is a small sun. This is the
        // cheapest "it connected" signal there is.
        let flash = (1.0 - anim.hurt / HURT_FLASH).clamp(0.0, 1.0)
            * (0.35 + 0.65 * (anim.hurt_damage / 30.0).clamp(0.0, 1.0));
        let colour = COLOURS[i % COLOURS.len()].lerp(Vec3::new(1.0, 0.90, 0.90), flash);
        let flinch = (1.0 - anim.hurt / HURT_FLINCH).clamp(0.0, 1.0)
            * (anim.hurt_damage / 25.0).clamp(0.15, 1.0);
        let rs = WORM_HEIGHT / 1.8;
        let caps = |a: Vec3, b: Vec3, radius: f32, colour: Vec3, shadow: bool| {
            DrawPrim::capsule(a, b, radius, colour, shadow)
        };

        let (face, right) = basis(worm.face_yaw);
        let aim = worm.aim_dir();
        // The aim expressed in the body's own frame, so the eyes and gun turn
        // with the body while still following the mouse.
        let look = Vec3::new(aim.dot(right), aim.dot(Vec3::Y), aim.dot(face)).normalize_or_zero();

        let speed = worm.forward_vel.abs();
        let run = (speed / 11.0).clamp(0.0, 1.0);
        let ground = worm.on_ground;
        let air = !ground;
        let action = worm.action;
        let climb = action == Action::Climb;
        let rope = action == Action::Rope;
        let tumble = action == Action::Tumble;
        let dig = worm.digging;

        // Recoil: how hard this gun kicks, and how far into the kick we are.
        // The clock lives in `Anim`, so it is the same curve the face uses.
        let gun_id = worm.weapons[worm.current].id;
        let (kick_back, kick_rise) = recoil_style(gun_id);
        let kick = recoil_curve(anim.fire);
        // The plasma whip is the one weapon whose whole body is the animation:
        // the arm draws back, then throws the shoulder through the swing, and
        // the torso follows the lash across the front. All of it is a function
        // of the swing's age, so it shows in a screenshot too.
        let sw = if gun_id == WHIP { swung(&anim) } else { Swung::REST };
        let (whip_cock, whip_strike) = (sw.cock, sw.strike);
        // Wielding the whip is a stance of its own: weight low, shoulders turned
        // slightly, free arm out, before and between swings.
        let whip_ready = if gun_id == WHIP {
            (1.0 - whip_cock.max(whip_strike)).clamp(0.0, 1.0)
        } else {
            0.0
        };

        let mut pose = Pose::default();
        pose.lean = match action {
            Action::Climb => 0.45,
            Action::Rope => -0.08,
            Action::DoubleJump | Action::Tumble => 0.0,
            _ => 0.03 + 0.14 * run,
        };
        if dig {
            // Hunch over the hole and shake with the drill.
            pose.lean = 0.28;
            pose.stretch = 0.96;
            pose.wide = 1.04;
            pose.roll = (anim.clock * 26.0).sin() * 0.03;
        }
        // A landing squashes once. `landed` is only set on the touching-down
        // update, so starting to walk does not squash.
        if anim.landed && anim.since < 0.14 {
            let k = 1.0 - anim.since / 0.14;
            pose.stretch = 1.0 - 0.12 * k;
            pose.wide = 1.0 + 0.10 * k;
            pose.deep = 1.0 + 0.06 * k;
        }
        // A jump starts from a crouch: a beat of anticipation before the spring,
        // which sells the launch far better than an instant stretch.
        if action == Action::Jump && anim.since < 0.06 {
            let k = 1.0 - anim.since / 0.06;
            pose.stretch = 1.0 - 0.09 * k;
            pose.wide = 1.0 + 0.06 * k;
        }
        // Rising stretches the worm; falling lets it compress a little.
        if air && !tumble && !rope && !climb {
            let s = (worm.vel.y / 22.0).clamp(-0.5, 0.5);
            pose.stretch = 1.0 + 0.18 * s;
            pose.wide = 1.0 - 0.05 * s;
            pose.deep = 1.0 - 0.05 * s;
        }
        // The double jump is one clean forward flip; a tumble is faster and
        // uncontrolled. Both are driven by the action's age, not a global clock.
        if action == Action::DoubleJump {
            pose.flip = TAU * (anim.since / 0.62).clamp(0.0, 1.0);
        } else if tumble {
            pose.flip = TAU * 1.5 * (anim.since / 0.5).clamp(0.0, 1.0);
        }
        if action == Action::WallKick {
            pose.roll = (anim.since * 18.0).sin() * 0.5;
        } else if rope {
            pose.roll = (anim.clock * 1.8).sin() * 0.10;
        }
        // A shot rocks the whole body back from the waist. It is small next to
        // the gun's own kick, but it is what stops the arm looking like a stick
        // waving a prop about.
        pose.lean -= kick_rise * 0.28 * kick;
        if gun_id == WHIP {
            let stance = whip_ready * if ground { 1.0 } else { 0.35 };
            // Ready stance: weight low, a little forward.
            pose.lean += 0.050 * stance;
            pose.wide *= 1.0 + 0.025 * stance;
            // Coil away from the target as the lash is cocked, then lean through
            // it as it comes round. The shoulder does the work, not the wrist.
            // The torso twists to follow the lash across the front (positive
            // twist is towards the worm's right, like `yaw`), so the swing runs
            // through the whole body and a held trigger alternates sides.
            pose.lean += -0.42 * whip_cock + 0.45 * whip_strike;
            pose.twist += 0.85 * sw.yaw * sw.on;
            pose.roll += -0.20 * sw.yaw * sw.on;
            // Squash and stretch, with the feet planted: crouch into the
            // wind-up, spring tall through the blow, and settle heavily as the
            // lash lands. Stretch alone would read as a hop; the crouch first is
            // what makes it a lunge.
            pose.stretch *= 1.0 - 0.10 * whip_cock + 0.06 * whip_strike - 0.08 * sw.follow;
            pose.wide *= 1.0 + 0.05 * whip_cock + 0.04 * whip_strike + 0.06 * sw.follow;
        }
        if flinch > 0.0 {
            // A hit knocks the body off its axis for a beat. The flash says
            // "connected"; the flinch says "hard". The shake decays fast so a
            // held trigger does not turn the worm into a vibrating prop.
            pose.lean -= 0.22 * flinch;
            pose.roll += (anim.hurt * 52.0).sin() * 0.11 * flinch;
            pose.twist += (anim.hurt * 41.0).cos() * 0.07 * flinch;
        }

        let rig = Rig::new(pos, worm.face_yaw, &pose, rs);

        // An undulating body: a travelling sideways wave, a small surge, a
        // breathing bob when idle, and a run bob on the ground.
        let amp = if ground { 0.035 + 0.05 * run } else { 0.02 };
        let sway = |t: f32| (anim.phase + t * 4.2).sin() * amp;
        let surge = |t: f32| (anim.phase * 0.5 + t * 2.4).sin() * 0.018 * run;
        let bob = if ground {
            0.018 * run * (0.5 + 0.5 * (anim.phase * 2.0).sin())
        } else {
            0.0
        };
        let breathe = if ground && run < 0.05 {
            0.008 * (0.5 + 0.5 * (anim.clock * 2.0).sin())
        } else {
            0.0
        };
        let lift = bob + breathe;

        // Body: four points, three overlapping capsules. Alternating shades
        // make the segmentation read even on the flat toon bands. A narrow body
        // and a big head is the Dr Nefarious silhouette: it makes the gun beside
        // it look enormous.
        //
        // The SDF body is the one that is drawn. These capsules stay as shadow
        // proxies (and for the tests that inspect the old rig); `proxy` makes the
        // shader skip them in the colour pass but still use them for sun shadows.
        let body_start = out.len();
        let body_y = [0.28, 0.76, 1.15, 1.50];
        let body_r = [0.30, 0.26, 0.23, 0.20];
        let mut prev = rig.at(sway(0.0), body_y[0] + lift, surge(0.0));
        for k in 1..body_y.len() {
            let t = k as f32 / (body_y.len() - 1) as f32;
            let p = rig.at(sway(t), body_y[k] + lift, surge(t));
            let r = 0.5 * (body_r[k - 1] + body_r[k]) * rs;
            let shade = if k % 2 == 0 { colour } else { darken(colour, 0.84) };
            out.push(caps(prev, p, r, shade, true));
            prev = p;
        }

        // Head: a big bulb on top of the neck, the character's largest feature.
        let head_x = sway(1.0) * 0.6;
        let head = Vec3::new(head_x, 1.58 + lift, 0.0);
        out.push(caps(
            rig.at(head.x, head.y - 0.02, 0.0),
            rig.at(head.x, head.y + 0.02, 0.0),
            0.37 * rs,
            lighten(colour, 1.07),
            true,
        ));

        // Nose: a small forward bump, so the head has a front and a muzzle
        // instead of being a blank sphere.
        let nose = head + look * 0.38 - Vec3::Y * 0.01;
        out.push(caps(
            rig.at(nose.x, nose.y, nose.z),
            rig.at(nose.x, nose.y + 0.01, nose.z),
            0.11 * rs,
            darken(colour, 0.80),
            false,
        ));

        // Belly stripe: a light band on the front. Besides being worm-like, it
        // shows which way the body faces when the mouse aims elsewhere.
        let belly = lighten(colour, 1.35);
        out.push(caps(
            rig.at(sway(0.18), 0.42 + lift, 0.36),
            rig.at(sway(0.72), 1.06 + lift, 0.29),
            0.10 * rs,
            belly,
            false,
        ));

        // Face: the brows lead the expression, the eyes follow the aim, the
        // mouth opens to shout or breathe. A firing worm frowns, a flying one
        // looks surprised. This is what keeps the character from reading as a
        // dead-eyed model.
        let frown = ((0.22 - anim.fire) / 0.22).clamp(0.0, 1.0);
        let surprise = if air && !tumble { 1.0 } else { 0.0 };
        let brow_angle = frown * 0.11 - surprise * 0.09;
        // A hurt worm squeezes its eyes shut; it also opens its mouth below.
        let squint = (1.0 - anim.hurt / 0.22).clamp(0.0, 1.0);
        let blink = (if (anim.clock + (i as f32) * 0.9) % 4.3 < 0.10 { 0.15 } else { 1.0 })
            * (1.0 - 0.70 * squint);
        for side in [-1.0, 1.0] {
            let inner = head + look * 0.20 + Vec3::X * (0.07 * side) + Vec3::Y * (0.20 + brow_angle);
            let outer = head + look * 0.24 + Vec3::X * (0.23 * side) + Vec3::Y * (0.15 - brow_angle);
            out.push(caps(
                rig.at(inner.x, inner.y, inner.z),
                rig.at(outer.x, outer.y, outer.z),
                0.025 * rs,
                darken(colour, 0.55),
                false,
            ));
        }
        for side in [-1.0, 1.0] {            let eye = head + look * 0.30 + Vec3::X * (0.16 * side) + Vec3::Y * 0.05;
            // White eyeball, a coloured iris and a dark pupil, each in front of
            // the last, so the eye reads as a real ball looking along the aim.
            out.push(caps(
                rig.at(eye.x - look.x * 0.03, eye.y - look.y * 0.03, eye.z - look.z * 0.03),
                rig.at(eye.x + look.x * 0.03, eye.y + look.y * 0.03, eye.z + look.z * 0.03),
                0.125 * rs * blink,
                EYE,
                false,
            ));
            let iris = eye + look * 0.08;
            out.push(caps(
                rig.at(iris.x, iris.y, iris.z),
                rig.at(iris.x, iris.y + 0.01, iris.z),
                0.072 * rs * blink,
                IRIS,
                false,
            ));
            let pupil = eye + look * 0.115;
            out.push(caps(
                rig.at(pupil.x, pupil.y, pupil.z),
                rig.at(pupil.x, pupil.y + 0.01, pupil.z),
                0.042 * rs * blink,
                PUPIL,
                false,
            ));
        }

        // Mouth: a wide dark line that opens with a shout or a gasp.
        let open =
            ((0.25 - anim.fire) / 0.25).clamp(0.0, 1.0) + if air { 0.5 } else { 0.0 } + 0.8 * flinch;
        let mouth = head + look * 0.35 - Vec3::Y * (0.11 + 0.05 * open);
        let mw = 0.10 + 0.05 * open;
        out.push(caps(
            rig.at(mouth.x - mw, mouth.y, mouth.z),
            rig.at(mouth.x + mw, mouth.y, mouth.z),
            0.034 * rs,
            MOUTH,
            false,
        ));

        // A single springy antenna with a ball tip: it swings on its own, so the
        // worm keeps a little life even standing still.
        let head_top = head + Vec3::Y * 0.36;
        let a_sway = if ground { (anim.clock * 2.4).sin() * 0.15 } else { (anim.clock * 4.2).sin() * 0.26 };
        let tip = head_top + Vec3::X * a_sway + Vec3::Y * 0.26;
        out.push(caps(
            rig.at(head_top.x, head_top.y, head_top.z),
            rig.at(tip.x, tip.y, tip.z),
            0.032 * rs,
            darken(colour, 0.72),
            false,
        ));
        out.push(caps(
            rig.at(tip.x, tip.y, tip.z),
            rig.at(tip.x, tip.y + 0.02, tip.z),
            0.075 * rs,
            lighten(colour, 1.25),
            false,
        ));

        for c in &mut out[body_start..] {
            *c = c.proxy();
        }

        // Health bar: a chunky vertical bar over a wounded worm. It is vertical
        // so every camera angle sees its full length, and it only appears once
        // there is damage to report. A fresh hit makes it jump under the flash,
        // which is the "indicator" the eye catches before it reads the length.
        if worm.hp < MAX_HP - 0.01 {
            let frac = (worm.hp / MAX_HP).clamp(0.0, 1.0);
            let colour = if frac > 0.55 {
                BAR_GOOD
            } else if frac > 0.25 {
                BAR_MID
            } else {
                BAR_LOW
            };
            let punch = (1.0 - anim.hurt / 0.18).clamp(0.0, 1.0);
            let base = pos + Vec3::Y * (2.12 + lift);
            // Five stacked dashes, not one bar. A horizontal dash keeps its
            // full width from any camera angle, and the empty sockets stay
            // visible, so the row reads as a gauge instead of a laser sight
            // taped to the worm's head.
            let lit = (frac * 5.0).ceil().max(1.0) as usize;
            let half = right * (BAR_WIDTH * 0.5);
            for k in 0..5 {
                let y = base + Vec3::Y * (k as f32 * BAR_STEP);
                if k < lit {
                    out.push(
                        caps(
                            y - half,
                            y + half,
                            0.026 * rs * (1.0 + 0.55 * punch),
                            colour.lerp(Vec3::ONE, 0.20 * punch),
                            false,
                        )
                        .glowing()
                        .inkless(),
                    );
                } else {
                    out.push(caps(y - half, y + half, 0.020 * rs, Vec3::new(0.13, 0.09, 0.10), false).inkless());
                }
            }
        }

        // Arms. The right one holds the gun; the left one swings with the gait.
        // In the air both flail out; on a wall or a rope both reach up; digging
        // puts both hands forward.
        let (hand_l, hand_r) = if dig {
            (Vec3::new(-0.28, 0.95, 0.44), Vec3::new(0.28, 0.95, 0.44))
        } else if climb {
            let step = (anim.phase * 1.6).sin() * 0.10;
            (
                Vec3::new(-0.34, 1.50 + step, 0.18),
                Vec3::new(0.34, 1.50 - step, 0.18),
            )
        } else if rope {
            (Vec3::new(-0.24, 1.58, 0.12), Vec3::new(0.24, 1.58, 0.12))
        } else if tumble {
            (Vec3::new(-0.36, 0.95, 0.02), Vec3::new(0.34, 0.95, 0.14))
        } else if air {
            (
                Vec3::new(-0.54, 1.20, 0.04),
                Vec3::new(0.52, 1.14, 0.16),
            )
        } else {
            let swing = anim.phase.sin() * 0.15 * run;
            (
                Vec3::new(-0.56, 1.02, 0.10 + swing),
                Vec3::new(0.72, 1.15 + 0.03 * run, 0.26),
            )
        };
        // The kick travels through the arm: the hand snaps back and up, and the
        // spring brings it forward past rest a moment later.
        let mut hand_l = hand_l;
        let mut hand_r = hand_r;
        if gun_id == WHIP {
            let stance = whip_ready * if ground { 1.0 } else { 0.35 };
            // The free arm counterbalances the lash; the whip arm is held out
            // and slightly low, ready to throw. Cock the lash up and back over
            // the shoulder, then throw the arm across the front of the body.
            // The hand rides the lash's side (`yaw`), so it crosses the body
            // with the swing instead of jumping to one side.
            hand_l += Vec3::new(-WHIP_FREE_ARM_THROW * sw.yaw * sw.on, 0.22 * stance + 0.22 * whip_strike, 0.12 * stance);
            hand_r += Vec3::new(0.04 * stance, -0.02 * stance, 0.10 * stance);
            hand_r += Vec3::new(0.30, 0.42, -0.50) * whip_cock;
            hand_r += Vec3::new(WHIP_ARM_THROW * sw.yaw * sw.on, -0.10 * whip_strike, 0.55 * whip_strike);
        }
        let hand_r = hand_r - look * (kick_back * 0.55 * kick) + Vec3::Y * (kick_rise * 0.10 * kick);
        let shoulder_l = Vec3::new(-0.30, 1.18, 0.02);
        let shoulder_r = Vec3::new(0.30, 1.18, 0.02);
        let arm = darken(colour, 0.84);
        let glove = darken(colour, 0.68);
        // The SDF body has its own arms. These old capsules are kept as
        // invisible proxies for the existing rig tests; they cast no shadow.
        let arms_start = out.len();
        for (shoulder, hand) in [(shoulder_l, hand_l), (shoulder_r, hand_r)] {
            out.push(caps(
                rig.at(shoulder.x, shoulder.y, shoulder.z),
                rig.at(hand.x, hand.y, hand.z),
                0.065 * rs,
                arm,
                false,
            ));
            // A gloved hand at the end of the arm, a size up from the wrist.
            out.push(caps(
                rig.at(hand.x, hand.y, hand.z),
                rig.at(hand.x, hand.y + 0.01, hand.z),
                0.075 * rs,
                glove,
                false,
            ));
        }
        for c in &mut out[arms_start..] {
            *c = c.proxy();
        }

        // The SDF body's hands, in the preview's model units. The gun still uses
        // world space, so both are derived from the same rig. Recoil and the
        // whip move the right hand; the arm follows it in the shader.
        let ms = WORM_HEIGHT / SDF_MODEL;
        let mut sdf_hand_r = SDF_HAND_R;
        if gun_id == WHIP {
            let stance = whip_ready * if ground { 1.0 } else { 0.35 };
            sdf_hand_r += Vec3::new(0.04 * stance, -0.02 * stance, 0.10 * stance) / ms;
            sdf_hand_r += Vec3::new(0.30, 0.42, -0.50) * whip_cock / ms;
            sdf_hand_r += Vec3::new(WHIP_ARM_THROW * sw.yaw * sw.on, -0.10 * whip_strike, 0.55 * whip_strike) / ms;
        }
        sdf_hand_r += -look * (kick_back * 0.55 * kick / ms) + Vec3::Y * (kick_rise * 0.10 * kick / ms);
        let mut sdf_hand_l = SDF_HAND_L;
        if gun_id == WHIP {
            let stance = whip_ready * if ground { 1.0 } else { 0.35 };
            sdf_hand_l +=
                Vec3::new(-WHIP_FREE_ARM_THROW * sw.yaw * sw.on, 0.22 * stance + 0.22 * whip_strike, 0.12 * stance) / ms;
        }

        // The weapon. Each id gets its own oversized, distinct silhouette built
        // from the same three primitives, so the per-pixel cost stays one cheap
        // closed-form raycast per piece. `fwd` is the aim in the rig's rotated
        // frame, so the gun flips with the body on a double jump.
        //
        // The SDF body bobs and sways with the gait. The gun rides the same
        // offset, so the hands and the weapon never separate.
        // The whip lunges the whole body forward into the blow (visual only: the
        // sim's position does not move), and back as it recovers.
        let lunge = if gun_id == WHIP { face * (WHIP_LUNGE * (whip_strike - 0.4 * whip_cock)) } else { Vec3::ZERO };
        let sdf_off = Vec3::Y * lift + rig.right * sway(0.5) + lunge;
        let hand_r_world = rig.at(sdf_hand_r.x * ms, sdf_hand_r.y * ms, sdf_hand_r.z * ms) + sdf_off;
        let fwd = (rig.right * look.x + rig.up * look.y + rig.face * look.z).normalize_or_zero();
        // The gun itself travels the rest of the kick, and the muzzle climbs as
        // it does. The hand above took the first half, so the arm compresses
        // instead of the gun sliding through the glove.
        // The lash rope is stepped here, because only here is the hand known:
        // it is pinned to the handle, and a swing pulls it along the sim's lash
        // line (the same `whip_dir` the sim hits and cuts along).
        if gun_id == WHIP {
            let anchor = Gun::new(hand_r_world, fwd, kick_back * 0.45 * kick, kick_rise * kick, rs * 1.25)
                .at(0.22, 0.0, 0.0);
            let pull = swing_phase(&anim).map(|p| SwingPull {
                dir: whip_dir(anim.whip_style, anim.whip_fwd, anim.whip_side, anim.whip_lean, anim.whip_flip, p),
                reach: whip_reach(WEAPONS[WHIP as usize].range, p),
                weight: sw.on,
            });
            let carry = Carry { phase: anim.phase, run };
            anim.rope.step(anchor, face, right, carry, pull, rs, dt.clamp(0.0, 0.05));
            if let Some(slot) = anims.get_mut(i) {
                slot.rope = anim.rope;
            }
        }
        let heat = 0.32 + 0.68 * sw.on.max(whip_cock);
        weapon(
            gun_id,
            hand_r_world,
            fwd,
            kick_back * 0.45 * kick,
            kick_rise * kick,
            rs,
            colour,
            anim.clock,
            Lash { rope: &anim.rope, heat },
            &mut glow,
            &mut out,
        );

        // Tail: two tapering segments behind the body, wagging out of phase.
        // It is the clearest cue for which way the body faces. Like the old
        // arms, these are invisible test proxies; the SDF tail is the one drawn.
        let tail_start = out.len();
        let wag = (anim.phase * 1.1 + 1.3).sin() * (0.10 + 0.08 * run);
        let tail_base = Vec3::new(sway(0.0) * 0.8, 0.42, -0.30);
        let tail_mid = Vec3::new(
            -0.10 + wag * 0.4,
            if climb { 0.18 } else { 0.30 },
            -0.60,
        );
        let tail_tip = Vec3::new(
            -0.28 + wag * 0.8,
            if climb { 0.08 } else { 0.52 },
            -0.78,
        );
        let tail_whip = Vec3::new(
            -0.44 + wag * 1.15,
            if climb { 0.02 } else { 0.62 },
            -0.86,
        );
        out.push(caps(
            rig.at(tail_base.x, tail_base.y, tail_base.z),
            rig.at(tail_mid.x, tail_mid.y, tail_mid.z),
            0.12 * rs,
            darken(colour, 0.90),
            false,
        ));
        out.push(caps(
            rig.at(tail_mid.x, tail_mid.y, tail_mid.z),
            rig.at(tail_tip.x, tail_tip.y, tail_tip.z),
            0.070 * rs,
            darken(colour, 0.82),
            false,
        ));
        out.push(caps(
            rig.at(tail_tip.x, tail_tip.y, tail_tip.z),
            rig.at(tail_whip.x, tail_whip.y, tail_whip.z),
            0.040 * rs,
            darken(colour, 0.72),
            false,
        ));
        for c in &mut out[tail_start..] {
            *c = c.proxy();
        }

        // The SDF body. `ms` maps the preview's 1.52 m sculpt onto WORM_HEIGHT;
        // the inverse of the rig (including the squash) is what the shader uses
        // to put a ray into the model's frame.
        let sdf_inv = Vec3::new(
            1.0 / (rig.scale * rig.wide * ms),
            1.0 / (rig.scale * rig.stretch * ms),
            1.0 / (rig.scale * rig.deep * ms),
        );
        let sdf_y = rig.pivot_y / (rig.scale * rig.stretch * ms);
        let sdf_bound = rig.at(0.0, 0.76 * ms, 0.08 * ms);
        let sdf_radius = 1.12 * ms * rig.scale * rig.wide.max(rig.stretch).max(rig.deep) + 0.06;
        sdfs.push(WormSdf {
            pivot: rig.pivot + sdf_off,
            y_offset: sdf_y,
            right: rig.right,
            up: rig.up,
            face: rig.face,
            inv_scale: sdf_inv,
            hand_r: sdf_hand_r,
            hand_l: sdf_hand_l,
            skin: COLOURS[i % COLOURS.len()],
            flash,
            bound: sdf_bound + sdf_off,
            radius: sdf_radius,
        });

        if let Some(rope) = worm.rope {
            if (rope.anchor - hand_r_world).length() > 0.1 {
                out.push(caps(hand_r_world, rope.anchor, 0.035, ROPE, false));
            }
        }
    }
    (out, glow, sdfs)
}

/// Capsules only, for tests that do not inspect the whip's glow.
#[cfg(test)]
pub fn capsules(worms: &[Worm], positions: &[Vec3], anims: &[Anim]) -> Vec<DrawPrim> {
    let mut anims = anims.to_vec();
    draw(worms, positions, &mut anims, 1.0 / 60.0).0
}

/// Draw `frames` frames in a row, advancing the animation clocks like the game
/// does, and return the last one. The lash is a rope, so it only has its
/// real shape after it has been stepped.
#[cfg(test)]
fn capsules_after(worm: &Worm, anim: &mut Anim, frames: usize) -> Vec<DrawPrim> {
    let dt = 1.0 / 60.0;
    let mut out = Vec::new();
    for _ in 0..frames {
        anim.update(worm, dt);
        let mut anims = [*anim];
        out = draw(std::slice::from_ref(worm), &[worm.pos], &mut anims, dt).0;
        *anim = anims[0];
    }
    out
}

/// The body's forward and right axes. `right` is the direction `move_dir.x`
/// adds when the body faces along `face` (for yaw 0, forward is +Z and right
/// is -X).
fn basis(yaw: f32) -> (Vec3, Vec3) {
    let (s, c) = yaw.sin_cos();
    (Vec3::new(s, 0.0, c), Vec3::new(-c, 0.0, s))
}

fn lighten(c: Vec3, k: f32) -> Vec3 {
    (c * k).clamp(Vec3::ZERO, Vec3::ONE)
}

fn darken(c: Vec3, k: f32) -> Vec3 {
    (c * k).clamp(Vec3::ZERO, Vec3::ONE)
}

/// Rotate `v` about `axis` (which must be unit length).
fn rot(v: Vec3, axis: Vec3, angle: f32) -> Vec3 {
    if angle.abs() < 1.0e-5 {
        return v;
    }
    let (s, c) = angle.sin_cos();
    v * c + axis.cross(v) * s + axis * axis.dot(v) * (1.0 - c)
}

/// The pose offsets that the action supplies; the rig turns these into a
/// rotated body. All angles are radians.
struct Pose {
    /// Forward lean around the body's right axis.
    lean: f32,
    /// End-over-end flip, for the double jump and the tumble.
    flip: f32,
    /// Barrel roll around the body's facing axis, for a wall kick or a dangle.
    roll: f32,
    /// Twist around the body's own up axis: the shoulders turning through a
    /// melee swing. Unlike `lean`, this keeps the feet planted.
    twist: f32,
    /// Vertical scale, with the feet planted.
    stretch: f32,
    wide: f32,
    deep: f32,
}

impl Default for Pose {
    fn default() -> Pose {
        Pose { lean: 0.0, flip: 0.0, roll: 0.0, twist: 0.0, stretch: 1.0, wide: 1.0, deep: 1.0 }
    }
}

/// A local frame for the worm. The model is written for a 1.8 m worm; `scale`
/// maps it onto whatever `WORM_HEIGHT` is now.
struct Rig {
    pivot: Vec3,
    right: Vec3,
    up: Vec3,
    face: Vec3,
    pivot_y: f32,
    scale: f32,
    stretch: f32,
    wide: f32,
    deep: f32,
}

impl Rig {
    fn new(pos: Vec3, face_yaw: f32, pose: &Pose, scale: f32) -> Rig {
        let (base_face, base_right) = basis(face_yaw);
        let turn = pose.lean + pose.flip;
        let up = rot(Vec3::Y, base_right, turn);
        let face = rot(base_face, base_right, turn);
        let right = rot(base_right, face, pose.roll);
        let up = rot(up, face, pose.roll);
        // Last, the twist: the whole upper body turns to throw a melee strike.
        // It comes after the lean and roll so it turns around the body's own
        // vertical, not the world's.
        let face = rot(face, up, pose.twist);
        let right = rot(right, up, pose.twist);
        let pivot_y = 0.95 * scale;
        Rig {
            pivot: pos + Vec3::Y * pivot_y,
            right,
            up,
            face,
            pivot_y,
            scale,
            stretch: pose.stretch,
            wide: pose.wide,
            deep: pose.deep,
        }
    }

    /// A model point (x right, y up from the feet, z forward) in the world.
    /// Stretch scales y from the feet so a squash keeps the worm planted;
    /// lean, flip and roll rotate the whole frame around the body's middle.
    fn at(&self, x: f32, y: f32, z: f32) -> Vec3 {
        let local = self.right * (x * self.scale * self.wide)
            + self.up * (y * self.scale * self.stretch - self.pivot_y)
            + self.face * (z * self.scale * self.deep);
        self.pivot + local
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worm(action: Action, on_ground: bool) -> Worm {
        let mut w = Worm::new(Vec3::new(0.0, 5.0, 0.0));
        w.action = action;
        w.on_ground = on_ground;
        w.forward_vel = if on_ground { 9.0 } else { 0.0 };
        w.vel = Vec3::new(0.0, if on_ground { 0.0 } else { 6.0 }, 9.0);
        w
    }

    /// The `a` endpoint of a capsule/cylinder (the head and body segments these
    /// tests inspect are all capsules); a cuboid's centre stands in for it.
    fn a_of(c: &DrawPrim) -> Vec3 {
        match *c {
            DrawPrim::Capsule { a, .. } | DrawPrim::Cylinder { a, .. } => a,
            DrawPrim::Cuboid { centre, .. } => centre,
        }
    }

    /// The highest point of a primitive, for the squash test.
    fn top(c: &DrawPrim) -> f32 {
        match *c {
            DrawPrim::Capsule { a, b, radius, .. } | DrawPrim::Cylinder { a, b, radius, .. } => a.y.max(b.y) + radius,
            DrawPrim::Cuboid { centre, half, .. } => centre.y + half.y,
        }
    }

    /// Every primitive must be finite and have a positive size.
    fn sane(c: &DrawPrim) -> bool {
        match *c {
            DrawPrim::Capsule { a, b, radius, .. } | DrawPrim::Cylinder { a, b, radius, .. } => {
                a.is_finite() && b.is_finite() && radius.is_finite() && radius > 0.0
            }
            DrawPrim::Cuboid { centre, half, forward, .. } => {
                centre.is_finite() && half.is_finite() && forward.is_finite() && half.max_element() > 0.0
            }
        }
    }

    /// The farthest point of any primitive from `from`, for measuring reach.
    fn furthest(caps: &[DrawPrim], from: Vec3) -> f32 {
        caps.iter()
            .map(|c| match *c {
                DrawPrim::Capsule { a, b, .. } | DrawPrim::Cylinder { a, b, .. } => {
                    a.distance(from).max(b.distance(from))
                }
                DrawPrim::Cuboid { centre, half, .. } => centre.distance(from) + half.length(),
            })
            .fold(f32::MIN, f32::max)
    }

    #[test]
    fn the_body_undulates_as_it_walks() {
        let w = worm(Action::Walk, true);
        let pos = w.pos;
        let mut anim = Anim::default();
        anim.action = Action::Walk;
        anim.phase = 0.0;
        let a = capsules(std::slice::from_ref(&w), &[pos], &[anim]);
        anim.phase = 1.4;
        let b = capsules(std::slice::from_ref(&w), &[pos], &[anim]);
        let moved = (a_of(&a[0]) - a_of(&b[0])).length();
        assert!(moved > 0.02, "the body sways as it walks: {moved}");
        assert!(moved < 0.3, "but does not leave the collision capsule: {moved}");
    }

    #[test]
    fn digging_hunches_the_worm() {
        let mut w = worm(Action::Idle, true);
        let pos = w.pos;
        let anims = [Anim::default()];
        w.digging = true;
        let dig = capsules(std::slice::from_ref(&w), &[pos], &anims);
        w.digging = false;
        let idle = capsules(std::slice::from_ref(&w), &[pos], &anims);
        assert!(a_of(&dig[3]).y < a_of(&idle[3]).y - 0.05, "the head comes down to work");
    }

    #[test]
    fn every_action_makes_a_finite_rig_inside_the_budget() {
        let actions = [
            Action::Idle,
            Action::Walk,
            Action::Jump,
            Action::DoubleJump,
            Action::Freefall,
            Action::WallKick,
            Action::Climb,
            Action::Tumble,
            Action::Rope,
        ];
        for action in actions {
            let mut w = worm(action, matches!(action, Action::Idle | Action::Walk));
            w.face_yaw = 0.7;
            w.yaw = -0.4;
            let mut anim = Anim::default();
            anim.update(&w, 1.0 / 60.0);
            let pos = w.pos;
            let caps = capsules(std::slice::from_ref(&w), &[pos], &[anim]);
            // A worm plus an oversized weapon. Four of these still leave room in
            // MAX_CAPSULES for projectiles and effects (see `four_fully_rigged...`).
            assert!(caps.len() <= 40, "{action:?}: {} capsules", caps.len());
            for c in &caps {
                assert!(sane(c), "{action:?}");
            }
        }
    }

    #[test]
    fn four_fully_rigged_worms_fit_in_one_frame() {
        let worms: Vec<Worm> = (0..4)
            .map(|i| {
                let mut w = worm(Action::Walk, true);
                w.pos = Vec3::new(i as f32 * 3.0, 5.0, 0.0);
                w
            })
            .collect();
        let positions: Vec<Vec3> = worms.iter().map(|w| w.pos).collect();
        let anims: Vec<Anim> = worms.iter().map(|_| Anim::default()).collect();
        let caps = capsules(&worms, &positions, &anims);
        assert!(caps.len() <= crate::renderer::MAX_CAPSULES, "{} capsules", caps.len());
    }

    #[test]
    fn recoil_kicks_then_settles() {
        assert!((recoil_curve(0.0) - 1.0).abs() < 1.0e-6, "the shot starts at full kick");
        assert!(recoil_curve(0.10) < 0.4, "it comes back quickly");
        assert!(recoil_curve(0.20) < 0.0, "then overshoots forward, like a spring");
        assert_eq!(recoil_curve(0.50), 0.0);
        assert_eq!(recoil_curve(2.0), 0.0);
    }

    #[test]
    fn a_hit_flashes_the_body_and_raises_a_health_bar() {
        let w = worm(Action::Idle, true);
        let pos = w.pos;
        let anim = Anim::default();
        let full = capsules(std::slice::from_ref(&w), &[pos], &[anim]);

        // A wounded worm gets five extra primitives: the pip row.
        let mut wounded = w.clone();
        wounded.hp = MAX_HP * 0.2;
        let hurt_anims = [anim];
        let caps = capsules(std::slice::from_ref(&wounded), &[pos], &hurt_anims);
        assert_eq!(caps.len(), full.len() + 5, "the health bar");
        let bar = caps
            .iter()
            .find_map(|c| match c {
                DrawPrim::Capsule { a, b, colour, .. } if *colour == BAR_LOW => Some((*a, *b)),
                _ => None,
            })
            .expect("low health is drawn in red");
        assert!(
            bar.0.y > pos.y + 1.8 && (bar.1 - bar.0).length() > 0.1,
            "the bar floats above the head: {bar:?}"
        );

        // A fresh hit whitens the body and knocks it off its axis.
        let mut flinching = Anim::default();
        flinching.hurt(30.0);
        let flashed = capsules(std::slice::from_ref(&w), &[pos], &[flinching]);
        let plain_colour = match full[0] {
            DrawPrim::Capsule { colour, .. } => colour,
            _ => panic!("the body starts with a capsule"),
        };
        let flash_colour = match flashed[0] {
            DrawPrim::Capsule { colour, .. } => colour,
            _ => panic!("the body starts with a capsule"),
        };
        assert!(flash_colour.y > plain_colour.y + 0.1, "the flash goes white: {plain_colour:?} {flash_colour:?}");
        assert!(
            (a_of(&full[0]) - a_of(&flashed[0])).length() > 0.005,
            "the flinch moves the body"
        );
    }

    #[test]
    fn the_big_guns_kick_harder_than_the_blaster() {
        assert!(recoil_style(NOVA).0 > recoil_style(ROCKET).0);
        assert!(recoil_style(ROCKET).0 > recoil_style(BLASTER).0);
        assert!(recoil_style(BLASTER).0 > 0.0);
        assert_eq!(recoil_style(WHIP), (0.0, 0.0), "the swing is the recoil");
    }

    #[test]
    fn a_whip_swing_draws_back_throws_and_recovers() {
        let swing_at = |age: f32, flip: bool| {
            let mut a = Anim::default();
            a.struck(WhipStyle::Sweep, flip, Vec3::Z, Vec3::NEG_X, 0.0);
            a.whip = age;
            swung(&a)
        };
        let start = swing_at(0.0, false);
        assert_eq!((start.cock, start.strike), (0.0, 0.0), "it starts at rest, not thrown");
        assert!(swing_at(0.04, false).cock > 0.8, "the arm draws back first");
        let mid = swing_at(WHIP_SWING_TIME * 0.5, false);
        assert!(mid.strike > 0.9 && mid.yaw.abs() < 0.05, "thrown through the middle: {}", mid.strike);
        let end = swing_at(WHIP_SWING_TIME, false);
        assert_eq!((end.cock, end.strike, end.on), (0.0, 0.0, 0.0), "and the follow-through is over");
        // The body settles after the blow, not during the wind-up, and is level again at the end.
        assert_eq!((start.follow, end.follow), (0.0, 0.0));
        assert!(swing_at(WHIP_SWING_TIME * 0.8, false).follow > 0.9, "it lands heavily as the lash arrives");
        // The lash crosses from one side to the other, and the next swing mirrors it.
        assert!(swing_at(0.02, false).yaw > 0.9 && swing_at(0.28, false).yaw < -0.9);
        assert!(swing_at(0.02, true).yaw < -0.9 && swing_at(0.28, true).yaw > 0.9);
        // The beat matters: a swing that lands in one frame has no weight.
        assert!(WHIP_SWING_TIME > 0.2);
    }

    #[test]
    fn the_plasma_lash_is_carried_and_then_swung_out() {
        let mut w = worm(Action::Idle, true);
        w.current = WHIP as usize;
        let pos = w.pos;
        let mut anim = Anim::default();

        // At rest the lash hangs from the hand, long enough to read as a weapon.
        let idle = capsules_after(&w, &mut anim, 60);
        let idle_reach = furthest(&idle, pos);
        assert!(idle_reach > 3.0 && idle_reach < 7.0, "carried out: {idle_reach}");
        assert!(idle.iter().all(sane));

        // The swing pays the rope out: part way through, it is much longer.
        anim.struck(WhipStyle::Sweep, false, Vec3::Z, Vec3::NEG_X, 0.0);
        let strike = capsules_after(&w, &mut anim, 9);
        assert!(furthest(&strike, pos) > 7.5, "the lash reaches out: {}", furthest(&strike, pos));
        assert!(strike.iter().all(sane));

        // And it settles back to the carried pose, not to a hidden one.
        let settled = capsules_after(&w, &mut anim, 90);
        let reach = furthest(&settled, pos);
        assert!(reach > 3.0 && reach < 7.0, "settled out: {reach}");
        assert!(settled.iter().all(sane));
    }

    #[test]
    fn the_lash_follows_the_hand_instead_of_staying_on_one_side() {
        // The first complaint about the old whip: it was pinned to one side and
        // jumped across when it attacked. Walk a whole swing and check the tip
        // really crosses the front, frame by frame, with no jump.
        let mut w = worm(Action::Idle, true);
        w.current = WHIP as usize;
        let mut anim = Anim::default();
        capsules_after(&w, &mut anim, 60);
        anim.struck(WhipStyle::Sweep, false, Vec3::Z, Vec3::NEG_X, 0.0);
        let mut tips = Vec::new();
        for _ in 0..18 {
            capsules_after(&w, &mut anim, 1);
            tips.push(anim.rope.pos[LASH_NODES - 1] - anim.rope.pos[0]);
        }
        let (first, last) = (tips[2], tips[16]);
        assert!(first.x < -1.0 && last.x > 1.0, "right to left across the front: {first:?} {last:?}");
        // The tip is fast (150 degrees in 0.3 s, peaking near 4 m a frame) but
        // continuous: a warp across the body would be a step of 10 m or more.
        let biggest = tips.windows(2).map(|p| (p[1] - p[0]).length()).fold(0.0, f32::max);
        assert!(biggest < 5.0, "the tip never teleports: {biggest} m in one frame");
    }

    /// How far left/right of the hand the carried lash is at the tip and at its
    /// middle, frame by frame, once it has had a second to settle in. The worm
    /// faces +Z, so its right is -X.
    fn carried_sway(forward_vel: f32, frames: usize) -> Vec<(f32, f32)> {
        let mut w = worm(Action::Walk, true);
        w.forward_vel = forward_vel;
        w.vel = Vec3::new(0.0, 0.0, forward_vel);
        w.current = WHIP as usize;
        let (_, right) = basis(w.face_yaw);
        let mut anim = Anim::default();
        capsules_after(&w, &mut anim, 60);
        let mut out = Vec::new();
        for _ in 0..frames {
            capsules_after(&w, &mut anim, 1);
            let hand = anim.rope.pos[0];
            out.push((
                (anim.rope.pos[LASH_NODES - 1] - hand).dot(right),
                (anim.rope.pos[LASH_NODES / 2] - hand).dot(right),
            ));
        }
        out
    }

    fn spread(sway: &[(f32, f32)]) -> (f32, f32) {
        let tips = sway.iter().map(|s| s.0);
        (tips.clone().fold(f32::MAX, f32::min), tips.fold(f32::MIN, f32::max))
    }

    #[test]
    fn the_carried_lash_swings_left_and_right_as_the_worm_walks() {
        let sway = carried_sway(9.0, 240);
        let (lo, hi) = spread(&sway);
        assert!(lo < -0.4 && hi > 0.4, "the tip crosses the front: {lo} .. {hi}");
        // It curls: the tip is behind the middle, so for part of every cycle
        // they are on opposite sides of the line from the hand (an S).
        assert!(sway.iter().any(|&(tip, mid)| tip * mid < -0.02), "the lash curls into an S");
    }

    #[test]
    fn an_idle_lash_sways_much_less_than_a_running_one_but_is_never_dead() {
        let (idle_lo, idle_hi) = spread(&carried_sway(0.0, 240));
        let (run_lo, run_hi) = spread(&carried_sway(9.0, 240));
        let (idle, run) = (idle_hi - idle_lo, run_hi - run_lo);
        assert!(idle > 0.05, "never dead still: {idle}");
        assert!(idle < 0.7 * run, "but much calmer than a run: {idle} vs {run}");
    }

    #[test]
    fn a_whole_arsenal_of_whips_still_fits_in_one_frame() {
        let worms: Vec<Worm> = (0..4)
            .map(|i| {
                let mut w = worm(Action::Idle, true);
                w.pos = Vec3::new(i as f32 * 3.0, 5.0, 0.0);
                w.current = WHIP as usize;
                w
            })
            .collect();
        let positions: Vec<Vec3> = worms.iter().map(|w| w.pos).collect();
        let anims: Vec<Anim> = worms.iter().map(|_| Anim::default()).collect();
        let caps = capsules(&worms, &positions, &anims);
        assert!(caps.len() <= crate::renderer::MAX_CAPSULES, "{} capsules", caps.len());
    }

    #[test]
    fn a_recoiling_gun_is_still_finite() {
        let mut w = worm(Action::Idle, true);
        w.current = ROCKET as usize;
        let pos = w.pos;
        let mut anim = Anim::default();
        anim.fired();
        for age in [0.0, 0.02, 0.1, 0.2, 0.4] {
            anim.fire = age;
            let caps = capsules(std::slice::from_ref(&w), &[pos], &[anim]);
            for c in &caps {
                assert!(sane(c), "recoil at {age} s");
            }
        }
    }

    #[test]
    fn the_double_jump_is_one_full_flip() {
        let w = worm(Action::DoubleJump, false);
        let pos = w.pos;
        let mut anim = Anim::default();
        anim.action = Action::DoubleJump;
        anim.since = 0.0;
        let start = capsules(std::slice::from_ref(&w), &[pos], &[anim]);
        anim.since = 0.62; // exactly one turn: the same orientation again
        let end = capsules(std::slice::from_ref(&w), &[pos], &[anim]);
        let head_start = a_of(&start[3]); // the head capsule, after the three body ones
        let head_end = a_of(&end[3]);
        assert!((head_start - head_end).length() < 0.02, "full flip returns: {head_start:?} vs {head_end:?}");

        anim.since = 0.31; // halfway: head below the feet
        let middle = capsules(std::slice::from_ref(&w), &[pos], &[anim]);
        assert!((head_start - a_of(&middle[3])).length() > 0.3, "halfway is visibly flipped");
    }

    #[test]
    fn a_landing_squashes_once() {
        let w = worm(Action::Walk, true);
        let pos = w.pos;
        let mut anim = Anim::default();
        // Pretend the previous update was in the air.
        anim.was_grounded = false;
        anim.action = Action::Walk;
        anim.since = 0.0;
        anim.update(&w, 0.0);
        assert!(anim.landed);
        let squashed = capsules(std::slice::from_ref(&w), &[pos], &[anim]);
        let tallest = squashed.iter().map(top).fold(f32::MIN, f32::max);
        anim.since = 0.5; // the squash is over
        let normal = capsules(std::slice::from_ref(&w), &[pos], &[anim]);
        let normal_tallest = normal.iter().map(top).fold(f32::MIN, f32::max);
        assert!(tallest < normal_tallest, "squash is shorter: {tallest} vs {normal_tallest}");
    }

    #[test]
    fn the_sdf_body_has_one_block_per_worm_and_a_sane_frame() {
        let w = worm(Action::Idle, true);
        let pos = w.pos;
        let anim = Anim::default();
        let (_, _, sdfs) = draw(std::slice::from_ref(&w), &[pos], &mut [anim], 1.0 / 60.0);
        assert_eq!(sdfs.len(), 1, "one SDF block per live worm");
        let s = sdfs[0];
        assert!(s.pivot.is_finite() && s.pivot.y > 0.0);
        assert!(s.right.is_normalized() && s.up.is_normalized() && s.face.is_normalized());
        assert!(s.inv_scale.min_element() > 0.0 && s.inv_scale.is_finite());
        assert!(s.hand_r.length() > 0.2 && s.hand_l.length() > 0.2);
        assert!(s.radius > 0.5 && s.radius.is_finite());
        assert!(s.bound.distance(pos) < 3.0, "the bound is on the worm");
    }
}
