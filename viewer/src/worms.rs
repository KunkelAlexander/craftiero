// Turns the sim's worms into the capsules the renderer draws. The sim knows
// nothing about colours or how a worm looks; this is the only place that does.

use crate::renderer::DrawCapsule;
use glam::Vec3;
use sim::{Worm, WORM_HEIGHT, WORM_RADIUS};

/// One colour per player (as picked in a paint program).
const COLOURS: [Vec3; 4] = [
    Vec3::new(0.93, 0.42, 0.33), // coral
    Vec3::new(0.33, 0.58, 0.92), // blue
    Vec3::new(0.96, 0.80, 0.30), // yellow
    Vec3::new(0.45, 0.78, 0.40), // green
];
const GUN: Vec3 = Vec3::new(0.22, 0.20, 0.28);
const EYE: Vec3 = Vec3::new(1.0, 0.97, 0.9);
const ROPE: Vec3 = Vec3::new(0.92, 0.86, 0.70);

/// The capsules for `worms`, each drawn at `positions[i]` (the sim's position
/// smoothed between two ticks, see `Viewer::frame`).
pub fn capsules(worms: &[Worm], positions: &[Vec3]) -> Vec<DrawCapsule> {
    let mut out = Vec::new();
    for (i, (worm, &pos)) in worms.iter().zip(positions).enumerate() {
        if !worm.alive {
            continue; // (the death flash is an effect, see `effects.rs`)
        }
        let caps = |a: Vec3, b: Vec3, radius: f32, colour: Vec3, shadow: bool| DrawCapsule { a, b, radius, colour, shadow };
        let aim = worm.aim_dir();
        let (sy, cy) = worm.yaw.sin_cos();
        let right = Vec3::new(-cy, 0.0, sy);

        // Body: the very capsule the sim collides with.
        let foot = pos + Vec3::Y * WORM_RADIUS;
        let head = pos + Vec3::Y * (WORM_HEIGHT - WORM_RADIUS);
        out.push(caps(foot, head, WORM_RADIUS, COLOURS[i % COLOURS.len()], true));

        // Eyes show which way the worm faces (the body is round all the way
        // round). Each is a very short capsule, because a capsule needs a direction.
        for side in [-1.0, 1.0] {
            let eye = head + aim * 0.36 + right * (0.16 * side) + Vec3::Y * 0.06;
            out.push(caps(eye, eye + aim * 0.03, 0.11, EYE, false));
        }

        // Gun: shows the aim, pitch included.
        let hand = pos + Vec3::Y * 1.1;
        out.push(caps(hand + aim * 0.3, hand + aim * 1.0, 0.09, GUN, false));

        if let Some(rope) = worm.rope {
            if (rope.anchor - hand).length() > 0.1 {
                out.push(caps(hand, rope.anchor, 0.035, ROPE, false));
            }
        }
    }
    out
}
