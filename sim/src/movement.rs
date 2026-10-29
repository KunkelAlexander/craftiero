//! Momentum movement for the worm.
//!
//! The tuning numbers are written in a reference frame of "units" and 30 Hz
//! "frames": one unit is a 160th of the worm's height, so the whole rig scales
//! with `WORM_HEIGHT`. `V` and `ACC` convert them to metres per second at the
//! edge of the module; the simulation itself runs at 60 Hz, so a tick is half
//! a frame.
//!
//! The model:
//!   * the body faces the direction it runs (`forward_vel` is a scalar), so
//!     steering turns the worm instead of strafing;
//!   * one mid-air jump (the double jump); there is no chain and no triple;
//!   * a jump adds height and nothing else: horizontal speed is untouched, and
//!     the air cannot push you past running speed, so hopping is never a
//!     faster way to travel than running;
//!   * a jump cut is gravity, not a velocity clamp;
//!   * input is forgiving: a jump a few ticks after leaving a ledge still
//!     counts (coyote), and a press just before touchdown is buffered;
//!   * small ledges are mantled (up to 0.9 m), steep crater walls are walked up
//!     to about 70 degrees, and a jump that reaches a higher lip grabs it and
//!     pulls the worm over, so blast holes do not trap the worm;
//!   * walls are climbed by holding into them, and kicked off with jump;
//!   * pushing the stick from a standstill starts with a dash: a fixed-length
//!     burst in the stick direction (the body snaps to it, so a start in any
//!     direction skips the turn). It has no button of its own.
//!
//! The ramp, brake and turn rates are tuned for a 1.8 m character close to the
//! camera, not a slow, heavy one: the worm reacts on the same tick as the
//! stick. Jump heights, gravity and wall kicks keep the classic platformer
//! shape.
//!
//! The mouse still aims: `yaw`/`pitch` are the camera and the gun, `face_yaw`
//! is the body. Aim and facing are separate on purpose.

use crate::world::{
    body_fits, push_out_of_rock, settle_on_ground, slide, try_ledge_mantle, try_step_up, Action, Worm,
};
use crate::{Input, Rope, DT, WORM_HEIGHT, WORM_RADIUS};
use glam::Vec3;
use sdf::Grid;
use std::f32::consts::{PI, TAU};

/// Metres per reference unit (the worm stands 160 units tall).
const U: f32 = WORM_HEIGHT / 160.0;
/// The reference frame the tuning numbers are written in: 30 frames a second.
const FRAME: f32 = 1.0 / 30.0;
/// Metres per second per (unit per frame).
const V: f32 = U / FRAME;
/// Metres per second squared per (unit per frame squared).
const ACC: f32 = V / FRAME;
/// One 60 Hz tick is half a reference frame.
const HALF: f32 = 30.0 * DT;

// --- walking ---------------------------------------------------------------
/// Top running speed: 35 units/frame (11.8 m/s). The body is a featureless
/// capsule with no run cycle to sell the speed, so it runs a little quicker
/// than its proportions suggest.
const WALK_MAX_U: f32 = 35.0;
/// The speed ramp adds `1.2 - v/40` units per frame: an exponential approach
/// with a 40-frame (1.3 s) time constant. That is tuned for a character much
/// smaller on screen than the worm, so the curve runs four times faster here:
/// the first tick already bites and top speed arrives in about 0.4 s.
const GROUND_ACCEL_U: f32 = 4.8;
const GROUND_ACCEL_TAU: f32 = 10.0;
/// Letting go of the keys stops in ~0.2 s. A slow 1 unit/frame bleed is a
/// full second of ice, and that slide is most of what made the old worm feel
/// heavy.
const BRAKE: f32 = 58.0;
/// The stick held opposite the run brakes harder still: the skid.
const SKID_BRAKE: f32 = 82.0;
/// Speed beyond what the stick asks for (a landing from a rope swing, a rocket
/// jump) bleeds off gently on flat ground, so momentum still means something.
const OVERSPEED_BLEED: f32 = 1.0 * V * HALF;
/// Body turn rate: a little over two turns a second. The old 337 deg/s arc
/// was far too wide at this scale; about-facing now takes about 0.24 s.
const TURN_RATE: f32 = 750.0 * PI / 180.0;

// --- airborne --------------------------------------------------------------
/// Gravity and terminal velocity, in units per frame squared / per frame.
const GRAVITY: f32 = 4.2 * ACC;
const TERMINAL: f32 = 70.0 * V;
/// Air drag, forward push and sideways drift, in units per frame. Drag is
/// light and the push stops at the running speed (see `step_air`), so holding
/// forward in the air keeps the speed you left the ground with.
const AIR_DRAG_U: f32 = 0.3;
const AIR_ACCEL_U: f32 = 2.0;
const AIR_SIDE_U: f32 = 9.0;
/// A jump gets 22% of the forward speed as extra height.
const JUMP_SPEED_BONUS: f32 = 0.22;
/// Letting go while rising faster than this strengthens gravity until the
/// speed is at most 21 units per frame (~7.1 m/s).
const JUMP_CUT: f32 = 21.0 * V;
/// Initial vertical speeds: 43 for the ground jump, 53 for the one mid-air
/// jump.
const JUMP_U: f32 = 43.0;
const DOUBLE_JUMP_U: f32 = 53.0;
const WALL_KICK_U: f32 = 64.0;
/// A jump pressed this close to leaving the ground still counts (0.1 s). Walking
/// off a ledge and pressing a tick or two late is a jump, not a fall.
const COYOTE: u8 = 6;
/// A jump pressed this close to landing is remembered and fires on touchdown
/// (0.1 s), instead of being swallowed by the last few centimetres of the fall.
const JUMP_BUFFER: u8 = 6;
/// A jump that reaches a ledge grabs it: the lip can be this far above the feet
/// when the body hits it. The grab pulls the worm up and over instead of
/// bouncing it back into the hole (called the ledge grab in the code; it reuses
/// the walk's mantle).
const LEDGE_UP: f32 = 1.2;
/// How long a wall kick is still legal after touching a wall: the hit tick
/// plus a short grace, 8 ticks at 60 Hz.
const WALL_WINDOW: u8 = 8;
/// After a kick, do not grab the same wall again immediately.
const CLIMB_REGRAB: u8 = 15;

// --- dash ------------------------------------------------------------------
/// Dash speed: 60 units/frame (20 m/s), about 1.7x the running speed. It is
/// constant for the whole burst, so the distance is predictable: 12 ticks
/// (0.2 s) cover about 4 m.
const DASH_SPEED_U: f32 = 60.0;
const DASH_TICKS: u8 = 12;
/// "At rest" for a dash: slower than this (3 units/frame, about 1 m/s).
const DASH_REST_U: f32 = 3.0;
/// Ticks from one dash start to the next allowed one (0.5 s). Without it,
/// tapping the stick (dash, brake, dash...) would out-run plain running.
const DASH_COOLDOWN: u8 = 30;

// --- climbing --------------------------------------------------------------
/// Free climbing is this game's own move, not a ledge grab. These are tuned,
/// not derived: slower than running, so the terrain still matters.
const CLIMB_UP: f32 = 4.0;
const CLIMB_DOWN: f32 = 6.0;
const CLIMB_SIDE: f32 = 2.6;
/// How far past the body radius to look for the wall that is being climbed.
const CLIMB_REACH: f32 = 0.35;

// --- rope ------------------------------------------------------------------
// The rope is a pendulum with its own model. It is fast because it is the one
// move that launches the worm.
const ROPE_RANGE: f32 = 30.0;
const ROPE_MIN: f32 = 1.0;
const REEL_SPEED: f32 = 12.0;
const REEL_ACCEL: f32 = 45.0;
const REEL_MAX_SPEED: f32 = 20.0;
const SWING_ACCEL: f32 = 30.0;
const SWING_MAX_SPEED: f32 = 24.0;
const ROPE_JUMP: f32 = 7.0;
const ROPE_GRAVITY: f32 = 25.0;
/// The anchor sits on the surface (distance ~0); air this far from it means
/// the rock was dug away.
const ANCHOR_GONE: f32 = 0.3;
const MAX_SPEED: f32 = 40.0;

fn flat(v: Vec3) -> Vec3 {
    Vec3::new(v.x, 0.0, v.z)
}

fn facing(yaw: f32) -> Vec3 {
    Vec3::new(yaw.sin(), 0.0, yaw.cos())
}

fn yaw_of(v: Vec3) -> f32 {
    v.x.atan2(v.z)
}

/// Shortest signed angle from `a` to `b`.
fn wrap(a: f32) -> f32 {
    (a + PI).rem_euclid(TAU) - PI
}

fn turn_toward(current: f32, target: f32, max_step: f32) -> f32 {
    current + wrap(target - current).clamp(-max_step, max_step)
}

fn approach(x: f32, target: f32, step: f32) -> f32 {
    if x < target {
        (x + step).min(target)
    } else {
        (x - step).max(target)
    }
}

/// One tick of movement. The caller (`world::step_worm`) does the digging
/// afterwards, so the climb can never fight a dig. `gravity` multiplies the
/// pull (1.0 normally; a Gravity zone changes it). The jump speeds do not
/// change, so a lighter pull is a higher jump and a heavier one a lower jump.
pub(crate) fn step(grid: &Grid, worm: &mut Worm, input: &Input, gravity: f32) {
    worm.yaw = input.yaw;
    worm.pitch = input.pitch;
    // The viewer's camera can be off the look ray (over the shoulder), in which
    // case it supplies the direction to the crosshair. Keep it as an override
    // only while the input carries one, so look angles stay in charge otherwise.
    worm.aim = input
        .aim
        .filter(|a| a.length_squared() > 1.0e-8)
        .map_or(Vec3::ZERO, |a| a.normalize());

    let jump_pressed = input.jump && !worm.jump_was_down;
    worm.jump_was_down = input.jump;
    if jump_pressed {
        worm.jump_buffer = JUMP_BUFFER;
    } else {
        worm.jump_buffer = worm.jump_buffer.saturating_sub(1);
    }
    // Coyote time: keep a jump available for a few ticks after the ground
    // disappears. `on_ground` is from the end of the last tick, so a worm that
    // walked off a ledge this tick still has the full window.
    if worm.on_ground {
        worm.coyote = COYOTE;
    } else {
        worm.coyote = worm.coyote.saturating_sub(1);
    }
    let mut jump_used = false;

    worm.wall_timer = worm.wall_timer.saturating_sub(1);
    worm.climb_cooldown = worm.climb_cooldown.saturating_sub(1);
    worm.tumble_timer = worm.tumble_timer.saturating_sub(1);

    worm.dash_cooldown = worm.dash_cooldown.saturating_sub(1);

    // Rope button: a press shoots it, or lets go if it is out.
    let rope_pressed = input.rope && !worm.rope_was_down;
    worm.rope_was_down = input.rope;
    if rope_pressed {
        if worm.rope.is_some() {
            detach_rope(worm);
        } else if let Some(t) = grid.raycast(worm.eye(), worm.aim_dir(), ROPE_RANGE) {
            let anchor = worm.eye() + worm.aim_dir() * t;
            worm.rope = Some(Rope {
                anchor,
                length: (anchor - worm.hand()).length().max(ROPE_MIN),
            });
        }
    }
    // The rock the rope hangs from can be dug away.
    if let Some(rope) = worm.rope {
        if grid.sample(rope.anchor) > ANCHOR_GONE {
            detach_rope(worm);
        }
    }

    if worm.rope.is_some() {
        if jump_pressed {
            // Rope kick: let go and launch upward.
            worm.rope = None;
            worm.vel.y += ROPE_JUMP;
            align_face_to_velocity(worm);
            worm.action = Action::Freefall;
            worm.jump_buffer = 0;
            worm.coyote = 0;
            jump_used = true;
        } else {
            step_rope(grid, worm, input, gravity);
            return;
        }
    }

    // Camera-relative wish. `move_dir.y` is forward, `.x` is right.
    let (sy, cy) = worm.yaw.sin_cos();
    let forward = Vec3::new(sy, 0.0, cy);
    let right = Vec3::new(-cy, 0.0, sy);
    let wish = forward * input.move_dir.y + right * input.move_dir.x;
    let mag = input.move_dir.length().min(1.0);
    let intended_yaw = if mag > 0.01 {
        worm.yaw + (-input.move_dir.x).atan2(input.move_dir.y)
    } else {
        worm.face_yaw
    };
    // A dash needs a fresh push of the stick, not a held one (Pattern: edge
    // detection, the same trick as the jump and rope buttons).
    let stick_pressed = mag > 0.01 && !worm.stick_was_down;
    worm.stick_was_down = mag > 0.01;

    if worm.action == Action::Climb {
        step_climb(grid, worm, input, jump_pressed);
        return;
    }

    // A dash in progress owns the body until it ends. Jump cancels it (at run
    // speed, so a dash-jump is not a speed boost) and falls through to the
    // normal jump rules below.
    if worm.action == Action::Dash {
        if jump_pressed {
            end_dash(worm);
        } else {
            step_dash(grid, worm);
            return;
        }
    } else if stick_pressed && can_dash(worm, input) {
        begin_dash(worm, intended_yaw);
        step_dash(grid, worm);
        return;
    }

    // A buffered press can fire here (a coyote jump) or, if this tick is the
    // landing, in `land`. Only a fresh press can kick off a wall or use the
    // mid-air jump; the buffer exists for the ground.
    if !jump_used {
        if worm.jump_buffer > 0 && (worm.on_ground || worm.coyote > 0) {
            ground_jump(worm);
            jump_used = true;
        } else if jump_pressed && worm.wall_timer > 0 {
            wall_kick(worm);
            jump_used = true;
        } else if jump_pressed && can_air_jump(worm) {
            double_jump(worm);
            jump_used = true;
        }
    }

    if worm.on_ground {
        step_ground(grid, worm, input, wish, mag, intended_yaw);
    } else {
        step_air(grid, worm, input, wish, mag, intended_yaw, jump_pressed && !jump_used, gravity);
    }
}

// --------------------------------------------------------------- ground ----

fn step_ground(
    grid: &Grid,
    worm: &mut Worm,
    input: &Input,
    wish: Vec3,
    mag: f32,
    intended_yaw: f32,
) {
    if mag > 0.01 {
        worm.face_yaw = turn_toward(worm.face_yaw, intended_yaw, TURN_RATE * DT);
    }

    let flat_wish = flat(wish);
    // How far off the body the stick points: 1 is straight ahead, -1 behind.
    let alignment = flat_wish.normalize_or_zero().dot(facing(worm.face_yaw));

    if mag < 0.01 {
        // No stick: stop, fast.
        worm.forward_vel = approach(worm.forward_vel, 0.0, BRAKE * DT);
    } else if alignment < -0.1 {
        // Stick behind the body (S, or a sharp reversal): skid to a stop
        // first, then run the new way. With the fast turn this is over in a
        // few ticks, which is what makes a reversal feel like a decision
        // rather than a lorry turning round.
        worm.forward_vel = approach(worm.forward_vel, 0.0, SKID_BRAKE * DT);
    } else {
        // The speed ramp, four times faster than the reference frame.
        let target = (mag * WALK_MAX_U).min(WALK_MAX_U) * V;
        if worm.forward_vel < target {
            let v_u = worm.forward_vel / V;
            let bite = (GROUND_ACCEL_U - v_u / GROUND_ACCEL_TAU).max(0.0);
            worm.forward_vel = (worm.forward_vel + bite * V * HALF).min(target);
        } else if worm.ground_normal.y >= 0.95 {
            worm.forward_vel = (worm.forward_vel - OVERSPEED_BLEED).max(target);
        }
        worm.forward_vel = worm.forward_vel.min(48.0 * V);
    }

    let f = facing(worm.face_yaw);
    // Walk along the ground plane, not into it. On a slope or a bump this keeps
    // the scalar speed; without it every bump eats a mouthful of speed.
    let n = worm.ground_normal;
    let mut dir = f - n * f.dot(n);
    dir = if dir.length_squared() < 1.0e-6 { f } else { dir.normalize() };
    worm.vel = dir * worm.forward_vel;

    let start = worm.pos;
    let disp = worm.vel * DT;
    let mut hit = slide(grid, worm, disp);
    // Walking into a ledge or a crater lip: try a mantle from STEP_UP higher.
    if worm.forward_vel.abs() > 0.1 {
        let want = f * (worm.forward_vel * DT);
        let stepped = try_step_up(grid, worm, start, flat(disp).length(), want);
        hit.ground |= stepped.ground;
        if stepped.wall.is_some() {
            hit.wall = stepped.wall;
        }
    }

    let settled = settle_on_ground(grid, worm, true);
    worm.on_ground = hit.ground || settled;
    if worm.on_ground {
        worm.vel.y = 0.0;
        worm.action = if worm.forward_vel.abs() > 0.2 { Action::Walk } else { Action::Idle };
    } else {
        // Walked off an edge: fall, but keep the speed. The air jump is still
        // available, so a fall can be recovered.
        worm.action = Action::Freefall;
        worm.ground_normal = Vec3::Y;
        worm.vel.y = 0.0;
    }

    // Holding into a tall wall starts a climb. Digging is excluded: while
    // digging the worm must be able to tunnel into the wall instead.
    if !input.dig && mag > 0.5 && worm.climb_cooldown == 0 {
        if let Some(n) = hit.wall {
            if flat_wish.normalize_or_zero().dot(-n) > 0.45 && wall_above(grid, worm, n) {
                begin_climb(worm, n);
            }
        }
    }
    clamp_speed(worm);
}

// ------------------------------------------------------------------ air ----

fn step_air(
    grid: &Grid,
    worm: &mut Worm,
    input: &Input,
    wish: Vec3,
    mag: f32,
    intended_yaw: f32,
    jump_pressed: bool,
    gravity: f32,
) {
    let before_move = worm.vel;

    if worm.action == Action::Tumble {
        if worm.tumble_timer == 0 {
            worm.action = Action::Freefall;
        }
        worm.vel.y -= GRAVITY * gravity * DT;
    } else {
        // A worm that was placed in the air (a respawn, a teleport, a rope
        // release that did not align) keeps the momentum it has.
        if worm.forward_vel.abs() < 1.0e-3
            && !matches!(
                worm.action,
                Action::Jump | Action::DoubleJump | Action::WallKick | Action::Climb
            )
        {
            align_face_to_velocity(worm);
        }
        // In the air there is no strafing and no body turn: just a forward
        // push and a sideways drift that is recomputed each frame.
        let mut fwd_u = worm.forward_vel / V;
        fwd_u = approach(fwd_u, 0.0, AIR_DRAG_U * HALF);
        let mut sideways = 0.0;
        if mag > 0.01 {
            let dyaw = wrap(intended_yaw - worm.face_yaw);
            // The air only pushes you up TO the running speed. (It used to
            // subtract a flat 1.0/frame above the cap while the push added
            // more, so holding W in the air accelerated without limit and
            // hopping became the fastest way to travel.)
            let target_u = (mag * WALK_MAX_U).min(WALK_MAX_U);
            if fwd_u < target_u {
                fwd_u = (fwd_u + AIR_ACCEL_U * dyaw.cos() * mag * HALF).min(target_u);
            }
            sideways = AIR_SIDE_U * dyaw.sin() * mag * V;
        }
        if fwd_u > WALK_MAX_U {
            // Above the cap (a rope or rocket launch): bleed back down.
            fwd_u = (fwd_u - 1.0 * HALF).max(WALK_MAX_U);
        }
        if fwd_u < -16.0 {
            fwd_u += 2.0 * HALF;
        }
        worm.forward_vel = fwd_u * V;

        let f = facing(worm.face_yaw);
        // 90 degrees clockwise from the facing: the direction `move_dir.x`
        // adds drift in (the sign is already in `sideways`).
        let side = Vec3::new(f.z, 0.0, -f.x);
        let drift = side * sideways;
        worm.vel.x = f.x * worm.forward_vel + drift.x;
        worm.vel.z = f.z * worm.forward_vel + drift.z;
        // Fairness rule: the air never makes you faster than running, and
        // never faster than you already were. Drift and air control come out
        // of that budget instead of being added on top, so a jump is not a
        // speed boost (see also `ground_jump`, which changes no horizontal
        // speed at all).
        let cap = (WALK_MAX_U * V).max(flat(before_move).length());
        let horiz = Vec3::new(worm.vel.x, 0.0, worm.vel.z);
        if horiz.length() > cap {
            let capped = horiz.normalize_or_zero() * cap;
            worm.vel.x = capped.x;
            worm.vel.z = capped.z;
            worm.forward_vel = capped.dot(f);
        }
        worm.vel.y -= GRAVITY * gravity * DT;

        // Jump cut: release the button while rising and the rest of the rise
        // is capped at `JUMP_CUT`.
        if matches!(worm.action, Action::Jump | Action::DoubleJump)
            && !input.jump
            && worm.vel.y > JUMP_CUT
        {
            worm.vel.y = JUMP_CUT;
        }
    }
    if worm.vel.y < -TERMINAL {
        worm.vel.y = -TERMINAL;
    }

    let start = worm.pos;
    let horiz = flat(worm.vel);
    let hit = slide(grid, worm, worm.vel * DT);

    // A wall we were moving into: remember it for the wall-kick window.
    if let Some(n) = hit.wall {
        let into = flat(before_move).dot(-n);
        if into > 0.5 {
            worm.wall_normal = n;
            worm.wall_timer = WALL_WINDOW;
            // Cancel the push into the wall, keep the slide along it.
            worm.forward_vel = flat(worm.vel).dot(facing(worm.face_yaw));
            if jump_pressed {
                wall_kick(worm);
                return;
            }
        }
    }

    // Ledge grab: the jump reached a lip but the body hit its face. If the
    // player is still pushing into it (or the jump carried the worm into it),
    // pull the worm up and over instead of cancelling its speed and dropping it
    // back into the crater.
    if !matches!(worm.action, Action::Tumble | Action::WallKick) {
        if let Some(n) = hit.wall {
            let push = flat(wish).normalize_or_zero().dot(-n);
            let carried = flat(horiz).dot(-n);
            if push > 0.3 || carried > 0.5 {
                let dir = if push > 0.3 { flat(wish).normalize_or_zero() } else { flat(horiz).normalize_or_zero() };
                let forward = dir * DT;
                if try_ledge_mantle(grid, worm, start, forward, LEDGE_UP, horiz, 0.2).is_some() {
                    // The grab itself is the landing; do not chain the jump
                    // press that got us here into a jump off the new ledge.
                    worm.jump_buffer = 0;
                    land(worm);
                    return;
                }
            }
        }
    }

    // Holding toward a tall wall grabs it, exactly like walking into one.
    if !input.dig && mag > 0.3 && worm.climb_cooldown == 0 {
        if let Some(n) = hit.wall {
            if flat(wish).normalize_or_zero().dot(-n) > 0.45 && wall_above(grid, worm, n) {
                begin_climb(worm, n);
                return;
            }
        }
    }

    let settled = settle_on_ground(grid, worm, false);
    // Land on a standing contact. `settle_on_ground` has already rejected a
    // body that is flying away from the surface, so this catches the case that
    // used to break the state machine: running (or being knocked) into a slope
    // turns horizontal speed into upward speed, and the worm then rides the
    // slope without ever landing. It was not on the ground, so it had no ground
    // jump, and the slow air drag let it slide for ages. `hit.ground` is the
    // coarser "touched a floor-like surface" flag, which a fast fall can reach
    // for one tick before the settle skin does; that one only counts if the
    // worm was not already rising when the tick began (a real jump or launch).
    if settled || (hit.ground && before_move.y <= 0.5) {
        land(worm);
        return;
    }

    worm.on_ground = false;
    worm.ground_normal = Vec3::Y;
    clamp_speed(worm);
}

/// Land from any airborne action. The single air jump is available again as
/// soon as the worm touches the ground.
fn land(worm: &mut Worm) {
    let from = worm.action;
    let flat_vel = flat(worm.vel);
    // A tumble or a rope can arrive sideways; point the body the way it is
    // travelling so the momentum is not thrown away.
    if matches!(from, Action::Tumble | Action::Rope | Action::Freefall | Action::WallKick)
        && flat_vel.length() > 0.2
    {
        worm.face_yaw = yaw_of(flat_vel);
    }
    worm.forward_vel = flat_vel.dot(facing(worm.face_yaw));
    worm.vel = facing(worm.face_yaw) * worm.forward_vel;
    worm.on_ground = true;
    worm.ground_normal = Vec3::Y;
    worm.action = if worm.forward_vel.abs() > 0.2 { Action::Walk } else { Action::Idle };
    worm.air_jumps = 1;

    // A jump pressed just before touchdown was remembered by the buffer; this
    // is the frame it gets to fire.
    if worm.jump_buffer > 0 && from != Action::Tumble {
        ground_jump(worm);
    }
}

/// The ground jump. It changes vertical speed and nothing else: scaling the
/// forward speed down here would make the jump slower, and landing would then
/// have to accelerate it back, which reads as a burst of speed just after
/// touching down. Jumping must be a fair way to move, not a fast one.
fn ground_jump(worm: &mut Worm) {
    let f = facing(worm.face_yaw);
    worm.vel = Vec3::new(
        f.x * worm.forward_vel,
        JUMP_U * V + JUMP_SPEED_BONUS * worm.forward_vel,
        f.z * worm.forward_vel,
    );
    worm.action = Action::Jump;
    worm.on_ground = false;
    worm.wall_timer = 0;
    worm.air_jumps = 1;
    worm.coyote = 0;
    worm.jump_buffer = 0;
}

fn can_air_jump(worm: &Worm) -> bool {
    worm.air_jumps > 0 && worm.action != Action::Tumble
}

/// The one mid-air jump. Like the ground jump it only adds height; the
/// horizontal speed is untouched, so a double jump is never a speed boost.
fn double_jump(worm: &mut Worm) {
    let f = facing(worm.face_yaw);
    worm.vel = Vec3::new(
        f.x * worm.forward_vel,
        DOUBLE_JUMP_U * V + JUMP_SPEED_BONUS * worm.forward_vel,
        f.z * worm.forward_vel,
    );
    worm.action = Action::DoubleJump;
    worm.air_jumps = 0;
    worm.on_ground = false;
    worm.wall_timer = 0;
    worm.coyote = 0;
    worm.jump_buffer = 0;
}

// ---------------------------------------------------------------- dash ----

/// A dash starts from a standstill on the ground. Digging is excluded for the
/// same reason it is for climbing: the worm has to creep into the hole it is
/// making, not burst past it.
fn can_dash(worm: &Worm, input: &Input) -> bool {
    worm.on_ground
        && worm.dash_cooldown == 0
        && matches!(worm.action, Action::Idle | Action::Walk)
        && worm.forward_vel.abs() < DASH_REST_U * V
        && !input.dig
}

/// The body snaps to the dash direction at once, so starting in any direction
/// skips the 0.24 s turn.
fn begin_dash(worm: &mut Worm, yaw: f32) {
    worm.face_yaw = yaw;
    worm.action = Action::Dash;
    worm.dash_timer = DASH_TICKS;
    worm.dash_cooldown = DASH_COOLDOWN;
    worm.wall_timer = 0;
}

/// One tick of the burst: a fixed velocity along the dash direction, no
/// steering. It walks like `step_ground` (along the ground plane, up small
/// steps), but it ends if the worm leaves the ground or meets a wall.
fn step_dash(grid: &Grid, worm: &mut Worm) {
    worm.dash_timer = worm.dash_timer.saturating_sub(1);
    let f = facing(worm.face_yaw);
    let speed = DASH_SPEED_U * V;
    worm.forward_vel = speed;
    // Along the ground plane, like walking, so a bump does not eat speed.
    let n = worm.ground_normal;
    let dir = f - n * f.dot(n);
    worm.vel = (if dir.length_squared() < 1.0e-6 { f } else { dir.normalize() }) * speed;

    let start = worm.pos;
    let disp = worm.vel * DT;
    let mut hit = slide(grid, worm, disp);
    let stepped = try_step_up(grid, worm, start, flat(disp).length(), f * (speed * DT));
    hit.ground |= stepped.ground;
    if stepped.wall.is_some() {
        hit.wall = stepped.wall;
    }
    let settled = settle_on_ground(grid, worm, true);
    worm.on_ground = hit.ground || settled;
    if !worm.on_ground {
        worm.ground_normal = Vec3::Y;
        worm.vel.y = 0.0;
    }

    // A wall stops the burst (pushing on would only scrape), and so does the
    // edge of a ledge: the dash is a start, not a jump.
    let blocked = hit.wall.is_some_and(|n| f.dot(-n) > 0.5);
    if blocked || !worm.on_ground || worm.dash_timer == 0 {
        end_dash(worm);
    }
    clamp_speed(worm);
}

/// Back to normal movement at running speed: the dash is a burst, not a
/// lasting speed-up (the same fairness rule as the jumps).
fn end_dash(worm: &mut Worm) {
    worm.dash_timer = 0;
    worm.forward_vel = worm.forward_vel.min(WALK_MAX_U * V);
    let f = facing(worm.face_yaw);
    worm.vel.x = f.x * worm.forward_vel;
    worm.vel.z = f.z * worm.forward_vel;
    worm.action = if !worm.on_ground {
        Action::Freefall
    } else if worm.forward_vel.abs() > 0.2 {
        Action::Walk
    } else {
        Action::Idle
    };
}

fn wall_kick(worm: &mut Worm) {
    let n = flat(worm.wall_normal).normalize_or_zero();
    // Failsafe: if the normal was lost, kick back the way we came.
    let n = if n.length_squared() < 1.0e-6 { -facing(worm.face_yaw) } else { n };
    worm.face_yaw = yaw_of(-n);
    worm.forward_vel = worm.forward_vel.abs().max(24.0 * V);
    worm.vel = facing(worm.face_yaw) * worm.forward_vel + Vec3::Y * (WALL_KICK_U * V);
    worm.pos += n * 0.06;
    worm.action = Action::WallKick;
    worm.on_ground = false;
    worm.wall_timer = 0;
    worm.climb_cooldown = CLIMB_REGRAB;
    // A kick is a launch: it arms the air jump again.
    worm.air_jumps = 1;
    worm.coyote = 0;
    worm.jump_buffer = 0;
}

// --------------------------------------------------------------- climb ----

/// Finds a mostly vertical wall next to the body. The gradient points out of
/// the rock, so the outward normal is exactly what a kick needs.
fn wall_probe(grid: &Grid, worm: &Worm) -> Option<Vec3> {
    let chest = worm.pos + Vec3::Y * (WORM_HEIGHT * 0.55);
    if grid.sample(chest) > WORM_RADIUS + CLIMB_REACH {
        return None;
    }
    let n = grid.gradient(chest);
    let n = flat(n);
    if n.length_squared() < 0.09 {
        return None;
    }
    let n = n.normalize();
    // Ignore floors, ceilings and overhangs.
    if grid.gradient(chest).y.abs() > 0.6 {
        return None;
    }
    Some(n)
}

/// Is there still wall at head height? A low ledge must be jumped onto, not
/// climbed; a cliff has rock all the way up.
fn wall_above(grid: &Grid, worm: &Worm, n: Vec3) -> bool {
    let head = worm.pos + Vec3::Y * (WORM_HEIGHT - 0.15);
    grid.raycast(head, -n, WORM_RADIUS + 0.3).is_some()
}

fn begin_climb(worm: &mut Worm, n: Vec3) {
    worm.action = Action::Climb;
    worm.on_ground = false;
    worm.vel = Vec3::ZERO;
    worm.forward_vel = 0.0;
    worm.wall_normal = n;
    worm.face_yaw = yaw_of(-n);
    worm.wall_timer = 0;
    worm.air_jumps = 1;
}

fn step_climb(grid: &Grid, worm: &mut Worm, input: &Input, jump_pressed: bool) {
    if jump_pressed {
        wall_kick(worm);
        return;
    }

    let n = wall_probe(grid, worm).unwrap_or(worm.wall_normal);
    worm.wall_normal = n;

    // W/S climb up and down; A/D slide along the wall in camera-right.
    let cam_right = Vec3::new(-worm.yaw.cos(), 0.0, worm.yaw.sin());
    let tangent = flat(cam_right - n * cam_right.dot(n)).normalize_or_zero();
    let up = input.move_dir.y;
    let vertical = if up > 0.1 {
        CLIMB_UP * up.min(1.0)
    } else if up < -0.1 {
        -CLIMB_DOWN * (-up).min(1.0)
    } else {
        0.0
    };
    worm.vel = Vec3::Y * vertical + tangent * (CLIMB_SIDE * input.move_dir.x);
    worm.forward_vel = 0.0;

    let before = worm.pos.y;
    let hit = slide(grid, worm, worm.vel * DT);
    if let Some(n) = hit.wall {
        worm.wall_normal = n;
    }

    // Feet have reached the top: pull up onto it.
    if (vertical > 0.0 || worm.pos.y > before) && try_mantle(grid, worm, worm.wall_normal) {
        return;
    }

    // Climbing down reaches the floor.
    if vertical < 0.0 && settle_on_ground(grid, worm, false) {
        worm.on_ground = true;
        worm.action = Action::Idle;
        worm.air_jumps = 1;
        worm.vel = Vec3::ZERO;
        return;
    }

    // The wall ended (or became a ceiling): let go.
    if wall_probe(grid, worm).is_none() {
        if try_mantle(grid, worm, worm.wall_normal) {
            return;
        }
        worm.action = Action::Freefall;
        worm.vel = Vec3::ZERO;
    }
    worm.on_ground = false;
    clamp_speed(worm);
}

/// Move from hanging on the lip to standing on top when the feet are level
/// with it. The probe goes into the wall (not along it) and then straight
/// down, so it finds the top surface, not the face.
fn try_mantle(grid: &Grid, worm: &mut Worm, n: Vec3) -> bool {
    let into = -flat(n).normalize_or_zero();
    if into.length_squared() < 1.0e-6 {
        return false;
    }
    let probe = worm.pos + Vec3::Y * 0.25 + into * (WORM_RADIUS + 0.3);
    let from = probe + Vec3::Y * 0.35;
    let Some(t) = grid.raycast(from, -Vec3::Y, 1.0) else {
        return false;
    };
    let floor = from.y - t;
    // Only mantle once the feet are at the top, not teleport up the face.
    if floor < worm.pos.y - 0.1 {
        return false;
    }
    let feet = Vec3::new(probe.x, floor + 0.02, probe.z);
    if !body_fits(grid, feet) {
        return false;
    }
    worm.pos = feet;
    worm.vel = Vec3::ZERO;
    worm.forward_vel = 0.0;
    worm.on_ground = true;
    worm.ground_normal = Vec3::Y;
    worm.action = Action::Idle;
    worm.air_jumps = 1;
    true
}

// ---------------------------------------------------------------- rope ----

fn step_rope(grid: &Grid, worm: &mut Worm, input: &Input, gravity: f32) {
    let hand = worm.hand();
    if let Some(rope) = &mut worm.rope {
        let base = if input.reel > 0.0 {
            rope.length.min((rope.anchor - hand).length())
        } else {
            rope.length
        };
        rope.length = (base - input.reel * REEL_SPEED * DT).clamp(ROPE_MIN, ROPE_RANGE);
    }

    let (sy, cy) = worm.yaw.sin_cos();
    let forward = Vec3::new(sy, 0.0, cy);
    let right = Vec3::new(-cy, 0.0, sy);
    let wish = forward * input.move_dir.y + right * input.move_dir.x;
    let mut vel = worm.vel;

    // Pumping the swing: stronger than air control, capped higher.
    let w = wish.length();
    if w > 1.0e-4 {
        let dir = wish / w;
        let flat_vel = flat(vel);
        let add = (SWING_MAX_SPEED * w - flat_vel.dot(dir)).clamp(0.0, SWING_ACCEL * DT);
        vel += dir * add;
    }
    vel.y -= ROPE_GRAVITY * gravity * DT;

    if let (Some(rope), true) = (worm.rope, input.reel > 0.0) {
        let to_anchor = rope.anchor - hand;
        let dist = to_anchor.length();
        if dist > 1.0e-4 {
            let dir = to_anchor / dist;
            let room = (REEL_MAX_SPEED - vel.dot(dir)).max(0.0);
            vel += dir * (REEL_ACCEL * input.reel * DT).min(room);
        }
    }
    worm.vel = vel.clamp_length_max(MAX_SPEED);

    let hit = slide(grid, worm, worm.vel * DT);

    // A taut rope is a hard distance limit; what is left is sideways speed.
    if let Some(rope) = worm.rope {
        let to_anchor = rope.anchor - worm.hand();
        let dist = to_anchor.length();
        if dist > rope.length && dist > 1.0e-4 {
            let dir = to_anchor / dist;
            worm.pos += dir * (dist - rope.length);
            worm.vel -= dir * worm.vel.dot(dir).min(0.0);
            push_out_of_rock(grid, worm, false);
        }
    }

    let settled = settle_on_ground(grid, worm, false);
    worm.on_ground = hit.ground || settled;
    worm.action = Action::Rope;
    worm.forward_vel = flat(worm.vel).length();
    clamp_speed(worm);
}

fn detach_rope(worm: &mut Worm) {
    worm.rope = None;
    align_face_to_velocity(worm);
    if worm.action == Action::Rope {
        worm.action = Action::Freefall;
    }
}

fn align_face_to_velocity(worm: &mut Worm) {
    let flat_vel = flat(worm.vel);
    if flat_vel.length() > 0.2 {
        worm.face_yaw = yaw_of(flat_vel);
        worm.forward_vel = flat_vel.length();
    } else {
        worm.forward_vel = 0.0;
    }
}

fn clamp_speed(worm: &mut Worm) {
    worm.vel = worm.vel.clamp_length_max(MAX_SPEED);
}
