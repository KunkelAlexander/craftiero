// The world and the worm: movement against the signed distance field, jumping,
// digging and the ninja rope.
//
// Everything here is a plain struct and a plain function. `World::step` is the
// only way the state changes, and it takes nothing but `Input`s, so a test can
// replay a game from a list of inputs.
//
// A note on determinism (the same inputs giving the same worm): +, -, *, / and
// sqrt on f32 are exact IEEE operations and give identical bits everywhere.
// `sin` and `cos` are not guaranteed to. That is fine today because only the
// host runs the sim and clients draw its snapshots. If a client ever predicts
// its own worm, swap `sin_cos` for a table or an approximation written here.

use crate::input::Input;
use glam::{Vec2, Vec3};
use sdf::{Grid, Op, Shape};

/// The sim runs at a fixed 60 Hz, whatever the frame rate of whoever draws it.
/// A fixed step makes physics repeatable: variable steps change how high you
/// jump and whether you tunnel through a wall.
pub const DT: f32 = 1.0 / 60.0;

// --- the worm's body -----------------------------------------------------
pub const WORM_RADIUS: f32 = 0.45;
pub const WORM_HEIGHT: f32 = 1.8;
/// Eye (and aim origin) above the feet.
pub const EYE_HEIGHT: f32 = 1.5;
/// Height of the hand: where the rope and (later) the gun are.
const HAND_HEIGHT: f32 = 1.1;
/// Pattern: Spatial approximation. The collision shape is a capsule, tested as
/// three spheres up its axis (feet, middle, head). A sphere against a distance
/// field is one lookup: `distance < radius` means touching, and the gradient
/// says which way to push out. The spheres are one radius apart, so they
/// overlap and leave no gap a thin ledge could slip through.
const BODY_SPHERES: [f32; 3] = [WORM_RADIUS, WORM_HEIGHT * 0.5, WORM_HEIGHT - WORM_RADIUS];

// --- movement feel (metres, seconds) --------------------------------------
const WALK_SPEED: f32 = 8.0;
/// How fast the walking speed is reached on the ground. High, so the worm stops
/// and starts crisply and standing still is not a slide.
const GROUND_ACCEL: f32 = 70.0;
/// Slowing down while still steering when faster than walking speed (landing
/// from a swing, a bunny hop). Much gentler than `GROUND_ACCEL`, so momentum
/// survives a landing: let go of the keys and the full friction stops you.
const OVERSPEED_DRAG: f32 = 10.0;
/// Steering in the air. It never BRAKES: the speed gained from a rope swing is
/// kept, steering only adds speed up to the walking speed.
const AIR_ACCEL: f32 = 20.0;
const GRAVITY: f32 = 25.0;
/// Falling is heavier than rising. A symmetric arc feels floaty; a quick drop
/// makes jumps snappy. (Not while on a rope: a swing should be a pendulum.)
const FALL_GRAVITY_MULT: f32 = 1.5;
/// Rise = speed^2 / (2 * gravity) = 2.4 m, well above the worm's own height.
const JUMP_SPEED: f32 = 11.0;
/// Letting go of jump while still rising cuts the speed to this: a tap is a
/// hop of about half a metre, holding gives the full jump.
const JUMP_CUT_SPEED: f32 = 4.0;
/// Pressing jump up to this many ticks (0.1 s) after walking off an edge still
/// jumps ("coyote time"), and pressing it up to this many ticks before landing
/// jumps on landing ("jump buffer"). Both forgive human timing; without them
/// a jump that feels like it was pressed in time is sometimes eaten.
const COYOTE_TICKS: u8 = 6;
const JUMP_BUFFER_TICKS: u8 = 6;
const MAX_SPEED: f32 = 40.0;
/// A surface counts as ground when its normal is within ~41 degrees of up.
/// Steeper is a wall: the worm slides along it instead of standing on it.
/// (Not 45: at a wall-floor crease the foot sphere's blended normal is exactly
/// 45 degrees, and a threshold there makes the answer flip from tick to tick.)
const GROUND_COS: f32 = 0.75;
/// Still "standing" when the feet are this far above the surface, and then the
/// worm is pulled down onto it. It keeps the worm grounded while walking over
/// bumps and down slopes, where each tick would otherwise be a tiny fall.
const GROUND_SKIN: f32 = 0.1;
/// A worm that was walking sticks to ground up to this far below it (crests of
/// bumps, steps down), instead of launching off every crest like a ball.
const STICK_DISTANCE: f32 = 0.3;
/// Highest ledge the worm walks up without jumping.
const STEP_UP: f32 = 0.5;
/// Largest move between collision checks. The distance field is only valid in a
/// 1 m band, and a fast worm could otherwise end a tick deeper in rock than the
/// field can say which way is out.
const MAX_STEP: f32 = 0.2;
/// Largest correction per overlapping sphere and pass: a worm buried deeper
/// than the field's band (where the gradient is zero) is pushed up gradually.
const MAX_PUSH: f32 = 0.5;
const PITCH_LIMIT: f32 = 1.553; // 89 degrees

// --- digging --------------------------------------------------------------
/// How far the worm can reach (from the eye to the rock).
const DIG_REACH: f32 = 4.0;
/// Radius of the hole, centred on the surface point the worm looks at.
///
/// Why this size: a tunnel is a row of overlapping bites, and where two bites
/// meet the roof dips into a cusp. Bites one radius apart leave a cusp of
/// 2 * sqrt(r^2 - (r/2)^2) = 1.73 r, which has to be taller than the worm
/// (1.8 m), or the tunnel pinches it at every joint. 1.5 m gives 2.6 m, which
/// also leaves room to walk over the bumps the bites leave in the floor.
pub const DIG_RADIUS: f32 = 1.5;
/// Ticks between digs while the button is held: 5 a second. Each dig advances
/// about one radius (1.5 m), so digging forward goes at about walking speed.
const DIG_INTERVAL: u8 = 12;

// --- rope -----------------------------------------------------------------
const ROPE_RANGE: f32 = 30.0;
const ROPE_MIN: f32 = 1.0;
/// How fast a reeled rope shortens (m/s).
const REEL_SPEED: f32 = 12.0;
/// Reeling in also ACCELERATES the worm towards the anchor, up to
/// `REEL_MAX_SPEED`. The shortening alone would only drag the worm along at a
/// fixed speed and leave it with nothing when it let go; the acceleration is
/// what launches it out of a hole.
const REEL_ACCEL: f32 = 45.0;
const REEL_MAX_SPEED: f32 = 20.0;
/// Steering on a rope in the air: a stronger push, and allowed to go much
/// faster than walking, so pushing in the direction of the swing pumps it up
/// like a child on a swing.
const SWING_ACCEL: f32 = 30.0;
const SWING_MAX_SPEED: f32 = 24.0;
/// Pressing jump on the rope in the air lets go and kicks the worm upwards.
const ROPE_JUMP: f32 = 7.0;
/// The rope lets go when the rock it hangs from is dug away: the anchor sits on
/// the surface (distance ~0), so air this far from it means the rock is gone.
const ANCHOR_GONE: f32 = 0.3;

#[derive(Clone, Copy, Debug)]
pub struct Rope {
    pub anchor: Vec3,
    /// Maximum distance from the hand to the anchor. The worm can swing at any
    /// shorter distance (slack rope) but is pulled back from a longer one.
    pub length: f32,
}

#[derive(Clone, Debug)]
pub struct Worm {
    /// The point between the feet. The capsule stands on it.
    pub pos: Vec3,
    pub vel: Vec3,
    /// Where the worm looks (see `Input`).
    pub yaw: f32,
    pub pitch: f32,
    pub on_ground: bool,
    /// Normal of the ground the worm stands on (up when it is in the air).
    pub ground_normal: Vec3,
    /// Ticks until the next dig is allowed.
    pub dig_cooldown: u8,
    pub rope: Option<Rope>,
    /// Was the rope button down last tick? (To react to the press, not the hold.)
    rope_was_down: bool,
    jump_was_down: bool,
    /// True from a jump until the worm stops rising or lands: only then does
    /// letting go of the button cut the jump short.
    jumping: bool,
    /// Ticks left in which a jump is still allowed after leaving the ground.
    coyote: u8,
    /// Ticks left in which an early jump press is remembered.
    jump_buffer: u8,
}

impl Worm {
    pub fn new(pos: Vec3) -> Worm {
        Worm {
            pos,
            vel: Vec3::ZERO,
            yaw: 0.0,
            pitch: 0.0,
            on_ground: false,
            ground_normal: Vec3::Y,
            dig_cooldown: 0,
            rope: None,
            rope_was_down: false,
            jump_was_down: false,
            jumping: false,
            coyote: 0,
            jump_buffer: 0,
        }
    }

    /// Unit vector the worm looks along (the same maths as the viewer camera).
    pub fn aim_dir(&self) -> Vec3 {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        Vec3::new(sy * cp, sp, cy * cp)
    }

    pub fn eye(&self) -> Vec3 {
        self.pos + Vec3::Y * EYE_HEIGHT
    }

    pub fn hand(&self) -> Vec3 {
        self.pos + Vec3::Y * HAND_HEIGHT
    }
}

/// Something that happened this tick, for the renderer, the audio and (later)
/// the network. Consumers read them with `World::drain_events`.
#[derive(Clone, Copy, Debug)]
pub enum Event {
    /// The terrain was edited. This (a shape and an operation, a few bytes) is
    /// also what the host will send to clients instead of the changed voxels.
    Edit { shape: Shape, op: Op },
}

pub struct World {
    /// The terrain. The renderer reads its dirty chunks after each step.
    pub grid: Grid,
    pub worms: Vec<Worm>,
    pub tick: u32,
    events: Vec<Event>,
}

impl World {
    /// One worm per spawn column `(x, z)`, dropped from the top of the world
    /// onto whatever ground is highest there. (The top must be open air.)
    pub fn new(grid: Grid, spawns: &[(f32, f32)]) -> World {
        let top = grid.size_metres().y - 0.5;
        let worms = spawns
            .iter()
            .map(|&(x, z)| {
                let y = grid.raycast(Vec3::new(x, top, z), -Vec3::Y, top).map_or(0.0, |t| top - t);
                Worm::new(Vec3::new(x, y + 0.05, z))
            })
            .collect();
        World { grid, worms, tick: 0, events: Vec::new() }
    }

    pub fn drain_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }

    /// Advance the game by one tick (`DT`). `inputs[i]` drives worm `i`; a
    /// worm without an input stands still.
    pub fn step(&mut self, inputs: &[Input]) {
        for (i, worm) in self.worms.iter_mut().enumerate() {
            let input = clean(&inputs.get(i).copied().unwrap_or_default());
            step_worm(&mut self.grid, worm, &input, &mut self.events);
        }
        self.tick += 1;
    }
}

/// Inputs come from outside, so they are untrusted: NaN would poison the
/// worm's position for good (NaN compares false with everything, so every
/// "is it inside the rock?" test would pass it), and a huge number could
/// launch it through the world. Clamp everything to what a polite client sends.
fn clean(i: &Input) -> Input {
    let finite = |x: f32| if x.is_finite() { x } else { 0.0 };
    // Clamp each axis first: the length of a 1e30 vector overflows to infinity.
    let m = Vec2::new(finite(i.move_dir.x).clamp(-8.0, 8.0), finite(i.move_dir.y).clamp(-8.0, 8.0));
    Input {
        move_dir: m.clamp_length_max(1.0),
        yaw: finite(i.yaw),
        pitch: finite(i.pitch).clamp(-PITCH_LIMIT, PITCH_LIMIT),
        reel: finite(i.reel).clamp(-1.0, 1.0),
        ..*i
    }
}

fn step_worm(grid: &mut Grid, worm: &mut Worm, input: &Input, events: &mut Vec<Event>) {
    worm.yaw = input.yaw;
    worm.pitch = input.pitch;

    // Rope button: a press shoots it, or lets go if it is out.
    let rope_pressed = input.rope && !worm.rope_was_down;
    worm.rope_was_down = input.rope;
    if rope_pressed {
        if worm.rope.is_some() {
            worm.rope = None;
        } else if let Some(t) = grid.raycast(worm.eye(), worm.aim_dir(), ROPE_RANGE) {
            let anchor = worm.eye() + worm.aim_dir() * t;
            worm.rope = Some(Rope { anchor, length: (anchor - worm.hand()).length().max(ROPE_MIN) });
        }
    }
    // Jump button: remember presses; coyote time counts down once off the ground.
    let jump_pressed = input.jump && !worm.jump_was_down;
    worm.jump_was_down = input.jump;
    if jump_pressed {
        worm.jump_buffer = JUMP_BUFFER_TICKS;
    }
    worm.coyote = if worm.on_ground { COYOTE_TICKS } else { worm.coyote.saturating_sub(1) };
    // A new jump press in mid-air, on a rope: let go and kick upwards.
    let rope_kick = jump_pressed && worm.coyote == 0 && worm.rope.is_some();
    if rope_kick {
        worm.rope = None;
        worm.jump_buffer = 0;
    }

    let hand = worm.hand();
    if let Some(rope) = &mut worm.rope {
        // Reeling in first takes up any slack (a slack rope would waste the
        // pull), then shortens. Paying out just lengthens.
        let base = if input.reel > 0.0 { rope.length.min((rope.anchor - hand).length()) } else { rope.length };
        rope.length = (base - input.reel * REEL_SPEED * DT).clamp(ROPE_MIN, ROPE_RANGE);
        if grid.sample(rope.anchor) > ANCHOR_GONE {
            worm.rope = None;
        }
    }

    // Velocity. Movement is relative to where the worm looks.
    let (sy, cy) = worm.yaw.sin_cos();
    let forward = Vec3::new(sy, 0.0, cy);
    let right = Vec3::new(-cy, 0.0, sy);
    let wish = forward * input.move_dir.y + right * input.move_dir.x;
    let mut vel = worm.vel;
    if worm.on_ground {
        // Walk ALONG the ground, not horizontally: a horizontal walk into a
        // slope loses a quarter of its speed to the push-out every tick. So
        // work in the ground's plane: tilt the wish into it (keeping its
        // length) and accelerate the sideways-to-the-normal part of the velocity.
        // Gravity is not applied: the ground would cancel it anyway, and the
        // leftover pull along a slope would make a standing worm creep downhill.
        // (`settle_on_ground` keeps the worm touching the surface instead.)
        let n = worm.ground_normal;
        let tilted = wish - n * wish.dot(n);
        let target = if tilted.length() > 1.0e-4 { tilted / tilted.length() * wish.length() } else { Vec3::ZERO };
        let along = vel - n * vel.dot(n);
        // Approach the wanted velocity at a fixed rate. With no input the wanted
        // velocity is zero, so this is also the friction that stops the worm.
        // Faster than wanted while steering (landing from a swing): only a
        // gentle drag, so momentum survives the landing.
        let wanted = target * WALK_SPEED;
        let delta = wanted - along;
        let overspeed = wanted.length() > 0.1 && along.length() > wanted.length() + 0.5;
        let max_change = if overspeed { OVERSPEED_DRAG } else { GROUND_ACCEL } * DT;
        vel = along + if delta.length() <= max_change { delta } else { delta / delta.length() * max_change };
    } else {
        // In the air: add speed only in the wished direction and only up to a
        // cap, so steering never removes a swing's momentum. On a rope the push
        // is stronger and the cap much higher: pumping a swing.
        let (accel, cap) = if worm.rope.is_some() { (SWING_ACCEL, SWING_MAX_SPEED) } else { (AIR_ACCEL, WALK_SPEED) };
        let w = wish.length();
        if w > 1.0e-4 {
            let dir = wish / w;
            let flat = Vec3::new(vel.x, 0.0, vel.z);
            let add = (cap * w - flat.dot(dir)).clamp(0.0, accel * DT);
            vel += dir * add;
        }
        let falling = vel.y < 0.0 && worm.rope.is_none();
        vel.y -= GRAVITY * if falling { FALL_GRAVITY_MULT } else { 1.0 } * DT;
    }

    // Reeling in accelerates the worm towards the anchor, up to a top speed.
    if let (Some(rope), true) = (worm.rope, input.reel > 0.0) {
        let to_anchor = rope.anchor - worm.hand();
        let dist = to_anchor.length();
        if dist > 1.0e-4 {
            let dir = to_anchor / dist;
            let room = (REEL_MAX_SPEED - vel.dot(dir)).max(0.0);
            vel += dir * (REEL_ACCEL * input.reel * DT).min(room);
        }
    }
    if rope_kick {
        vel.y += ROPE_JUMP;
    }

    // Jump: a held button jumps on every landing, a press just before landing
    // is remembered, and `coyote` > 0 means "on the ground, or only just left it".
    let mut jumped = false;
    if (worm.jump_buffer > 0 || input.jump) && worm.coyote > 0 {
        jumped = true;
        vel.y = JUMP_SPEED;
        worm.coyote = 0;
        worm.jump_buffer = 0;
        worm.jumping = true;
    }
    worm.jump_buffer = worm.jump_buffer.saturating_sub(1);
    // Variable jump height: letting go while rising cuts the jump short.
    if worm.jumping {
        if vel.y <= 0.0 {
            worm.jumping = false;
        } else if !input.jump && vel.y > JUMP_CUT_SPEED {
            vel.y = JUMP_CUT_SPEED;
            worm.jumping = false;
        }
    }
    worm.vel = vel.clamp_length_max(MAX_SPEED);

    // Move in small steps, pushing out of the rock after each. If a ledge blocks
    // a walking worm, try again from half a metre higher.
    let was_on_ground = worm.on_ground;
    let start = worm.pos;
    let disp = worm.vel * DT;
    let mut grounded = slide(grid, worm, disp);
    if was_on_ground && wish.length_squared() > 1.0e-6 {
        let intended = Vec3::new(disp.x, 0.0, disp.z).length();
        grounded |= try_step_up(grid, worm, start, intended, wish * (WALK_SPEED * DT));
    }

    // Rope: a taut rope is a hard limit on the distance to the anchor. Pull the
    // worm back inside it and cancel the speed that was taking it further away.
    // What is left is sideways speed, which is what makes it a pendulum.
    if let Some(rope) = worm.rope {
        let to_anchor = rope.anchor - worm.hand();
        let dist = to_anchor.length();
        if dist > rope.length && dist > 1.0e-4 {
            let dir = to_anchor / dist;
            worm.pos += dir * (dist - rope.length);
            worm.vel -= dir * worm.vel.dot(dir).min(0.0);
            // The pull may have dragged the worm into rock.
            grounded |= push_out_of_rock(grid, worm, false);
        }
    }
    // Stick to the ground if walking, but not after a jump or while reeling in.
    let stick = was_on_ground && !jumped && !worm.jumping && !(worm.rope.is_some() && input.reel > 0.0);
    let settled = settle_on_ground(grid, worm, stick);
    worm.on_ground = grounded || settled;
    if worm.on_ground {
        worm.jumping = false;
    }
    if !worm.on_ground {
        worm.ground_normal = Vec3::Y;
    }
    // Safety net: nothing legitimate leaves the world (outside counts as rock),
    // but a bug or a freak push must never lose a worm for good.
    let size = grid.size_metres();
    let lo = Vec3::new(WORM_RADIUS, 0.0, WORM_RADIUS);
    worm.pos = worm.pos.clamp(lo, Vec3::new(size.x - WORM_RADIUS, size.y - WORM_HEIGHT, size.z - WORM_RADIUS));

    // Dig. After the move, so the hole is where the worm is looking now.
    worm.dig_cooldown = worm.dig_cooldown.saturating_sub(1);
    if input.dig && worm.dig_cooldown == 0 {
        if let Some(centre) = dig_target(grid, worm) {
            let shape = Shape::Sphere { center: centre, radius: DIG_RADIUS };
            grid.apply(shape, Op::Carve);
            events.push(Event::Edit { shape, op: Op::Carve });
            worm.dig_cooldown = DIG_INTERVAL;
        }
    }
}

/// Where a dig would be centred: the first rock the worm looks at within reach.
/// The viewer uses this too, to preview the hole.
pub fn dig_target(grid: &Grid, worm: &Worm) -> Option<Vec3> {
    let dir = worm.aim_dir();
    grid.raycast(worm.eye(), dir, DIG_REACH).map(|t| worm.eye() + dir * t)
}

/// Resolve the overlaps between the body spheres and the terrain. Returns true
/// if the worm is pushed up by something ground-like.
///
/// Pushing a sphere out of a distance field: the field value is how far the
/// sphere's centre is from the surface, so `radius - value` is how deep it is
/// in, and the gradient is the way out. Several spheres can push in different
/// directions (a worm in a corner), so repeat a few passes until none is
/// touching.
///
/// `roll_over_edges` keeps the tilted normal at an edge. Only the step-up uses
/// it: lowering the worm onto a ledge it relies on the foot rolling up the
/// corner, which a sideways-only push forbids.
fn push_out_of_rock(grid: &Grid, worm: &mut Worm, roll_over_edges: bool) -> bool {
    let mut grounded = false;
    for _ in 0..4 {
        let mut pushed = false;
        for offset in BODY_SPHERES {
            let centre = worm.pos + Vec3::Y * offset;
            let d = grid.sample(centre);
            if d >= WORM_RADIUS {
                continue;
            }
            let mut normal = grid.gradient(centre);
            if normal == Vec3::ZERO {
                normal = Vec3::Y; // buried deeper than the field knows: go up
            }
            let ground_like = normal.y > GROUND_COS;
            // A wall pushes sideways only. Pushed along its own (slightly tilted)
            // normal, a worm running into a wall was lifted a little each tick, and
            // at the crease where wall meets floor, where the normal is diagonal,
            // it climbed the wall. (A ceiling, normal.y < 0, still pushes down.)
            if !roll_over_edges && !ground_like && normal.y > 0.0 {
                let sideways = Vec3::new(normal.x, 0.0, normal.z);
                if sideways.length() > 1.0e-3 {
                    normal = sideways.normalize();
                }
            }
            worm.pos += normal * (WORM_RADIUS - d).min(MAX_PUSH);
            // Remove the part of the velocity that points into the surface, keep
            // the rest: that is sliding along a wall.
            let into = worm.vel.dot(normal);
            if into < 0.0 {
                worm.vel -= normal * into;
            }
            grounded |= ground_like;
            pushed = true;
        }
        if !pushed {
            break;
        }
    }
    grounded
}

/// Move by `disp` in steps of at most `MAX_STEP`, pushing out of the rock after
/// each. Returns true if something ground-like pushed the worm.
fn slide(grid: &Grid, worm: &mut Worm, disp: Vec3) -> bool {
    let steps = (disp.length() / MAX_STEP).ceil().max(1.0);
    let mut grounded = false;
    for _ in 0..steps as u32 {
        worm.pos += disp / steps;
        grounded |= push_out_of_rock(grid, worm, false);
    }
    grounded
}

/// A worm walking into a ledge is stopped by the rounded foot touching its edge.
/// If the walk made less than a quarter of the horizontal distance it meant to
/// (`intended`), try a full tick of walking (`want`) from `STEP_UP` higher, then
/// lower the worm onto whatever is there. Keeps the result only if it got
/// further. `want` is not the real velocity because the wall has just cancelled
/// that (and a worm starting to walk has not got up to speed either).
fn try_step_up(grid: &Grid, worm: &mut Worm, start: Vec3, intended: f32, want: Vec3) -> bool {
    let flat = |v: Vec3| Vec3::new(v.x, 0.0, v.z).length();
    let got = flat(worm.pos - start);
    if got >= intended * 0.25 {
        return false; // not blocked
    }
    let (blocked_pos, blocked_vel) = (worm.pos, worm.vel);
    worm.pos = start + Vec3::Y * STEP_UP;
    let fits = BODY_SPHERES.iter().all(|&h| grid.sample(worm.pos + Vec3::Y * h) >= WORM_RADIUS * 0.99);
    if fits {
        slide(grid, worm, want);
        // Lower the worm until its foot touches something, a little at a time. (Dropping
        // it the whole way and pushing it out again would shove it back off the
        // ledge's corner by as much as it had just moved forward.)
        let mut dropped = 0.0;
        while dropped < STEP_UP && grid.sample(worm.pos + Vec3::Y * WORM_RADIUS) > WORM_RADIUS + 0.02 {
            worm.pos.y -= 0.05;
            dropped += 0.05;
        }
        let grounded = push_out_of_rock(grid, worm, true);
        if flat(worm.pos - start) > got + 0.02 {
            worm.vel = Vec3::new(want.x / DT, worm.vel.y, want.z / DT);
            return grounded;
        }
    }
    worm.pos = blocked_pos;
    worm.vel = blocked_vel;
    false
}

/// Is the worm standing on something? If the feet are within `GROUND_SKIN`
/// above ground-like terrain, and not moving away from it, pull the worm down
/// onto it (along the normal, so a slope does not make it slide).
///
/// With `stick` (the worm was walking), the reach is `STICK_DISTANCE` and
/// "moving away" is ignored: a worm walking over a crest has upward speed from
/// the slope it just climbed, and without this it would launch off every bump.
/// Its speed is turned into the new ground's plane instead.
///
/// The normal is taken at the feet, not at the foot sphere's centre: where a
/// wall meets the floor the field is the minimum of two planes, and at the
/// sphere's centre the gradient points diagonally between them.
fn settle_on_ground(grid: &Grid, worm: &mut Worm, stick: bool) -> bool {
    let reach = if stick { STICK_DISTANCE } else { GROUND_SKIN };
    let d = grid.sample(worm.pos + Vec3::Y * WORM_RADIUS);
    if d >= WORM_RADIUS + reach {
        return false;
    }
    let normal = grid.gradient(worm.pos);
    if normal.y <= GROUND_COS || (!stick && worm.vel.dot(normal) > 0.5) {
        return false; // a wall, or flying away (a jump just started)
    }
    // The feet themselves must be near the ground. The foot sphere can be close
    // to a WALL while the feet hang half a metre above the floor; that is not
    // standing. (On a slope the feet sit a little above the surface point the
    // sphere touches, hence the allowance.)
    if grid.sample(worm.pos) > reach + WORM_RADIUS * (1.0 - normal.y) {
        return false;
    }
    if d > WORM_RADIUS {
        worm.pos -= normal * (d - WORM_RADIUS);
    }
    if stick {
        let away = worm.vel.dot(normal);
        if away > 0.0 {
            worm.vel -= normal * away;
        }
    }
    worm.ground_normal = normal;
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::IVec3;
    use std::f32::consts::FRAC_PI_2;

    // Test worlds are 32 x 16 x 32 m. Each is a distance function (negative =
    // rock) turned into a grid with `Grid::from_fn`, so they build in
    // milliseconds even in a debug build.
    const SIZE: IVec3 = IVec3::new(8, 4, 8);

    /// Rock below y = 4.
    fn flat(p: Vec3) -> f32 {
        p.y - 4.0
    }

    /// The flat ground plus a wall: rock for x > 20.
    fn walled(p: Vec3) -> f32 {
        (p.y - 4.0).min(20.0 - p.x)
    }

    /// The flat ground plus a ceiling: rock above y = 14.
    fn ceilinged(p: Vec3) -> f32 {
        (p.y - 4.0).min(14.0 - p.y)
    }

    /// A 30 degree ramp rising towards +x. Exact distance to the plane.
    fn ramp(p: Vec3) -> f32 {
        let (s, c) = 30f32.to_radians().sin_cos();
        (p.y - 4.0 - p.x * s / c) * c
    }

    fn world(f: fn(Vec3) -> f32, spawn: (f32, f32)) -> World {
        World::new(Grid::from_fn(SIZE, f), &[spawn])
    }

    fn run(w: &mut World, ticks: usize, input: Input) {
        for _ in 0..ticks {
            w.step(&[input]);
        }
    }

    fn settled(f: fn(Vec3) -> f32, spawn: (f32, f32)) -> World {
        let mut w = world(f, spawn);
        // `World::new` drops worms from the top of the world, which is rock in
        // the ceiling world, so put the worm on the ground by hand.
        w.worms[0].pos.y = 4.05;
        run(&mut w, 60, Input::default());
        w
    }

    fn walk(yaw: f32) -> Input {
        Input { move_dir: Vec2::new(0.0, 1.0), yaw, ..Input::default() }
    }

    #[test]
    fn a_worm_lands_on_flat_ground_and_stays_put() {
        let mut w = world(flat, (16.0, 16.0));
        assert!(w.worms[0].pos.y > 4.0, "spawns just above the ground");
        run(&mut w, 60, Input::default());
        let at = w.worms[0].pos;
        run(&mut w, 120, Input::default());
        let worm = &w.worms[0];
        assert!(worm.on_ground);
        assert!((worm.pos.y - 4.0).abs() < 0.02, "rests on the surface: {}", worm.pos.y);
        assert!((worm.pos - at).length() < 0.01, "does not creep");
    }

    #[test]
    fn walking_goes_where_the_worm_looks_at_walking_speed() {
        let mut w = settled(flat, (16.0, 16.0));
        let start = w.worms[0].pos;
        run(&mut w, 30, walk(0.0)); // yaw 0 looks along +z
        let moved = w.worms[0].pos - start;
        assert!(moved.z > 3.0 && moved.z < 4.0, "half a second at 8 m/s minus the start-up: {moved:?}");
        assert!(moved.x.abs() < 0.01);
        run(&mut w, 30, walk(FRAC_PI_2)); // a quarter turn left looks along +x
        assert!(w.worms[0].pos.x - start.x > 2.0, "turned: {:?}", w.worms[0].pos);
        // Strafing right while looking along +z goes towards -x.
        let mut w = settled(flat, (16.0, 16.0));
        let x = w.worms[0].pos.x;
        run(&mut w, 30, Input { move_dir: Vec2::new(1.0, 0.0), ..Input::default() });
        assert!(w.worms[0].pos.x < x - 2.0);
    }

    #[test]
    fn a_worm_stops_when_the_input_stops() {
        let mut w = settled(flat, (16.0, 16.0));
        run(&mut w, 30, walk(0.0));
        run(&mut w, 15, Input::default());
        assert!(w.worms[0].vel.length() < 0.01, "{:?}", w.worms[0].vel);
    }

    #[test]
    fn a_wall_stops_the_worm_and_it_does_not_get_inside() {
        let mut w = settled(walled, (16.0, 16.0));
        run(&mut w, 180, walk(FRAC_PI_2));
        let worm = &w.worms[0];
        let reach = 20.0 - WORM_RADIUS;
        assert!(worm.pos.x > reach - 0.1 && worm.pos.x < reach + 0.05, "x = {}", worm.pos.x);
        assert!(worm.on_ground);
    }

    /// Highest the worm gets above `ground` in `ticks` ticks of `input`.
    fn apex(w: &mut World, ground: f32, ticks: usize, input: Input) -> f32 {
        let mut apex: f32 = 0.0;
        for _ in 0..ticks {
            w.step(&[input]);
            apex = apex.max(w.worms[0].pos.y - ground);
        }
        apex
    }

    #[test]
    fn holding_jump_rises_two_and_a_half_metres_and_a_tap_hops_low() {
        let mut w = settled(flat, (16.0, 16.0));
        let ground = w.worms[0].pos.y;
        let up = apex(&mut w, ground, 30, Input { jump: true, ..Input::default() });
        assert!(up > 2.0 && up < 2.6, "held jump apex {up}");
        run(&mut w, 90, Input::default());
        assert!(w.worms[0].on_ground, "back on the ground");

        let mut w = settled(flat, (16.0, 16.0));
        w.step(&[Input { jump: true, ..Input::default() }]);
        let hop = apex(&mut w, ground, 90, Input::default());
        assert!(hop > 0.15 && hop < 1.0, "a tap is a hop: {hop}");
    }

    #[test]
    fn there_is_no_jumping_in_mid_air() {
        let mut w = settled(flat, (16.0, 16.0));
        w.step(&[Input { jump: true, ..Input::default() }]);
        run(&mut w, 10, Input { jump: true, ..Input::default() });
        let worm = &w.worms[0];
        assert!(!worm.on_ground);
        assert!(worm.vel.y < JUMP_SPEED - 3.0, "still slowing down, no second jump: {}", worm.vel.y);
    }

    #[test]
    fn a_jump_pressed_just_before_landing_still_happens() {
        let mut w = settled(flat, (16.0, 16.0));
        w.worms[0].pos.y += 1.5;
        w.worms[0].on_ground = false;
        while !(w.worms[0].vel.y < 0.0 && w.worms[0].pos.y < 4.0 + 0.4) {
            w.step(&[Input::default()]); // fall until just above the ground
        }
        w.step(&[Input { jump: true, ..Input::default() }]); // a press, too early
        let mut best: f32 = 0.0;
        for _ in 0..20 {
            w.step(&[Input::default()]);
            best = best.max(w.worms[0].vel.y);
        }
        assert!(best > 3.0, "the buffered press jumped on landing: {best}");
    }

    #[test]
    fn a_jump_pressed_just_after_walking_off_an_edge_still_works() {
        // A 3 m cliff: ground at y = 4 for x < 18, at y = 1 beyond.
        fn cliff(p: Vec3) -> f32 {
            (p.y - 4.0).max(p.x - 18.0).min(p.y - 1.0)
        }
        let mut w = settled(cliff, (14.0, 16.0));
        let go = walk(FRAC_PI_2);
        for _ in 0..200 {
            w.step(&[go]);
            if !w.worms[0].on_ground {
                break;
            }
        }
        assert!(!w.worms[0].on_ground, "walked off the edge");
        run(&mut w, 3, go);
        w.step(&[Input { jump: true, ..go }]);
        assert!(w.worms[0].vel.y > 5.0, "coyote time: {}", w.worms[0].vel.y);
        // But not long after.
        let mut w = settled(cliff, (14.0, 16.0));
        for _ in 0..200 {
            w.step(&[go]);
            if !w.worms[0].on_ground {
                break;
            }
        }
        run(&mut w, 12, go);
        w.step(&[Input { jump: true, ..go }]);
        assert!(w.worms[0].vel.y < 0.0, "too late: {}", w.worms[0].vel.y);
    }

    #[test]
    fn speed_from_the_air_survives_a_landing_while_steering_but_not_without_keys() {
        let landing = |input: Input| {
            let mut w = settled(flat, (6.0, 16.0));
            w.worms[0].pos.y += 0.3;
            w.worms[0].on_ground = false;
            w.worms[0].vel = Vec3::new(16.0, -2.0, 0.0);
            run(&mut w, 30, input);
            w.worms[0].vel.x
        };
        assert!(landing(walk(FRAC_PI_2)) > 11.0, "steering keeps momentum: {}", landing(walk(FRAC_PI_2)));
        assert!(landing(Input::default()) < 1.0, "letting go of the keys brakes: {}", landing(Input::default()));
    }

    #[test]
    fn a_worm_stands_on_a_slope_without_sliding_and_can_climb_it() {
        // Low on the ramp: it rises 0.58 m per metre and the test world is only
        // 16 m tall, so a worm higher up would soon bump its head on the world top.
        let mut w = settled(ramp, (6.0, 16.0));
        let at = w.worms[0].pos;
        assert!(w.worms[0].on_ground);
        run(&mut w, 120, Input::default());
        let drift = Vec3::new(w.worms[0].pos.x - at.x, 0.0, w.worms[0].pos.z - at.z).length();
        assert!(drift < 0.05, "slid {drift} m in 2 s");
        run(&mut w, 60, walk(FRAC_PI_2)); // +x is uphill
        let climbed = w.worms[0].pos.x - at.x;
        assert!(climbed > 3.0, "climbed {climbed} m in a second");
        assert!(w.worms[0].on_ground);
    }

    #[test]
    fn a_worm_buried_in_rock_is_pushed_out() {
        let mut w = settled(flat, (16.0, 16.0));
        w.worms[0].pos.y = 3.3; // the body is half inside the ground
        run(&mut w, 60, Input::default());
        assert!((w.worms[0].pos.y - 4.0).abs() < 0.05, "{}", w.worms[0].pos.y);
    }

    #[test]
    fn digging_carves_the_rock_it_looks_at_and_reports_the_edit() {
        let mut w = settled(walled, (17.5, 16.0));
        let look = Input { yaw: FRAC_PI_2, dig: true, ..Input::default() };
        let eye_y = w.worms[0].eye().y;
        assert!(w.grid.sample(Vec3::new(20.5, eye_y, 16.0)) < 0.0);
        w.step(&[look]);
        assert!(w.grid.sample(Vec3::new(20.5, eye_y, 16.0)) > 0.0, "a hole in the wall");
        let events = w.drain_events();
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], Event::Edit { op: Op::Carve, .. }));
        assert!(w.drain_events().is_empty(), "drained");
        assert!(!w.grid.take_dirty().is_empty(), "the renderer is told which chunks changed");
    }

    #[test]
    fn digging_is_rate_limited_and_needs_rock_within_reach() {
        // Looking at the sky: nothing to dig.
        let mut w = settled(flat, (16.0, 16.0));
        run(&mut w, 30, Input { pitch: 1.0, dig: true, ..Input::default() });
        assert!(w.drain_events().is_empty());
        // Digging into a wall while walking forward for 3 s: one dig per
        // DIG_INTERVAL ticks at most, and plenty of them.
        let mut w = settled(walled, (17.5, 16.0));
        let dig_in = Input { yaw: FRAC_PI_2, pitch: -0.15, dig: true, move_dir: Vec2::new(0.0, 1.0), ..Input::default() };
        run(&mut w, 180, dig_in);
        let n = w.drain_events().len();
        assert!(n >= 4, "only {n} digs in 3 s");
        assert!(n <= 180 / DIG_INTERVAL as usize + 1, "{n} digs in 3 s is faster than the limit");
    }

    #[test]
    fn a_worm_can_dig_its_own_tunnel_into_a_hillside() {
        let mut w = settled(walled, (17.5, 16.0));
        // Looking slightly down so the hole reaches the floor the worm stands on.
        let dig_in = Input { yaw: FRAC_PI_2, pitch: -0.15, dig: true, move_dir: Vec2::new(0.0, 1.0), ..Input::default() };
        run(&mut w, 300, dig_in);
        let x = w.worms[0].pos.x;
        assert!(x > 20.0 + 4.0, "walked 4 m into the wall, x = {x}");
    }

    #[test]
    fn the_rope_hangs_on_the_ceiling_and_reeling_pulls_the_worm_up() {
        let mut w = settled(ceilinged, (16.0, 16.0));
        let shoot = Input { pitch: 1.0, rope: true, ..Input::default() };
        w.step(&[shoot]);
        let rope = w.worms[0].rope.expect("attached");
        assert!((rope.anchor.y - 14.0).abs() < 0.1, "anchored on the ceiling: {:?}", rope.anchor);
        run(&mut w, 1, Input { pitch: 1.0, ..Input::default() }); // button up
        let before = w.worms[0].pos.y;
        run(&mut w, 30, Input { pitch: 1.0, reel: 1.0, ..Input::default() });
        let worm = &w.worms[0];
        let rope = worm.rope.expect("still attached");
        assert!(rope.length < 8.0, "reeled in 3 m: {}", rope.length);
        assert!((rope.anchor - worm.hand()).length() <= rope.length + 0.1, "never further than the rope");
        assert!(worm.pos.y > before + 0.5, "pulled off the ground");
    }

    #[test]
    fn a_rope_swing_builds_speed_and_letting_go_keeps_it() {
        let mut w = settled(ceilinged, (16.0, 16.0));
        // Hang in mid-air under the ceiling and shoot the rope up and forward.
        w.worms[0].pos = Vec3::new(16.0, 8.0, 16.0);
        w.worms[0].on_ground = false;
        let press = Input { pitch: 0.9, rope: true, ..Input::default() };
        let hold = Input { pitch: 0.9, ..Input::default() };
        w.step(&[press]);
        // Holding the button is not pressing it again.
        run(&mut w, 5, press);
        assert!(w.worms[0].rope.is_some(), "holding is not pressing again");
        // The rope is taut from the start, so gravity swings the worm down and
        // forward like a pendulum.
        let mut fastest: f32 = 0.0;
        for _ in 0..45 {
            w.step(&[hold]);
            fastest = fastest.max(w.worms[0].vel.length());
            let worm = &w.worms[0];
            let rope = worm.rope.unwrap();
            assert!((rope.anchor - worm.hand()).length() <= rope.length + 0.1, "stretched the rope");
            assert!(!worm.on_ground, "the swing stays in the air");
        }
        assert!(fastest > 5.0, "a swing from rest builds speed: {fastest}");
        // Second press: let go.
        w.step(&[press]);
        assert!(w.worms[0].rope.is_none());
        let flat = |w: &World| Vec3::new(w.worms[0].vel.x, 0.0, w.worms[0].vel.z).length();
        let before = flat(&w);
        run(&mut w, 5, hold);
        assert!(flat(&w) > before * 0.9, "no air drag: {before} -> {}", flat(&w));
    }

    #[test]
    fn the_rope_lets_go_when_its_rock_is_dug_away() {
        let mut w = settled(walled, (17.5, 16.0));
        w.step(&[Input { yaw: FRAC_PI_2, rope: true, ..Input::default() }]);
        let anchor = w.worms[0].rope.expect("attached").anchor;
        w.grid.apply(Shape::Sphere { center: anchor, radius: 1.5 }, Op::Carve);
        w.step(&[Input { yaw: FRAC_PI_2, ..Input::default() }]);
        assert!(w.worms[0].rope.is_none());
    }

    #[test]
    fn a_worm_walks_up_a_low_ledge_but_not_a_high_wall() {
        // The ground plus a block for x > 18, 0.4 m and 1.2 m high.
        // (`Grid::from_fn` wants a plain fn pointer, so no closure over the height.)
        fn low(p: Vec3) -> f32 {
            (p.y - 4.0).min((p.y - 4.4).max(18.0 - p.x))
        }
        fn high(p: Vec3) -> f32 {
            (p.y - 4.0).min((p.y - 5.2).max(18.0 - p.x))
        }
        let mut w = settled(low, (16.0, 16.0));
        run(&mut w, 120, walk(FRAC_PI_2));
        let worm = &w.worms[0];
        assert!(worm.pos.x > 19.0, "got up the 0.4 m ledge: x = {}", worm.pos.x);
        assert!((worm.pos.y - 4.4).abs() < 0.05 && worm.on_ground, "stands on it: y = {}", worm.pos.y);

        let mut w = settled(high, (16.0, 16.0));
        run(&mut w, 120, walk(FRAC_PI_2));
        assert!(w.worms[0].pos.x < 18.0 - WORM_RADIUS + 0.05, "a 1.2 m wall stops it: x = {}", w.worms[0].pos.x);
    }



    /// A pit 10 m deep and 6 m wide in ground at y = 10, centred on (16, 16).
    fn pit(p: Vec3) -> f32 {
        let r = ((p.x - 16.0).powi(2) + (p.z - 16.0).powi(2)).sqrt();
        (p.y - 10.0).max(3.0 - r)
    }

    #[test]
    fn the_rope_launches_a_worm_out_of_a_pit_it_cannot_jump_out_of() {
        let towards_wall = Input { yaw: FRAC_PI_2, pitch: 1.2, move_dir: Vec2::new(0.0, 1.0), ..Input::default() };

        // Without the rope: jump and push at the wall for five seconds.
        let mut w = settled(pit, (16.0, 16.0));
        let floor = w.worms[0].pos.y;
        let best = apex(&mut w, floor, 300, Input { jump: true, ..towards_wall });
        assert!(best < 4.0, "a 10 m pit is no place for jumping: rose {best} m");

        // With it: shoot at the wall just under the rim, reel in (which
        // accelerates the worm up the wall), and kick off with a jump at the top.
        let mut w = settled(pit, (16.0, 16.0));
        w.step(&[Input { rope: true, ..towards_wall }]);
        let rope = w.worms[0].rope.expect("the rope caught the wall");
        assert!(rope.anchor.y > 8.0 && rope.anchor.y < 10.0, "anchored just under the rim: {:?}", rope.anchor);
        let reel = Input { reel: 1.0, ..towards_wall };
        for _ in 0..180 {
            w.step(&[reel]);
            if (rope.anchor - w.worms[0].hand()).length() < 2.0 {
                break;
            }
        }
        w.step(&[Input { jump: true, ..towards_wall }]); // the rope kick
        assert!(w.worms[0].rope.is_none(), "jumping lets go of the rope");
        run(&mut w, 120, towards_wall);
        let worm = &w.worms[0];
        assert!(worm.pos.y > 9.9, "up on the ground above the pit: y = {}", worm.pos.y);
        assert!(worm.on_ground, "standing there");
        assert!(worm.pos.x > 16.0 + 3.2, "beyond the rim, not in the pit: x = {}", worm.pos.x);
    }

    #[test]
    fn steering_on_the_rope_pumps_the_swing_beyond_walking_speed() {
        // Same hang as the swing test: under a ceiling, rope up and forward.
        let swing = |steer: bool| {
            let mut w = settled(ceilinged, (16.0, 16.0));
            w.worms[0].pos = Vec3::new(16.0, 8.0, 16.0);
            w.worms[0].on_ground = false;
            let hold = Input { pitch: 0.9, ..Input::default() };
            w.step(&[Input { rope: true, ..hold }]);
            let mut fastest: f32 = 0.0;
            for t in 0..150 {
                // Push the way the worm is already swinging (what a player does).
                let along = w.worms[0].vel.z;
                let push = if steer && t > 10 { along.signum() } else { 0.0 };
                w.step(&[Input { move_dir: Vec2::new(0.0, push), ..hold }]);
                fastest = fastest.max(Vec3::new(w.worms[0].vel.x, 0.0, w.worms[0].vel.z).length());
            }
            fastest
        };
        let (idle, pumped) = (swing(false), swing(true));
        assert!(pumped > WALK_SPEED + 3.0, "pumped swing {pumped}");
        assert!(pumped > idle * 1.4, "pumping beats hanging on: {idle} -> {pumped}");
    }

    #[test]
    fn walking_over_bumpy_ground_keeps_the_pace() {
        // Bumps 0.15 m high every 1.2 m, up to 38 degrees: rougher than the floor
        // a digger leaves. (Scaled by 0.8 so the field never over-estimates distance.)
        fn bumpy(p: Vec3) -> f32 {
            (p.y - 4.0 - 0.15 * (p.x * 5.2).sin()) * 0.8
        }
        let mut w = settled(bumpy, (6.0, 16.0));
        let start = w.worms[0].pos.x;
        let mut grounded = 0;
        for _ in 0..120 {
            w.step(&[walk(FRAC_PI_2)]);
            grounded += w.worms[0].on_ground as u32;
        }
        let went = w.worms[0].pos.x - start;
        assert!(went > 12.0, "2 s at 8 m/s is 16 m, minus the start: only {went}");
        assert!(grounded > 100, "stays on the ground instead of hopping: {grounded}/120");
    }

    #[test]
    fn a_worm_can_walk_back_out_of_the_tunnel_it_dug() {
        // The floor of a dug tunnel is a row of bowls, not a plane.
        let mut w = settled(walled, (17.5, 16.0));
        let dig_in = Input { yaw: FRAC_PI_2, pitch: -0.15, dig: true, move_dir: Vec2::new(0.0, 1.0), ..Input::default() };
        run(&mut w, 300, dig_in);
        let deep = w.worms[0].pos.x;
        assert!(deep > 24.0, "dug in first: {deep}");
        // Turn round (looking along -x) and walk out.
        let out = Input { yaw: -FRAC_PI_2, move_dir: Vec2::new(0.0, 1.0), ..Input::default() };
        let mut ticks = 0;
        while w.worms[0].pos.x > 19.0 && ticks < 600 {
            w.step(&[out]);
            ticks += 1;
        }
        assert!(w.worms[0].pos.x <= 19.0, "stuck in the tunnel at x = {}", w.worms[0].pos.x);
        // ~6 m of tunnel at walking speed is under a second; allow for the bumps.
        assert!(ticks < 120, "took {ticks} ticks to walk {} m", deep - 19.0);
    }

    #[test]
    fn nonsense_input_cannot_break_the_worm() {
        let mut w = settled(walled, (17.5, 16.0));
        let junk = [f32::NAN, f32::INFINITY, -f32::INFINITY, 1.0e30, -1.0e30];
        for &x in &junk {
            let input = Input {
                move_dir: Vec2::new(x, x),
                yaw: x,
                pitch: x,
                reel: x,
                jump: true,
                dig: true,
                rope: true,
                ..Input::default()
            };
            run(&mut w, 10, input);
            run(&mut w, 2, Input::default());
        }
        let worm = &w.worms[0];
        assert!(worm.pos.is_finite() && worm.vel.is_finite(), "{worm:?}");
        assert!(worm.vel.length() <= MAX_SPEED + 0.01);
        let size = w.grid.size_metres();
        assert!(worm.pos.cmpge(Vec3::ZERO).all() && worm.pos.cmple(size).all(), "still in the world");
    }

    #[test]
    fn same_inputs_give_same_world() {
        // A busy script: turning, walking, jumping, digging, roping.
        let script = |t: usize| Input {
            move_dir: Vec2::new(if t % 90 < 30 { 1.0 } else { 0.0 }, if t % 70 < 50 { 1.0 } else { -0.5 }),
            yaw: t as f32 * 0.013,
            pitch: ((t % 100) as f32 - 50.0) * 0.01,
            jump: t % 45 == 0,
            dig: t % 3 != 0,
            rope: t % 37 == 0,
            reel: if t % 11 < 3 { 1.0 } else { 0.0 },
            ..Input::default()
        };
        let play = || {
            let mut w = world(walled, (17.0, 16.0));
            for t in 0..600 {
                w.step(&[script(t)]);
            }
            w
        };
        let (a, b) = (play(), play());
        assert_eq!(a.grid.checksum(), b.grid.checksum());
        let bits = |w: &World| {
            let v = &w.worms[0];
            [v.pos.x, v.pos.y, v.pos.z, v.vel.x, v.vel.y, v.vel.z].map(f32::to_bits)
        };
        assert_eq!(bits(&a), bits(&b));
        assert_ne!(a.grid.checksum(), world(walled, (17.0, 16.0)).grid.checksum(), "the script did dig");
    }
}
