// Turns the sim's worms into the capsules the renderer draws. The sim knows
// nothing about colours or how a worm looks; this is the only place that does.
//
// The worm is a stack of overlapping capsules with alternating shades, so it
// reads as a segmented body, plus a tail, a bulbous head, two eyes, tiny arms
// and the gun. There are no legs: it gets around by undulating, which is also
// what most of the animation below is about.

use crate::renderer::DrawPrim;
use glam::Vec3;
use sim::weapons::{
    BLASTER, BOMB, BOUNCER, BUILD, CLUSTER, NOVA, RAILGUN, ROCKET, SWARM, TELEPORT, TRACTOR, WEAPONS, WHIP,
};
use sim::{whip_path, Action, WhipStyle, Worm, WHIP_POINTS, WHIP_SWEEP, WORM_HEIGHT};
use std::f32::consts::TAU;

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
    /// Seconds since this worm's whip cracked, and the swing it is drawing.
    /// The lash is carried by the worm every frame, so this is what turns the
    /// draw-back into a strike without spawning a whole new weapon effect.
    whip: f32,
    whip_phase: f32,
    whip_style: WhipStyle,
}

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
            whip_phase: 0.0,
            whip_style: WhipStyle::Sweep,
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

    /// The plasma whip cracked: start the swing that the carried lash follows.
    /// `phase` is the sim's ripple, so the drawn lash and the scar agree.
    pub fn struck(&mut self, style: WhipStyle, phase: f32) {
        self.whip = 0.0;
        self.whip_style = style;
        self.whip_phase = phase;
    }

    /// Start the swing `age` seconds in. The screenshot path knows how many
    /// ticks it has stepped, so it can place the lash mid-crack without
    /// stepping the animation clock through the wind-up as well.
    pub fn struck_at(&mut self, style: WhipStyle, phase: f32, age: f32) {
        self.struck(style, phase);
        self.whip = age.max(0.0);
    }
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
const PLASMA: Vec3 = Vec3::new(0.55, 0.28, 1.0);
/// The white-hot core of the lash, used sparingly: a whip made entirely of it
/// reads as a flat ribbon.
const PLASMA_TIP: Vec3 = Vec3::new(0.95, 0.82, 1.0);

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

/// How long each part of a plasma-whip strike takes, in seconds. The unfurl is
/// the tip travelling out to the target, the swing is the lash crossing the arc
/// with the body, and the recovery is the follow-through. Together they are a
/// little shorter than the sim's cycle (0.30 s draw-back + 0.45 s cooldown), so
/// the lash is back in hand before the next strike.
const LASH_UNFURL: f32 = 0.10;
const LASH_SWING: f32 = 0.22;
const LASH_RECOVER: f32 = 0.26;
const LASH_LIFE: f32 = LASH_UNFURL + LASH_SWING + LASH_RECOVER;

/// The whip state the model needs: how far it is drawn back, and how long ago
/// it cracked.
#[derive(Clone, Copy)]
struct Lash {
    /// 0..1: how far the lash is cocked over the shoulder. It stays at 1
    /// through the crack, then relaxes, so the strike does not snap the arm
    /// back to rest on the frame the sim fires.
    cock: f32,
    /// Seconds since the strike; `LASH_LIFE` or more when the lash is at rest.
    age: f32,
    /// The sim's ripple, so the drawn lash and the scar it leaves agree.
    phase: f32,
    style: WhipStyle,
}

/// Smoothstep, for poses and for the lash's travel. Linear motion over a few
/// frames reads as a slide; a smooth one reads as a swing.
fn smooth(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// How far the body is thrown into the strike, 0..1: up on the swing, then
/// back down through the follow-through.
fn strike_body(age: f32) -> f32 {
    let lead = smooth(age / (LASH_UNFURL + LASH_SWING));
    let back = smooth((age - LASH_UNFURL - LASH_SWING) / LASH_RECOVER);
    lead * (1.0 - back)
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
    out: &mut Vec<DrawPrim>,
) {
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
            out.push(g.flange(-0.18, 0.16, 0.04, EMBER).glowing());
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
            // A tosser holding one enormous bomb in a three-claw cradle, fuse lit.
            out.push(g.plate((0.08, 0.0, 0.02), (0.07, 0.10, 0.15), RUST));
            out.push(g.tube((0.24, 0.0, 0.0), (0.52, 0.19, 0.0), 0.035, DARKMETAL));
            out.push(g.tube((0.24, 0.0, 0.0), (0.50, -0.12, 0.14), 0.035, DARKMETAL));
            out.push(g.tube((0.24, 0.0, 0.0), (0.50, -0.12, -0.14), 0.035, DARKMETAL));
            out.push(g.flange(0.46, 0.29, 0.07, DARKMETAL));
            out.push(g.ball((0.74, 0.0, 0.0), 0.30, BOMBSHELL));
            out.push(g.tube((0.82, 0.05, 0.15), (0.94, 0.12, 0.28), 0.022, RUST));
            out.push(g.ball((0.96, 0.13, 0.30), 0.06, EMBER).glowing());
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
            // The lash is carried, not spawned: the same energy sits in the
            // hand at rest, is drawn back by the wind-up, and cracks out with
            // the strike. See `lash`.
            lash_draw(&g, lash, rs, clock, out);
        }
        _ => {
            // BOMBLET is never held; unknown ids draw nothing.
        }
    }
}

/// The plasma lash: one continuous piece of energy, carried by the worm.
///
/// It is never spawned by a shot. At rest it hangs from the emitter and
/// undulates; as the sim's wind-up runs it is drawn back over the shoulder;
/// when the strike fires the tip snaps out to the weapon's reach, crosses the
/// arc with the body, then falls back to whatever the hand is doing (which may
/// already be the next draw-back).
fn lash_draw(g: &Gun, l: Lash, rs: f32, clock: f32, out: &mut Vec<DrawPrim>) {
    let from = g.at(0.46, 0.0, 0.0);
    // Carried: hangs forward and down, out of the way.
    let idle_dir = (g.fwd * 0.8 - Vec3::Y * 0.65).normalize_or_zero();
    let idle_len = 1.45 * rs;
    // Cocked: drawn up and back over the shoulder, gathered behind the worm.
    let cock_dir = (g.up * 0.65 - g.fwd * 0.55 - g.side * 0.40).normalize_or_zero();
    let cock_len = 2.00 * rs;

    let wind = l.cock;
    let base_dir = idle_dir.lerp(cock_dir, wind).normalize_or_zero();
    let base_len = idle_len + (cock_len - idle_len) * wind;
    let mut dir = base_dir;
    let mut len = base_len;
    let mut swing = 0.0;
    let mut heat = 0.30 + 0.70 * wind;

    if l.age < LASH_LIFE {
        // Unfurl: the tip travels from the hand out to the aim.
        let u = smooth(l.age / LASH_UNFURL);
        dir = dir.lerp(g.fwd, u).normalize_or_zero();
        len += (WEAPONS[WHIP as usize].range - len) * u;
        // Swing: the lash crosses the arc. The sim's fan covers the whole
        // left-to-right sweep, exactly as this rotation does.
        let s = smooth((l.age - LASH_UNFURL) / LASH_SWING);
        swing = match l.style {
            WhipStyle::Sweep => WHIP_SWEEP * (1.0 - 2.0 * s),
            WhipStyle::Smash => 0.0,
        };
        // Follow-through: fall back to the carried pose. Squaring the ease
        // leaves the lash hanging at full length for a beat before it drops.
        let back = smooth((l.age - LASH_UNFURL - LASH_SWING) / LASH_RECOVER);
        let back = back * back;
        dir = dir.lerp(base_dir, back).normalize_or_zero();
        len += (base_len - len) * back;
        swing *= 1.0 - back;
        heat = heat.max(1.0 - 0.55 * back);
    }

    // The same curve the sim uses, swung around the vertical axis from the
    // emitter: that rotation is the sweep the hit test covers.
    let path = whip_path(from, dir, g.side, l.phase + clock * 1.2, len, l.style);
    let (ss, cs) = swing.sin_cos();
    let place = |p: Vec3| {
        let d = p - from;
        from + Vec3::new(d.x * cs + d.z * ss, d.y, -d.x * ss + d.z * cs)
    };

    for k in 0..WHIP_POINTS - 1 {
        let t = k as f32 / (WHIP_POINTS - 1) as f32;
        // A travelling pulse of brighter segments, like current running down
        // the lash, so it looks alive even held still.
        let pulse = (clock * 7.0 - t * 9.0).sin() * 0.5 + 0.5;
        let base = if k % 3 == 0 { PLASMA } else { lighten(PLASMA, 1.15) };
        let col = base.lerp(PLASMA_TIP, heat * (0.12 + 0.20 * pulse));
        let r = (0.044 - 0.020 * t) * rs * (1.0 + 0.70 * heat);
        out.push(
            DrawPrim::capsule(place(path[k]), place(path[k + 1]), r, col, false)
                .glowing()
                .inkless(),
        );
    }
    // The hot tip: a small ball of energy that leads the crack.
    let tip = place(path[WHIP_POINTS - 1]);
    let size = (0.055 + 0.055 * heat) * rs;
    out.push(
        DrawPrim::capsule(tip, tip + Vec3::Y * 0.004, size, PLASMA_TIP, false)
            .glowing()
            .inkless(),
    );
}

/// The capsules for `worms`, each drawn at `positions[i]` (the sim's position
/// smoothed between two ticks, see `Viewer::frame`).
pub fn capsules(worms: &[Worm], positions: &[Vec3], anims: &[Anim]) -> Vec<DrawPrim> {
    let mut out = Vec::new();
    for (i, (worm, &pos)) in worms.iter().zip(positions).enumerate() {
        if !worm.alive {
            continue; // (the death flash is an effect, see `effects.rs`)
        }
        let anim = anims.get(i).copied().unwrap_or_default();
        let colour = COLOURS[i % COLOURS.len()];
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
        // it draws back during the wind-up, then throws the shoulder through
        // the strike. Both are state, so they show in a screenshot too.
        let whip_charge = if gun_id == WHIP { smooth(worm.charge_fraction()) } else { 0.0 };
        // The sim spends the charge on the firing tick. Keep the draw-back pose
        // for the first beat of the strike anyway, or the arm would snap back
        // to rest on the very frame the lash cracks.
        let whip_cock = if gun_id == WHIP && anim.whip < LASH_LIFE {
            whip_charge.max(1.0 - smooth(anim.whip / (LASH_UNFURL + LASH_SWING)))
        } else {
            whip_charge
        };
        let whip_strike = if gun_id == WHIP { strike_body(anim.whip) } else { 0.0 };

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
            // Coil away from the target as the lash is cocked, then lean and
            // twist through it as it cracks. This is the "full strike": the
            // shoulder does the work, not the wrist.
            pose.lean += -0.22 * whip_cock + 0.28 * whip_strike;
            pose.roll += 0.16 * whip_cock - 0.20 * whip_strike;
            pose.twist += 0.20 * whip_cock - 0.24 * whip_strike;
            pose.wide *= 1.0 + 0.04 * whip_strike;
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
        let blink = if (anim.clock + (i as f32) * 0.9) % 4.3 < 0.10 { 0.15 } else { 1.0 };
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
        let open = ((0.25 - anim.fire) / 0.25).clamp(0.0, 1.0) + if air { 0.5 } else { 0.0 };
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
        let mut hand_r = hand_r;
        if gun_id == WHIP {
            // Cock the lash up and back over the shoulder, then throw the arm
            // across the front of the body. The arm and the lash move together,
            // so the weapon never looks like it is waving itself.
            hand_r += Vec3::new(0.18, 0.24, -0.34) * whip_cock;
            hand_r += Vec3::new(-0.75, -0.10, 0.38) * whip_strike;
        }
        let hand_r = hand_r - look * (kick_back * 0.55 * kick) + Vec3::Y * (kick_rise * 0.10 * kick);
        let shoulder_l = Vec3::new(-0.30, 1.18, 0.02);
        let shoulder_r = Vec3::new(0.30, 1.18, 0.02);
        let arm = darken(colour, 0.84);
        let glove = darken(colour, 0.68);
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

        // The weapon. Each id gets its own oversized, distinct silhouette built
        // from the same three primitives, so the per-pixel cost stays one cheap
        // closed-form raycast per piece. `fwd` is the aim in the rig's rotated
        // frame, so the gun flips with the body on a double jump.
        let hand_r_world = rig.at(hand_r.x, hand_r.y, hand_r.z);
        let fwd = (rig.right * look.x + rig.up * look.y + rig.face * look.z).normalize_or_zero();
        // The gun itself travels the rest of the kick, and the muzzle climbs as
        // it does. The hand above took the first half, so the arm compresses
        // instead of the gun sliding through the glove.
        weapon(
            gun_id,
            hand_r_world,
            fwd,
            kick_back * 0.45 * kick,
            kick_rise * kick,
            rs,
            colour,
            anim.clock,
            Lash {
                cock: whip_cock,
                age: anim.whip,
                phase: anim.whip_phase,
                style: anim.whip_style,
            },
            &mut out,
        );

        // Tail: two tapering segments behind the body, wagging out of phase.
        // It is the clearest cue for which way the body faces.
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

        if let Some(rope) = worm.rope {
            if (rope.anchor - hand_r_world).length() > 0.1 {
                out.push(caps(hand_r_world, rope.anchor, 0.035, ROPE, false));
            }
        }
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
    fn the_big_guns_kick_harder_than_the_blaster() {
        assert!(recoil_style(NOVA).0 > recoil_style(ROCKET).0);
        assert!(recoil_style(ROCKET).0 > recoil_style(BLASTER).0);
        assert!(recoil_style(BLASTER).0 > 0.0);
        assert_eq!(recoil_style(WHIP), (0.0, 0.0), "the swing is the recoil");
    }

    #[test]
    fn the_plasma_lash_is_carried_and_then_cracks_out() {
        let mut w = worm(Action::Idle, true);
        w.current = WHIP as usize;
        let pos = w.pos;
        let mut anim = Anim::default();

        // At rest the lash is in the hand, short and drooping, never a full
        // nine-metre weapon waiting to appear.
        let idle = capsules(std::slice::from_ref(&w), &[pos], &[anim]);
        assert!(furthest(&idle, pos) < 4.0, "carried short: {}", furthest(&idle, pos));
        assert!(idle.iter().all(sane));

        // Drawn back: still carried, but gathered up towards the shoulder.
        w.weapons[WHIP as usize].charge = WEAPONS[WHIP as usize].windup;
        let cocked = capsules(std::slice::from_ref(&w), &[pos], &[anim]);
        assert!(furthest(&cocked, pos) < 4.0, "still in hand while cocked");
        assert!(cocked.iter().all(sane));

        // The strike: the tip reaches almost the whole 9.5 m.
        anim.struck(WhipStyle::Sweep, 0.0);
        anim.whip = LASH_UNFURL + LASH_SWING * 0.5;
        let strike = capsules(std::slice::from_ref(&w), &[pos], &[anim]);
        assert!(furthest(&strike, pos) > 8.0, "the crack reaches out: {}", furthest(&strike, pos));
        assert!(strike.iter().all(sane));

        // And it is gone again by the time the next strike can start, so the
        // carried pose is continuous across the cycle.
        anim.whip = LASH_LIFE + 0.01;
        let settled = capsules(std::slice::from_ref(&w), &[pos], &[anim]);
        assert!(furthest(&settled, pos) < 4.0);
        for c in &settled {
            assert!(sane(c), "settled lash");
        }
    }

    #[test]
    fn a_whole_arsenal_of_whips_still_fits_in_one_frame() {
        let worms: Vec<Worm> = (0..4)
            .map(|i| {
                let mut w = worm(Action::Idle, true);
                w.pos = Vec3::new(i as f32 * 3.0, 5.0, 0.0);
                w.current = WHIP as usize;
                w.weapons[WHIP as usize].charge = WEAPONS[WHIP as usize].windup;
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
}
