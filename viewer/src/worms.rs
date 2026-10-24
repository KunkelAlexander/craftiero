// Turns the sim's worms into the capsules the renderer draws. The sim knows
// nothing about colours or how a worm looks; this is the only place that does.
//
// The worm is a stack of overlapping capsules with alternating shades, so it
// reads as a segmented body, plus a tail, a bulbous head, two eyes, tiny arms
// and the gun. There are no legs: it gets around by undulating, which is also
// what most of the animation below is about.

use crate::renderer::DrawCapsule;
use glam::Vec3;
use sim::{Action, Worm, WORM_HEIGHT};
use std::f32::consts::TAU;

/// One colour per player (as picked in a paint program).
const COLOURS: [Vec3; 4] = [
    Vec3::new(0.93, 0.42, 0.33), // coral
    Vec3::new(0.33, 0.58, 0.92), // blue
    Vec3::new(0.96, 0.80, 0.30), // yellow
    Vec3::new(0.45, 0.78, 0.40), // green
];
const GUN: Vec3 = Vec3::new(0.22, 0.20, 0.28);
const EYE: Vec3 = Vec3::new(1.0, 0.97, 0.9);
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
        // Running makes the undulation quicker; idle keeps a slow ripple.
        let gait = if worm.on_ground {
            3.0 + worm.forward_vel.abs() * 0.55
        } else {
            1.5
        };
        self.phase = (self.phase + dt * gait) % TAU;
    }
}

/// The capsules for `worms`, each drawn at `positions[i]` (the sim's position
/// smoothed between two ticks, see `Viewer::frame`).
pub fn capsules(worms: &[Worm], positions: &[Vec3], anims: &[Anim]) -> Vec<DrawCapsule> {
    let mut out = Vec::new();
    for (i, (worm, &pos)) in worms.iter().zip(positions).enumerate() {
        if !worm.alive {
            continue; // (the death flash is an effect, see `effects.rs`)
        }
        let anim = anims.get(i).copied().unwrap_or_default();
        let colour = COLOURS[i % COLOURS.len()];
        let rs = WORM_HEIGHT / 1.8;
        let caps = |a: Vec3, b: Vec3, radius: f32, colour: Vec3, shadow: bool| DrawCapsule {
            a,
            b,
            radius,
            colour,
            shadow,
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
        // make the segmentation read even on the flat toon bands. The lowest
        // sphere touches the ground, so the worm sits on the terrain instead of
        // being cut off by it.
        let body_y = [0.46, 0.78, 1.10, 1.43];
        let body_r = [0.46, 0.43, 0.36, 0.30];
        let mut prev = rig.at(sway(0.0), body_y[0] + lift, surge(0.0));
        for k in 1..body_y.len() {
            let t = k as f32 / (body_y.len() - 1) as f32;
            let p = rig.at(sway(t), body_y[k] + lift, surge(t));
            let r = 0.5 * (body_r[k - 1] + body_r[k]) * rs;
            let shade = if k % 2 == 0 { colour } else { darken(colour, 0.82) };
            out.push(caps(prev, p, r, shade, true));
            prev = p;
        }

        // Head: a bulb on top of the neck, with a big cartoon face.
        let head_x = sway(1.0) * 0.7;
        let head = Vec3::new(head_x, 1.52 + lift, 0.0);
        out.push(caps(
            rig.at(head.x, head.y - 0.02, 0.0),
            rig.at(head.x, head.y + 0.02, 0.0),
            0.34 * rs,
            lighten(colour, 1.06),
            true,
        ));

        // Belly stripe: a light band on the front. Besides being worm-like, it
        // shows which way the body faces when the mouse aims elsewhere.
        let belly = lighten(colour, 1.35);
        out.push(caps(
            rig.at(sway(0.18), 0.44 + lift, 0.36),
            rig.at(sway(0.72), 1.10 + lift, 0.29),
            0.12 * rs,
            belly,
            false,
        ));

        // Eyes: white eyeballs and dark pupils on the front of the head. They
        // look along the aim, so the body can turn while the mouse looks
        // somewhere else. The occasional blink is a radius, not a new capsule.
        let blink = if (anim.clock + (i as f32) * 0.9) % 4.3 < 0.10 { 0.15 } else { 1.0 };
        for side in [-1.0, 1.0] {
            let eye = head + look * 0.28 + Vec3::X * (0.14 * side) + Vec3::Y * 0.04;
            out.push(caps(
                rig.at(eye.x - look.x * 0.02, eye.y - look.y * 0.02, eye.z - look.z * 0.02),
                rig.at(eye.x + look.x * 0.04, eye.y + look.y * 0.04, eye.z + look.z * 0.04),
                0.105 * rs * blink,
                EYE,
                false,
            ));
            let pupil = eye + look * 0.125;
            out.push(caps(
                rig.at(pupil.x, pupil.y, pupil.z),
                rig.at(pupil.x, pupil.y + 0.01, pupil.z),
                0.048 * rs * blink,
                PUPIL,
                false,
            ));
        }

        // Mouth: a small dark line under the eyes.
        let mouth = head + look * 0.33 - Vec3::Y * 0.10;
        out.push(caps(
            rig.at(mouth.x - 0.09, mouth.y, mouth.z),
            rig.at(mouth.x + 0.09, mouth.y, mouth.z),
            0.028 * rs,
            MOUTH,
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
                Vec3::new(-0.55, 0.92, 0.10 + swing),
                Vec3::new(0.55, 0.98 + 0.03 * run, 0.24),
            )
        };
        let shoulder_l = Vec3::new(-0.42, 1.16, 0.02);
        let shoulder_r = Vec3::new(0.42, 1.16, 0.02);
        let arm = darken(colour, 0.86);
        out.push(caps(
            rig.at(shoulder_l.x, shoulder_l.y, shoulder_l.z),
            rig.at(hand_l.x, hand_l.y, hand_l.z),
            0.085 * rs,
            arm,
            false,
        ));
        out.push(caps(
            rig.at(shoulder_r.x, shoulder_r.y, shoulder_r.z),
            rig.at(hand_r.x, hand_r.y, hand_r.z),
            0.085 * rs,
            arm,
            false,
        ));

        // Gun: a thick grip at the hand and a slim barrel along the aim. The
        // end sphere is the muzzle.
        let hand_r_world = rig.at(hand_r.x, hand_r.y, hand_r.z);
        let grip = hand_r + look * 0.12;
        let muzzle = hand_r + look * 0.68;
        out.push(caps(hand_r_world, rig.at(grip.x, grip.y, grip.z), 0.105 * rs, GUN, false));
        out.push(caps(
            rig.at(grip.x, grip.y, grip.z),
            rig.at(muzzle.x, muzzle.y, muzzle.z),
            0.050 * rs,
            darken(GUN, 0.80),
            false,
        ));

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
            -0.30 + wag,
            if climb { 0.08 } else { 0.52 },
            -0.72,
        );
        out.push(caps(
            rig.at(tail_base.x, tail_base.y, tail_base.z),
            rig.at(tail_mid.x, tail_mid.y, tail_mid.z),
            0.15 * rs,
            darken(colour, 0.90),
            false,
        ));
        out.push(caps(
            rig.at(tail_mid.x, tail_mid.y, tail_mid.z),
            rig.at(tail_tip.x, tail_tip.y, tail_tip.z),
            0.070 * rs,
            darken(colour, 0.80),
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
    /// Vertical scale, with the feet planted.
    stretch: f32,
    wide: f32,
    deep: f32,
}

impl Default for Pose {
    fn default() -> Pose {
        Pose { lean: 0.0, flip: 0.0, roll: 0.0, stretch: 1.0, wide: 1.0, deep: 1.0 }
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
        let moved = (a[0].a - b[0].a).length();
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
        assert!(dig[3].a.y < idle[3].a.y - 0.05, "the head comes down to work");
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
            assert!(caps.len() <= 16, "{action:?}: {} capsules", caps.len());
            for c in &caps {
                assert!(c.a.is_finite() && c.b.is_finite() && c.radius.is_finite(), "{action:?}");
                assert!(c.radius > 0.0, "{action:?}");
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
    fn the_double_jump_is_one_full_flip() {
        let w = worm(Action::DoubleJump, false);
        let pos = w.pos;
        let mut anim = Anim::default();
        anim.action = Action::DoubleJump;
        anim.since = 0.0;
        let start = capsules(std::slice::from_ref(&w), &[pos], &[anim]);
        anim.since = 0.62; // exactly one turn: the same orientation again
        let end = capsules(std::slice::from_ref(&w), &[pos], &[anim]);
        let head_start = start[3].a; // the head capsule, after the three body ones
        let head_end = end[3].a;
        assert!((head_start - head_end).length() < 0.02, "full flip returns: {head_start:?} vs {head_end:?}");

        anim.since = 0.31; // halfway: head below the feet
        let middle = capsules(std::slice::from_ref(&w), &[pos], &[anim]);
        assert!((head_start - middle[3].a).length() > 0.3, "halfway is visibly flipped");
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
        let tallest = squashed.iter().map(|c| c.a.y.max(c.b.y)).fold(f32::MIN, f32::max);
        anim.since = 0.5; // the squash is over
        let normal = capsules(std::slice::from_ref(&w), &[pos], &[anim]);
        let normal_tallest = normal.iter().map(|c| c.a.y.max(c.b.y)).fold(f32::MIN, f32::max);
        assert!(tallest < normal_tallest, "squash is shorter: {tallest} vs {normal_tallest}");
    }
}
