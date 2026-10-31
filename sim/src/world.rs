// The world and the worm: movement against the signed distance field, jumping,
// digging and the ninja rope. Weapons, projectiles and damage are in `combat.rs`.
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

use crate::combat::{Projectile, Swing, WhipStyle, MAX_HP};
use crate::input::Input;
use crate::lava::LavaBit;
use crate::rng::Rng;
use crate::weapons::{WeaponState, ARSENAL, WEAPONS};
use glam::{Vec2, Vec3};
use sdf::{Grid, Op, Shape};
use serde::{Deserialize, Serialize};

/// The sim runs at a fixed 60 Hz, whatever the frame rate of whoever draws it.
/// A fixed step makes physics repeatable: variable steps change how high you
/// jump and whether you tunnel through a wall.
pub const DT: f32 = 1.0 / 60.0;

/// Kills needed to win a match. A match can choose another (`World::score_limit`).
pub const SCORE_LIMIT: i32 = 15;

// --- the worm's body -----------------------------------------------------
/// The collision capsule. The *drawn* body is slimmer than this (see
/// `body_r` in the viewer): the character reads better thin, but the movement
/// rules (what it can climb, what gap it fits) are tuned around this radius.
pub const WORM_RADIUS: f32 = 0.45;
pub const WORM_HEIGHT: f32 = 1.8;
/// Eye (and aim origin) above the feet.
pub const EYE_HEIGHT: f32 = 1.5;
/// Height of the hand: where the rope and the gun are.
const HAND_HEIGHT: f32 = 1.028;
/// How far out to the body's right and forward the gun hand sits. These mirror
/// the viewer's standing arm pose (`SDF_HAND_R` in `viewer/src/worms.rs`, in
/// model units, scaled by `WORM_HEIGHT / SDF_MODEL`), because the sim decides
/// where a shot starts (and that has to be the barrel, not the chest). Keep the
/// two in step: if the sculpt's hand moves and this does not, shots leave from
/// thin air beside the worm instead of from the gun.
const HAND_RIGHT: f32 = 0.128;
const HAND_FORWARD: f32 = 0.808;
/// Pattern: Spatial approximation. The collision shape is a capsule, tested as
/// three spheres up its axis (feet, middle, head). A sphere against a distance
/// field is one lookup: `distance < radius` means touching, and the gradient
/// says which way to push out. The spheres are one radius apart, so they
/// overlap and leave no gap a thin ledge could slip through.
pub(crate) const BODY_SPHERES: [f32; 3] = [WORM_RADIUS, WORM_HEIGHT * 0.5, WORM_HEIGHT - WORM_RADIUS];

/// A surface counts as ground when its normal is within ~70 degrees of up.
/// Steeper is a wall: the worm slides along it instead of standing on it.
/// (This used to be 41 degrees. Craters are bowls whose rims approach vertical,
/// and at 41 the worm could not climb more than a metre up the wall before it
/// became "a wall" and stopped. 70 lets it crawl out of its own blast holes;
/// only near-vertical walls still need the climb move.)
pub(crate) const GROUND_COS: f32 = 0.35;
/// Still "standing" when the feet are this far above the surface, and then the
/// worm is pulled down onto it. It keeps the worm grounded while walking over
/// bumps and down slopes, where each tick would otherwise be a tiny fall.
const GROUND_SKIN: f32 = 0.1;
/// A worm that was walking sticks to ground up to this far below it (crests of
/// bumps, steps down), instead of launching off every crest like a ball.
const STICK_DISTANCE: f32 = 0.3;
/// Highest ledge the worm walks up without jumping. Deliberately generous: the
/// craters the weapons leave are a metre or two deep, with scalloped lips that
/// used to stop the worm dead. The dedicated climb move is for walls taller
/// than this.
const STEP_UP: f32 = 0.9;
/// How far forward a step-up carries the worm. A tick of walking is only ~0.2 m,
/// less than the body radius, so the foot would still be against the face; the
/// mantle has to place the body on top of the lip in one go.
const MANTLE_REACH: f32 = 0.6;
/// A mantle only lands on a roughly flat-ish top (cos 45 degrees). The walk is
/// blocked only by surfaces steeper than 70 degrees, so this cannot be used to
/// skate up a walkable slope; it just lets a crater rim on a hillside count.
const MANTLE_FLAT: f32 = 0.7;
/// Largest move between collision checks. The distance field is only valid in a
/// 1 m band, and a fast worm could otherwise end a tick deeper in rock than the
/// field can say which way is out.
const MAX_STEP: f32 = 0.2;
/// Largest correction per overlapping sphere and pass: a worm buried deeper
/// than the field's band (where the gradient is zero) is pushed up gradually.
const MAX_PUSH: f32 = 0.5;
/// Moving and colliding constants live with the movement rules in
/// `movement.rs`; the body's shape and the dig stay here.
const PITCH_LIMIT: f32 = 1.553; // 89 degrees

// --- digging --------------------------------------------------------------
/// How far the worm can reach (from the eye to the rock).
const DIG_REACH: f32 = 4.0;
/// Radius of the hole. Wide enough for the worm (1.8 m tall) with room to
/// spare; the capsule joins below keep the width smooth, so this no longer has
/// to be derived from a cusp angle.
pub const DIG_RADIUS: f32 = 1.5;
/// Ticks between digs while the button is held: 5 a second.
const DIG_INTERVAL: u8 = 12;
/// How far the tunnel front moves per bite. This must beat the run: a bite
/// every 12 ticks and a run of 11.8 m/s cover 2.36 m per bite, so with a
/// 1.5 m advance the worm caught the rounded end of its own tunnel, got
/// pushed up its curve, and the next bite then followed the raised eye — the
/// tunnel slowly climbed into the rock. 2.5 m keeps the worm behind the front.
const DIG_ADVANCE: f32 = 2.5;

// --- rope -----------------------------------------------------------------
// The speed constants and the anchor rule live in `movement.rs`; the rope's
// shape is shared state, so its struct stays next to the worm.

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Rope {
    pub anchor: Vec3,
    /// Maximum distance from the hand to the anchor. The worm can swing at any
    /// shorter distance (slack rope) but is pulled back from a longer one.
    pub length: f32,
}

/// What the worm's body is doing. The movement rules are a state machine:
/// each action owns a few numbers (speed, timer) and decides when to hand over
/// to another. This is the pattern the book calls a State.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    Idle,
    Walk,
    Jump,
    /// The one mid-air jump. It can be used once per airtime.
    DoubleJump,
    Freefall,
    /// Bounced off a wall. Gravity still applies, but the height is fixed.
    WallKick,
    /// Holding into a wall: climb it. Press jump while climbing to kick off
    /// the wall.
    Climb,
    /// Hit by an explosion: no steering until the timer runs out.
    Tumble,
    Rope,
    /// The burst a worm makes when it starts running from a standstill (see
    /// `movement::step_dash`). It has no button: pushing the stick at rest is
    /// the trigger.
    Dash,
}

impl Action {
    pub fn name(self) -> &'static str {
        match self {
            Action::Idle => "idle",
            Action::Walk => "walk",
            Action::Jump => "jump",
            Action::DoubleJump => "double",
            Action::Freefall => "fall",
            Action::WallKick => "wall kick",
            Action::Climb => "climb",
            Action::Tumble => "tumble",
            Action::Rope => "rope",
            Action::Dash => "dash",
        }
    }
}

// Serialize: a snapshot carries whole worms. The `pub(crate)` fields are the
// sim's private bookkeeping (button edges, timers); only `World::step` reads
// them and a replica never steps, so they stay off the wire (`skip`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Worm {
    /// The point between the feet. The capsule stands on it.
    pub pos: Vec3,
    pub vel: Vec3,
    /// Where the worm looks (see `Input`). Aim and camera, not necessarily the
    /// way the body faces: the body turns to run, while the mouse can keep
    /// aiming somewhere else.
    pub yaw: f32,
    pub pitch: f32,
    /// The direction the shot leaves along, when it is not `yaw`/`pitch`.
    /// Zero means "no override: aim where the worm looks". The viewer sets
    /// this when its camera is off the look ray (see `Input::aim`).
    pub aim: Vec3,
    /// Way the body faces, in the same convention as `yaw`.
    pub face_yaw: f32,
    /// Horizontal speed along `face_yaw`. The scalar (not a vector) is what
    /// makes steering turn the body instead of strafing.
    pub forward_vel: f32,
    pub action: Action,
    pub on_ground: bool,
    /// Normal of the ground the worm stands on (up when it is in the air).
    pub ground_normal: Vec3,
    /// Ticks until the next dig is allowed.
    pub dig_cooldown: u8,
    /// Is the dig button down while there is rock in reach? (A pose hint for
    /// the viewer; the digging itself still happens here.)
    pub digging: bool,
    pub rope: Option<Rope>,
    /// Was the rope button down last tick? (To react to the press, not the hold.)
    #[serde(skip)]
    pub(crate) rope_was_down: bool,
    #[serde(skip)]
    pub(crate) jump_was_down: bool,
    /// Mid-air jumps left before landing. One, until the double jump is used.
    #[serde(skip)]
    pub(crate) air_jumps: u8,
    /// Ticks since the worm last stood on the ground (for coyote jumps).
    #[serde(skip)]
    pub(crate) coyote: u8,
    /// Ticks left on a jump press that could not be used yet (jump buffering).
    #[serde(skip)]
    pub(crate) jump_buffer: u8,
    /// The wall most recently touched, and how long a wall kick is still legal.
    #[serde(skip)]
    pub(crate) wall_normal: Vec3,
    #[serde(skip)]
    pub(crate) wall_timer: u8,
    /// Ticks before another wall can be grabbed (set after a kick, so a kick
    /// does not instantly re-stick to the same wall).
    #[serde(skip)]
    pub(crate) climb_cooldown: u8,
    /// Ticks of no steering left after an explosion.
    #[serde(skip)]
    pub(crate) tumble_timer: u8,
    /// Dash bookkeeping: was the stick pushed last tick (so a dash needs a
    /// fresh push, not a held stick), ticks of the burst left, and ticks until
    /// the next dash is allowed.
    #[serde(skip)]
    pub(crate) stick_was_down: bool,
    #[serde(skip)]
    pub(crate) dash_timer: u8,
    #[serde(skip)]
    pub(crate) dash_cooldown: u8,
    // --- fighting (see `combat.rs`) ---
    pub hp: f32,
    pub alive: bool,
    /// Seconds until a dead worm comes back.
    pub respawn_timer: f32,
    /// Seconds of spawn protection left.
    pub invuln: f32,
    /// Seconds of burning left (lava), and who lit it, for the kill credit.
    pub burn: f32,
    pub burn_by: u8,
    /// One entry per weapon in `ARSENAL`, in the same order.
    pub weapons: Vec<WeaponState>,
    /// Index into `weapons`.
    pub current: usize,
    /// The plasma-whip swing in progress, if any (see `combat::Swing`).
    pub swing: Option<Swing>,
    /// The overhead smash was used in this airtime. It is a stomp: one per
    /// double jump, so holding the trigger does not chain them. Cleared on landing.
    #[serde(skip)]
    pub(crate) smashed: bool,
    pub kills: i32,
    pub deaths: i32,
}

impl Worm {
    pub fn new(pos: Vec3) -> Worm {
        Worm {
            pos,
            vel: Vec3::ZERO,
            yaw: 0.0,
            pitch: 0.0,
            aim: Vec3::ZERO,
            face_yaw: 0.0,
            forward_vel: 0.0,
            action: Action::Idle,
            on_ground: false,
            ground_normal: Vec3::Y,
            dig_cooldown: 0,
            digging: false,
            rope: None,
            rope_was_down: false,
            jump_was_down: false,
            air_jumps: 1,
            coyote: 0,
            jump_buffer: 0,
            wall_normal: Vec3::Y,
            wall_timer: 0,
            climb_cooldown: 0,
            tumble_timer: 0,
            stick_was_down: false,
            dash_timer: 0,
            dash_cooldown: 0,
            hp: MAX_HP,
            alive: true,
            respawn_timer: 0.0,
            invuln: 0.0,
            burn: 0.0,
            burn_by: 0,
            weapons: ARSENAL.iter().map(|&id| WeaponState::full(id)).collect(),
            current: 0,
            swing: None,
            smashed: false,
            kills: 0,
            deaths: 0,
        }
    }

    /// An explosion push. It is a real velocity impulse, so it must not be
    /// overwritten by the next tick's locomotion: the worm tumbles instead.
    pub fn knockback(&mut self, impulse: Vec3) {
        self.vel += impulse;
        self.on_ground = false;
        self.rope = None;
        self.action = Action::Tumble;
        self.tumble_timer = 30;
    }

    /// Stand up again at `pos` with every movement state cleared (a respawn, or
    /// a teleport: the rope and the speed belong to the old place).
    pub(crate) fn reset_motion(&mut self, pos: Vec3) {
        *self = Worm {
            hp: self.hp,
            alive: self.alive,
            respawn_timer: self.respawn_timer,
            invuln: self.invuln,
            weapons: std::mem::take(&mut self.weapons),
            current: self.current,
            kills: self.kills,
            deaths: self.deaths,
            yaw: self.yaw,
            pitch: self.pitch,
            face_yaw: self.yaw,
            ..Worm::new(pos)
        };
    }

    /// Unit vector the worm looks along (the same maths as the viewer camera).
    /// `Input::aim` overrides it; `Vec3::ZERO` means there is no override.
    pub fn aim_dir(&self) -> Vec3 {
        if self.aim == Vec3::ZERO {
            look_dir(self.yaw, self.pitch)
        } else {
            self.aim
        }
    }

    pub fn eye(&self) -> Vec3 {
        self.pos + Vec3::Y * EYE_HEIGHT
    }

    pub fn hand(&self) -> Vec3 {
        self.pos + Vec3::Y * HAND_HEIGHT
    }

    /// Where the gun is: out to the right hand and a little forward of the body
    /// axis. The offsets mirror the viewer's standing arm pose (`hand_r` in
    /// `worms.rs`), so a shot starts where the gun is *drawn* rather than in the
    /// middle of the chest.
    pub fn muzzle(&self) -> Vec3 {
        let (s, c) = self.face_yaw.sin_cos();
        // The body's forward and right, the same convention as the viewer's
        // `basis`: for yaw 0 forward is +Z and right is -X.
        let face = Vec3::new(s, 0.0, c);
        let right = Vec3::new(-c, 0.0, s);
        self.hand() + right * HAND_RIGHT + face * HAND_FORWARD
    }

    /// The two ends of the body's axis: the centres of the foot and head
    /// spheres. Everything that asks "is this point near the worm?" (a bolt, a
    /// blast) measures the distance to this segment and subtracts the radius.
    pub fn axis(&self) -> (Vec3, Vec3) {
        body_axis(self.pos)
    }

    /// How far into its wind-up the weapon in hand is, 0..1. Always 0 for a
    /// weapon that fires on the click, and for a dead worm. The viewer draws
    /// the charge from this; a HUD would too.
    pub fn charge_fraction(&self) -> f32 {
        if !self.alive {
            return 0.0;
        }
        let w = &self.weapons[self.current];
        let windup = WEAPONS[w.id as usize].windup;
        if windup <= 0.0 {
            0.0
        } else {
            (w.charge / windup).clamp(0.0, 1.0)
        }
    }
}

/// `Worm::axis` for a worm standing with its feet at `feet`.
pub(crate) fn body_axis(feet: Vec3) -> (Vec3, Vec3) {
    (feet + Vec3::Y * WORM_RADIUS, feet + Vec3::Y * (WORM_HEIGHT - WORM_RADIUS))
}

/// Unit vector for a look direction, with the same convention as `Input`.
pub(crate) fn look_dir(yaw: f32, pitch: f32) -> Vec3 {
    let (sy, cy) = yaw.sin_cos();
    let (sp, cp) = pitch.sin_cos();
    Vec3::new(sy * cp, sp, cy * cp)
}

/// Does a standing worm with its feet at `feet` fit without touching rock?
pub(crate) fn body_fits(grid: &Grid, feet: Vec3) -> bool {
    BODY_SPHERES.iter().all(|&h| grid.sample(feet + Vec3::Y * h) >= WORM_RADIUS * 0.99)
}

/// Something that happened this tick, for the renderer, the audio and (later)
/// the network. Consumers read them with `World::drain_events`.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum Event {
    /// The terrain was edited. This (a shape and an operation, a few bytes) is
    /// also what the host will send to clients instead of the changed voxels.
    Edit { shape: Shape, op: Op },
    /// `worm` pulled the trigger of `weapon` (an id); `pos` is the muzzle.
    Shot { worm: u8, weapon: u8, pos: Vec3 },
    /// A beam weapon's line, for drawing.
    Beam { from: Vec3, to: Vec3 },
    /// `worm` has started a plasma-whip swing. `fwd`, `side`, `lean`, `style`
    /// and `flip` are the swing's fixed frame: with them the viewer rebuilds
    /// the same lash direction the sim hits and cuts along (see `whip_dir`),
    /// then plays it for `WHIP_SWING_TIME` seconds. The worm itself carries the
    /// lash every frame, so this event starts the swing animation, not the
    /// weapon. `flip` reverses the arc, so a held trigger alternates
    /// right-to-left and left-to-right instead of repeating one canned arc.
    Whip { worm: u8, from: Vec3, fwd: Vec3, side: Vec3, lean: f32, style: WhipStyle, flip: bool },
    /// `weapon` made the blast: the viewer, the audio and the camera each have
    /// their own signature for it (see `docs/WEAPON_FEEL.md`). `radius` is the
    /// damage reach, not the size of the fireball. `normal` is the surface the
    /// blast landed on, so the shock ring can lie on a wall or ceiling instead
    /// of pretending every explosion happens on flat ground.
    Explosion { weapon: u8, pos: Vec3, normal: Vec3, radius: f32 },
    /// `weapon` hurt `victim`. Carrying it lets a railgun hit spark differently
    /// from a blaster round. `dir` is the way the damage pushed the body (away
    /// from the shot or the blast), so the viewer can throw the blood that way.
    Hit { victim: u8, weapon: u8, pos: Vec3, damage: f32, dir: Vec3 },
    /// A projectile finished on rock instead of a worm: where it struck and the
    /// surface normal, so the viewer can spark, mark and voice the hit. Without
    /// this, most missed shots would be silent and invisible.
    Impact { weapon: u8, pos: Vec3, normal: Vec3 },
    Died { victim: u8, killer: u8, pos: Vec3 },
    /// `worm` jumped from `from` to `to` (the Teleport gun).
    Teleported { worm: u8, from: Vec3, to: Vec3 },
    Respawned { worm: u8 },
    /// `worm` reached the score limit: the match is over and the world is frozen.
    MatchWon { worm: u8 },
    /// A `Zone` appeared: the viewer draws its ring and the audio sounds it.
    ZoneStarted { kind: ZoneKind, pos: Vec3, radius: f32, seconds: f32 },
    /// A bit of lava turned to rock at `pos` (the terrain edit comes separately).
    LavaFroze { pos: Vec3 },
}

/// What a `Zone` does to whoever is inside it.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub enum ZoneKind {
    /// Gravity is multiplied by this (0.3 = moon, 2.5 = heavy).
    Gravity(f32),
}

/// A timed region of the world with an effect: a sphere that lives for `left`
/// seconds. Plain data (no behaviour, no pointers), so a snapshot can carry the
/// list to clients and they draw the same zones the host simulates.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Zone {
    pub kind: ZoneKind,
    pub centre: Vec3,
    pub radius: f32,
    pub left: f32,
    pub owner: u8,
}

pub struct World {
    /// The terrain. The renderer reads its dirty chunks after each step.
    pub grid: Grid,
    pub worms: Vec<Worm>,
    pub projectiles: Vec<Projectile>,
    /// Timed effects in the world (low gravity). They expire on their own.
    pub zones: Vec<Zone>,
    /// Molten lava, flowing. Each bit freezes into the terrain when it cools.
    pub lava: Vec<LavaBit>,
    /// The cheat: firing costs no ammo. Set by whoever hosts the match.
    pub infinite_ammo: bool,
    /// Kills that win the match: a rule, set by whoever creates the world.
    pub score_limit: i32,
    /// Who won, once somebody has. From then on `step` only counts ticks.
    pub winner: Option<u8>,
    /// The match has not begun: the players are still picking weapons, so
    /// nothing moves, shoots or respawns (like after a win, only the tick
    /// counts). Whoever hosts the match sets and clears it.
    pub choosing: bool,
    /// Where worms start, and come back after dying.
    pub spawns: Vec<Vec3>,
    /// Ticks simulated so far. Online, this is the shared clock that snapshots
    /// and inputs are labelled with ("tick 1234").
    pub tick: u32,
    pub(crate) events: Vec<Event>,
    /// Where and how big the explosions of the last tick were. Mines read it on
    /// the next tick, so a blast sets them off one tick later and a row of
    /// mines goes off as a ripple instead of all in one call stack.
    pub(crate) blasts: Vec<(Vec3, f32)>,
    pub(crate) rng: Rng,
}

/// Where a worm dropped from `from` (a point in open air; its y is the height
/// the drop starts at) lands: on the first ground below it.
fn drop_to_ground(grid: &Grid, from: Vec3) -> Vec3 {
    let y = grid.raycast(from, -Vec3::Y, from.y).map_or(0.0, |t| from.y - t);
    Vec3::new(from.x, y + 0.05, from.z)
}

impl World {
    /// One worm per spawn column `(x, z)`, dropped from the top of the world
    /// onto whatever ground is highest there. (The top must be open air.) The
    /// same spots are where a worm comes back after dying. `seed` starts the
    /// world's own random numbers (weapon spread and so on): the same seed and
    /// the same inputs always play out the same way.
    pub fn new(grid: Grid, spawns: &[(f32, f32)], seed: u32) -> World {
        let top = grid.size_metres().y - 0.5;
        let from: Vec<Vec3> = spawns.iter().map(|&(x, z)| Vec3::new(x, top, z)).collect();
        World::from_points(grid, &from, seed)
    }

    /// `new`, but each worm drops from its own point instead of from the top of
    /// the world: a map that is rock all the way up (tunnels) starts its worms
    /// inside, from a point in the open air of a chamber.
    pub fn from_points(grid: Grid, from: &[Vec3], seed: u32) -> World {
        let spawns: Vec<Vec3> = from.iter().map(|&p| drop_to_ground(&grid, p)).collect();
        let worms = spawns.iter().map(|&p| Worm::new(p)).collect();
        World {
            grid,
            worms,
            projectiles: Vec::with_capacity(64),
            zones: Vec::new(),
            lava: Vec::new(),
            infinite_ammo: false,
            score_limit: SCORE_LIMIT,
            winner: None,
            choosing: false,
            spawns,
            tick: 0,
            events: Vec::new(),
            blasts: Vec::new(),
            rng: Rng::new(seed),
        }
    }

    /// More places to come back at, without more worms: a match of two still
    /// respawns on one of many spots, not always the same two.
    pub fn add_respawn_points(&mut self, from: &[Vec3]) {
        let grid = &self.grid;
        self.spawns.extend(from.iter().map(|&p| drop_to_ground(grid, p)));
    }

    /// Give worm `i` the weapons `ids` (in that slot order) with full
    /// magazines, and take the first one in hand. The ids are the caller's to
    /// check (`weapons::valid_loadout`).
    pub fn set_loadout(&mut self, i: usize, ids: &[u8]) {
        let worm = &mut self.worms[i];
        worm.weapons = ids.iter().map(|&id| WeaponState::full(id)).collect();
        worm.current = 0;
    }

    /// Edit the terrain from outside the rules (the viewer's fly-mode brush) and
    /// report it like any other edit, so every copy of the world gets it.
    pub fn edit(&mut self, shape: Shape, op: Op) {
        self.grid.apply(shape, op);
        self.events.push(Event::Edit { shape, op });
    }

    pub fn drain_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }

    /// The gravity multiplier at `p`: 1.0 in open world, and the newest
    /// Gravity zone's scale inside one (zones do not stack, so two overlapping
    /// shifts cannot multiply into something absurd).
    pub fn gravity_at(&self, p: Vec3) -> f32 {
        self.zones
            .iter()
            .rev()
            .find_map(|z| match z.kind {
                ZoneKind::Gravity(scale) if p.distance(z.centre) < z.radius => Some(scale),
                _ => None,
            })
            .unwrap_or(1.0)
    }

    /// Advance the game by one tick (`DT`). `inputs[i]` drives worm `i`; a
    /// worm without an input stands still.
    pub fn step(&mut self, inputs: &[Input]) {
        if self.winner.is_some() || self.choosing {
            // Match over (or not begun): nothing moves, shoots or respawns. The tick still
            // counts, because snapshots are labelled with it and a replica
            // plays them back by it (a clock that stopped would stall it).
            self.tick += 1;
            return;
        }
        for i in 0..self.worms.len() {
            let input = clean(&inputs.get(i).copied().unwrap_or_default());
            if self.worms[i].alive {
                // Gravity is read at the middle of the body: a zone's edge
                // cuts the worm in half, not by where its feet happen to be.
                let gravity = self.gravity_at(self.worms[i].pos + Vec3::Y * (WORM_HEIGHT * 0.5));
                step_worm(&mut self.grid, &mut self.worms[i], &input, gravity, &mut self.events);
                self.step_weapons(i, &input);
                self.step_swing(i);
            } else {
                self.step_dead(i);
            }
        }
        self.step_projectiles();
        self.step_lava();
        self.step_burning();
        for z in &mut self.zones {
            z.left -= DT;
        }
        self.zones.retain(|z| z.left > 0.0);
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
    let aim = i.aim.and_then(|a| {
        let a = Vec3::new(finite(a.x), finite(a.y), finite(a.z));
        (a.length_squared() > 1.0e-8).then(|| a.normalize())
    });
    Input {
        move_dir: m.clamp_length_max(1.0),
        yaw: finite(i.yaw),
        pitch: finite(i.pitch).clamp(-PITCH_LIMIT, PITCH_LIMIT),
        aim,
        reel: finite(i.reel).clamp(-1.0, 1.0),
        ..*i
    }
}

fn step_worm(grid: &mut Grid, worm: &mut Worm, input: &Input, gravity: f32, events: &mut Vec<Event>) {
    // The rules (walk, jump, climb, rope) live in `movement`; the world stays
    // responsible for terrain edits.
    crate::movement::step(grid, worm, input, gravity);

    // Safety net: nothing legitimate leaves the world (outside counts as rock),
    // but a bug or a freak push must never lose a worm for good.
    let size = grid.size_metres();
    let lo = Vec3::new(WORM_RADIUS, 0.0, WORM_RADIUS);
    worm.pos = worm
        .pos
        .clamp(lo, Vec3::new(size.x - WORM_RADIUS, size.y - WORM_HEIGHT, size.z - WORM_RADIUS));

    // Dig. After the move, so the hole is where the worm is looking now.
    worm.dig_cooldown = worm.dig_cooldown.saturating_sub(1);
    let target = if input.dig { dig_target(grid, worm) } else { None };
    worm.digging = target.is_some();
    if let Some(centre) = target {
        if worm.dig_cooldown == 0 {
            // A capsule, not a sphere: each bite sweeps back along the look ray
            // to where the last one ended and forward to the next front, so the
            // tunnel is one smooth pipe. Spheres one radius apart leave a 0.2 m
            // cusp at every joint, and the worm bumps over each one.
            //
            // The forward end is `DIG_ADVANCE` past the current surface and the
            // tail is a radius behind it. Those are the same point on the next
            // bite, so consecutive capsules share an end (a smooth join) and
            // the front outruns the worm. The tail is always in air (the look
            // ray is air up to `centre`), so it cannot eat rock behind.
            let eye = worm.eye();
            let dir = look_dir(worm.yaw, worm.pitch);
            let back = DIG_RADIUS.min((centre - eye).length());
            let ahead = (DIG_ADVANCE - DIG_RADIUS).max(0.0);
            let shape = Shape::Capsule { a: centre - dir * back, b: centre + dir * ahead, radius: DIG_RADIUS };
            grid.apply(shape, Op::Carve);
            events.push(Event::Edit { shape, op: Op::Carve });
            worm.dig_cooldown = DIG_INTERVAL;
        }
    }
}
/// Where a dig would start: the first rock the worm looks at within reach.
/// The viewer uses this to preview the hole. The carve is a capsule from a
/// radius behind this point to `DIG_ADVANCE` past it, so the bite is a smooth
/// 2.5 m advance rather than a sphere dropped on the surface.
///
/// Straight out of the worm's face along the look angles, *not* along
/// `Worm::aim_dir`. The viewer's camera sits over the shoulder, so the crosshair
/// ray is a shoulder to the right and half a metre above the eye; a hole centred
/// on it would sit beside the worm (and, at point-blank range, above its head)
/// where the worm cannot walk into it. The mouth digs where the worm is going.
pub fn dig_target(grid: &Grid, worm: &Worm) -> Option<Vec3> {
    let dir = look_dir(worm.yaw, worm.pitch);
    grid.raycast(worm.eye(), dir, DIG_REACH).map(|t| worm.eye() + dir * t)
}

/// What a move touched. `wall` is the outward normal of the wall that took the
/// most speed, which the movement rules use for wall kicks and climbing.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Collision {
    pub ground: bool,
    pub wall: Option<Vec3>,
}

/// Resolve the overlaps between the body spheres and the terrain. Returns what
/// was touched (ground and/or a wall).
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
pub(crate) fn push_out_of_rock(grid: &Grid, worm: &mut Worm, roll_over_edges: bool) -> Collision {
    let mut out = Collision::default();
    // The wall that removed the most speed. A sphere in a corner can report
    // several; the one we ran into is the useful one.
    let mut wall_hit = 0.0;
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
            let into = worm.vel.dot(normal);
            // A near-vertical surface that we were moving into. Ignore floors,
            // ceilings and the shallow shoulder at the top of a slope.
            if !ground_like && normal.y > -0.3 && into < -wall_hit {
                wall_hit = -into;
                out.wall = Some(normal);
            }
            worm.pos += normal * (WORM_RADIUS - d).min(MAX_PUSH);
            // Remove the part of the velocity that points into the surface, keep
            // the rest: that is sliding along a wall.
            if into < 0.0 {
                worm.vel -= normal * into;
            }
            out.ground |= ground_like;
            pushed = true;
        }
        if !pushed {
            break;
        }
    }
    out
}

/// Move by `disp` in steps of at most `MAX_STEP`, pushing out of the rock after
/// each. Returns the union of what was touched.
pub(crate) fn slide(grid: &Grid, worm: &mut Worm, disp: Vec3) -> Collision {
    let steps = (disp.length() / MAX_STEP).ceil().max(1.0);
    let mut out = Collision::default();
    for _ in 0..steps as u32 {
        worm.pos += disp / steps;
        let hit = push_out_of_rock(grid, worm, false);
        out.ground |= hit.ground;
        if hit.wall.is_some() {
            out.wall = hit.wall;
        }
    }
    out
}

/// A worm walking into a ledge is stopped by the rounded foot touching its edge.
/// If the walk made less than half the horizontal distance it meant to
/// (`intended`), try a mantle from `STEP_UP` higher, `MANTLE_REACH` forward, then
/// lower the worm onto whatever is there. Keep the result only if it ends on a
/// flat top and got further. `want` is not the real velocity because the wall has
/// just cancelled that (and a worm starting to walk has not got up to speed
/// either).
pub(crate) fn try_step_up(grid: &Grid, worm: &mut Worm, start: Vec3, intended: f32, want: Vec3) -> Collision {
    let flat = |v: Vec3| Vec3::new(v.x, 0.0, v.z).length();
    if flat(worm.pos - start) >= intended * 0.5 {
        return Collision::default(); // not blocked enough to need help
    }
    let speed = want / DT;
    try_ledge_mantle(grid, worm, start, want, STEP_UP, speed, 0.0).unwrap_or_default()
}

/// Pull the worm onto a ledge it is touching: lift by `lift`, carry it
/// `MANTLE_REACH` along `forward`, then lower it onto a supported, roughly flat
/// top. `speed` is the horizontal velocity kept on success; `min_rise` rejects a
/// mantle that would only move the worm sideways or down. Returns `None` (and
/// puts everything back) when there is no ledge to mantles onto.
///
/// Used by the walk (`try_step_up`) and, from the air, by the ledge grab: the
/// jump that reaches a lip but hits its face should pull the worm over, not
/// bounce it back into the hole.
pub(crate) fn try_ledge_mantle(
    grid: &Grid,
    worm: &mut Worm,
    start: Vec3,
    forward: Vec3,
    lift: f32,
    speed: Vec3,
    min_rise: f32,
) -> Option<Collision> {
    let flat = |v: Vec3| Vec3::new(v.x, 0.0, v.z).length();
    let got = flat(worm.pos - start);
    let (blocked_pos, blocked_vel) = (worm.pos, worm.vel);
    worm.pos = start + Vec3::Y * lift;
    let fits = BODY_SPHERES.iter().all(|&h| grid.sample(worm.pos + Vec3::Y * h) >= WORM_RADIUS * 0.99);
    if fits && forward.length_squared() > 1.0e-6 {
        // A step-up is a mantle, not a walk: move at least `MANTLE_REACH` so the
        // worm ends up on top of the lip instead of with its foot still against
        // the face (a tick's walk is only ~0.2 m, half a radius).
        let step = forward.normalize() * forward.length().max(MANTLE_REACH);
        let mut hit = slide(grid, worm, step);
        // Lower the worm until its foot touches something, a little at a time. (Dropping
        // it the whole way and pushing it out again would shove it back off the
        // ledge's corner by as much as it had just moved forward.)
        let mut dropped = 0.0;
        while dropped < lift && grid.sample(worm.pos + Vec3::Y * WORM_RADIUS) > WORM_RADIUS + 0.02 {
            worm.pos.y -= 0.05;
            dropped += 0.05;
        }
        let landed = push_out_of_rock(grid, worm, true);
        hit.ground |= landed.ground;
        if landed.wall.is_some() {
            hit.wall = landed.wall;
        }
        // Only a mantle that ends on the ground is progress; stepping forward
        // over a drop is a fall, and the normal move already handles it. And
        // only onto a roughly flat top: otherwise the long mantle step could be
        // used to skate up any steep slope in 0.6 m leaps.
        let supported = grid.sample(worm.pos + Vec3::Y * WORM_RADIUS) <= WORM_RADIUS + 0.06;
        let onto_a_ledge = grid.gradient(worm.pos).y >= MANTLE_FLAT;
        let high_enough = worm.pos.y >= start.y + min_rise;
        if supported && onto_a_ledge && high_enough && flat(worm.pos - start) > got + 0.02 {
            worm.vel = Vec3::new(speed.x, worm.vel.y, speed.z);
            return Some(hit);
        }
    }
    worm.pos = blocked_pos;
    worm.vel = blocked_vel;
    None
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
pub(crate) fn settle_on_ground(grid: &Grid, worm: &mut Worm, stick: bool) -> bool {
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
    use std::f32::consts::{FRAC_PI_2, PI};

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
        World::new(Grid::from_fn(SIZE, f), &[spawn], 1)
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

    /// A flat world wide enough to get up to running speed without running
    /// into the edge. 24 chunks = 96 m of run-up room.
    fn big_flat() -> World {
        let mut w = World::new(Grid::from_fn(IVec3::new(24, 4, 8), flat), &[(6.0, 16.0)], 1);
        w.worms[0].pos.y = 4.05;
        run(&mut w, 60, Input::default());
        w
    }

    fn walk(yaw: f32) -> Input {
        Input { move_dir: Vec2::new(0.0, 1.0), yaw, ..Input::default() }
    }

    /// Drop a worm from 8 m up and say how far it fell in 20 ticks.
    fn fall_in_20_ticks(zone: Option<Zone>) -> f32 {
        let mut w = world(flat, (16.0, 16.0));
        w.worms[0].pos.y = 12.0;
        w.zones.extend(zone);
        run(&mut w, 20, Input::default());
        12.0 - w.worms[0].pos.y
    }

    fn gravity_zone(scale: f32, seconds: f32) -> Zone {
        Zone { kind: ZoneKind::Gravity(scale), centre: Vec3::new(16.0, 10.0, 16.0), radius: 14.0, left: seconds, owner: 0 }
    }

    #[test]
    fn a_worm_falls_slower_in_a_moon_zone_and_faster_in_a_heavy_one() {
        let normal = fall_in_20_ticks(None);
        let moon = fall_in_20_ticks(Some(gravity_zone(0.3, 8.0)));
        let heavy = fall_in_20_ticks(Some(gravity_zone(2.5, 8.0)));
        assert!(moon < normal * 0.5, "moon {moon} vs normal {normal}");
        assert!(heavy > normal * 1.5, "heavy {heavy} vs normal {normal}");
    }

    #[test]
    fn a_zone_expires_and_gravity_is_exactly_normal_again() {
        let mut w = world(flat, (16.0, 16.0));
        w.zones.push(gravity_zone(0.3, 0.5));
        let inside = Vec3::new(16.0, 10.0, 16.0);
        assert_eq!(w.gravity_at(inside), 0.3);
        assert_eq!(w.gravity_at(Vec3::new(40.0, 10.0, 16.0)), 1.0, "outside the sphere");
        run(&mut w, 29, Input::default());
        assert_eq!(w.zones.len(), 1, "still alive just before its time");
        run(&mut w, 2, Input::default());
        assert!(w.zones.is_empty());
        assert_eq!(w.gravity_at(inside), 1.0);
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
    fn walking_picks_up_speed_fast() {
        let mut w = settled(flat, (16.0, 16.0));
        let start = w.worms[0].pos;
        run(&mut w, 30, walk(0.0)); // yaw 0 looks along +z
        let moved = w.worms[0].pos - start;
        // Half a second from a standstill: full speed and ~7.6 m covered, the
        // 4 m start dash plus running. (Without the dash it was ~3.5 m, and a
        // 1.3 s ramp would cover less than half of that and make the worm feel
        // like it was wading.)
        assert!(moved.z > 6.5 && moved.z < 8.5, "half a second from rest: {moved:?}");
        assert!(moved.x.abs() < 0.01);
        assert!(w.worms[0].forward_vel > 10.5, "{}", w.worms[0].forward_vel);
    }

    #[test]
    fn stopping_and_reversing_take_a_fraction_of_a_second() {
        let mut w = settled(flat, (16.0, 16.0));
        run(&mut w, 60, walk(0.0));
        assert!(w.worms[0].forward_vel > 10.0);
        // Let go: stopped in 20 ticks (0.33 s) without sliding off.
        run(&mut w, 20, Input::default());
        assert!(w.worms[0].vel.length() < 0.05, "stopped: {:?}", w.worms[0].vel);

        // Full speed the other way, then press S: the body turns and runs back.
        let mut w = settled(flat, (16.0, 16.0));
        run(&mut w, 60, walk(0.0));
        let at = w.worms[0].pos.z;
        run(&mut w, 40, walk(PI)); // forward is -z now
        assert!(w.worms[0].pos.z < at - 2.0, "reversed: {} -> {}", at, w.worms[0].pos.z);
        assert!(w.worms[0].forward_vel > 10.0, "and running: {}", w.worms[0].forward_vel);
    }

    #[test]
    fn the_body_turns_toward_the_stick_instead_of_strafing() {
        let mut w = settled(flat, (16.0, 16.0));
        // Looking along +z, D means "right" of the camera: -x.
        run(&mut w, 45, Input { move_dir: Vec2::new(1.0, 0.0), ..Input::default() });
        let worm = &w.worms[0];
        assert!(worm.face_yaw < -1.2, "body turned right: {}", worm.face_yaw);
        assert!(worm.pos.x < 14.5, "and ran that way: {:?}", worm.pos);
        assert!(worm.yaw.abs() < 0.01, "the aim did not turn with the body");
    }

    #[test]
    fn a_worm_stops_when_the_input_stops() {
        let mut w = settled(flat, (16.0, 16.0));
        run(&mut w, 60, walk(0.0));
        run(&mut w, 40, Input::default());
        assert!(w.worms[0].vel.length() < 0.01, "{:?}", w.worms[0].vel);
    }

    /// A 1.2 m block: below the worm's head, so it is a wall to stop at, not a
    /// cliff to climb.
    fn low_wall(p: Vec3) -> f32 {
        (p.y - 4.0).min((p.y - 5.2).max(18.0 - p.x))
    }

    #[test]
    fn a_low_wall_stops_the_worm_and_it_does_not_get_inside() {
        let mut w = settled(low_wall, (16.0, 16.0));
        run(&mut w, 180, walk(FRAC_PI_2));
        let worm = &w.worms[0];
        let reach = 18.0 - WORM_RADIUS;
        assert!(worm.pos.x > reach - 0.1 && worm.pos.x < reach + 0.05, "x = {}", worm.pos.x);
        assert!(worm.on_ground);
        assert!(worm.pos.y < 4.6, "did not climb the 1.2 m block: y = {}", worm.pos.y);
    }

    #[test]
    fn a_tall_wall_is_climbed_by_holding_into_it() {
        let mut w = settled(walled, (16.0, 16.0));
        run(&mut w, 150, walk(FRAC_PI_2));
        let worm = &w.worms[0];
        assert_eq!(worm.action, Action::Climb, "holding W into the wall climbs");
        assert!(worm.pos.y > 7.0, "climbed: y = {}", worm.pos.y);
        assert!(worm.pos.x < 20.0 - WORM_RADIUS + 0.1, "stays on the face: x = {}", worm.pos.x);

        // S descends the same wall.
        let y = worm.pos.y;
        run(&mut w, 30, Input { move_dir: Vec2::new(0.0, -1.0), yaw: FRAC_PI_2, ..Input::default() });
        assert!(w.worms[0].pos.y < y - 0.5, "climbed down: {} -> {}", y, w.worms[0].pos.y);
    }

    #[test]
    fn digging_disables_wall_climbing() {
        let mut w = settled(walled, (17.5, 16.0));
        let dig_in = Input { yaw: FRAC_PI_2, dig: true, move_dir: Vec2::new(0.0, 1.0), ..Input::default() };
        let mut climbed = false;
        for _ in 0..120 {
            w.step(&[dig_in]);
            climbed |= w.worms[0].action == Action::Climb;
        }
        assert!(!climbed, "never grabs the wall while the dig button is down");
        // It does creep up a little: it is stepping up the lip of the hole it
        // just carved. A real climb is metres; this is a knee.
        assert!(w.worms[0].pos.y < 6.0, "tunnelled instead of climbing: y = {}", w.worms[0].pos.y);
    }

    #[test]
    fn digging_stops_at_solid_rock() {
        // The same wall, but the rock beyond it is the arena's solid frame.
        fn solid(p: Vec3) -> sdf::Material {
            if p.x > 17.0 { sdf::Material::Solid } else { sdf::Material::Rock }
        }
        let grid = Grid::from_fn_material(SIZE, walled, solid);
        let mut w = World::new(grid, &[(17.0, 16.0)], 1);
        w.worms[0].pos.y = 4.05;
        run(&mut w, 30, Input::default());
        let probe = Vec3::new(20.5, 5.5, 16.0);
        let before = w.grid.sample(probe);
        assert!(before < 0.0);
        let dig_in = Input { yaw: FRAC_PI_2, dig: true, ..Input::default() };
        run(&mut w, 120, dig_in);
        assert_eq!(w.grid.sample(probe), before, "the solid wall is untouched");
        assert_eq!(w.grid.material(probe), sdf::Material::Solid);
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
    fn the_jump_heights_step_up() {
        // Ground jump: 43 units/frame at 30 Hz, gravity 4.2 units/frame^2:
        // 14.5125^2 / (2 * 42.525) = 2.48 m. The double adds 53 -> 3.76 m
        // from wherever it is pressed.
        let mut w = settled(flat, (16.0, 16.0));
        let ground = w.worms[0].pos.y;
        let single = apex(&mut w, ground, 240, Input { jump: true, ..Input::default() });
        assert!((single - 2.48).abs() < 0.15, "single: {single}");

        let mut w = settled(flat, (16.0, 16.0));
        let ground = w.worms[0].pos.y;
        w.step(&[Input { jump: true, ..Input::default() }]);
        // Hold to the top of the first jump, then release so the next press
        // is a new button press (a held button does not double jump).
        run(&mut w, 20, Input { jump: true, ..Input::default() });
        w.step(&[Input::default()]);
        w.step(&[Input { jump: true, ..Input::default() }]);
        assert_eq!(w.worms[0].action, Action::DoubleJump);
        let top = apex(&mut w, ground, 120, Input { jump: true, ..Input::default() });
        assert!(top > single + 2.5 && top < single + 4.2, "single {single} -> double {top}");
    }

    #[test]
    fn there_is_one_double_jump_per_airtime() {
        let mut w = settled(flat, (16.0, 16.0));
        w.step(&[Input { jump: true, ..Input::default() }]);
        assert_eq!(w.worms[0].action, Action::Jump);
        // Release, then press again in mid-air: the double.
        w.step(&[Input::default()]);
        w.step(&[Input { jump: true, ..Input::default() }]);
        assert_eq!(w.worms[0].action, Action::DoubleJump);
        // Release and press a third time: nothing happens.
        w.step(&[Input::default()]);
        w.step(&[Input { jump: true, ..Input::default() }]);
        assert_eq!(w.worms[0].action, Action::DoubleJump, "no second air jump");
        assert_eq!(w.worms[0].air_jumps, 0);
    }

    #[test]
    fn pushing_the_stick_from_rest_starts_with_a_four_metre_dash() {
        // Camera yaw 0 looks along +Z; the stick points right (-X in this
        // convention is "right"), so move_dir.x = -1 sends the worm along +X.
        let mut w = big_flat();
        let x0 = w.worms[0].pos;
        let right = Input { move_dir: Vec2::new(-1.0, 0.0), ..Input::default() };
        w.step(&[right]);
        assert_eq!(w.worms[0].action, Action::Dash);
        assert!((w.worms[0].face_yaw - FRAC_PI_2).abs() < 1.0e-3, "the body snaps to the dash, no turn delay");
        run(&mut w, 11, right);
        let d = w.worms[0].pos - x0;
        assert!(d.x > 3.5 && d.x < 4.6, "dash distance {}", d.x);
        assert!(d.z.abs() < 0.3, "straight along the stick: {}", d.z);
        assert_ne!(w.worms[0].action, Action::Dash, "the burst is over");
        assert!(w.worms[0].forward_vel <= 12.0, "back to running speed: {}", w.worms[0].forward_vel);
    }

    #[test]
    fn there_is_no_dash_when_already_running_or_digging() {
        let mut w = big_flat();
        let fwd = Input { move_dir: Vec2::new(0.0, 1.0), yaw: FRAC_PI_2, ..Input::default() };
        run(&mut w, 40, fwd); // dash, then run on at full speed
        w.step(&[Input { yaw: FRAC_PI_2, ..Input::default() }]); // let go for a tick
        w.step(&[fwd]);
        assert_ne!(w.worms[0].action, Action::Dash, "still moving: not a start from rest");

        let mut w = big_flat();
        w.step(&[Input { dig: true, ..fwd }]);
        assert_ne!(w.worms[0].action, Action::Dash, "digging creeps, it does not burst");
    }

    #[test]
    fn a_held_stick_dashes_once_and_a_tap_has_a_cooldown() {
        let mut w = big_flat();
        let go = Input { move_dir: Vec2::new(0.0, 1.0), yaw: FRAC_PI_2, ..Input::default() };
        let stop = Input { yaw: FRAC_PI_2, ..Input::default() };
        w.step(&[go]);
        assert_eq!(w.worms[0].action, Action::Dash);
        run(&mut w, 60, go);
        assert_ne!(w.worms[0].action, Action::Dash, "holding the stick is not pushing it again");
        // Stop, and tap again 20 ticks after the first dash: still cooling down.
        run(&mut w, 20, stop);
        assert!(w.worms[0].forward_vel.abs() < 1.0, "at rest again");
        let mut w2 = big_flat();
        w2.step(&[go]);
        run(&mut w2, 25, stop);
        w2.step(&[go]);
        assert_ne!(w2.worms[0].action, Action::Dash, "tapping must not out-run running");
        run(&mut w2, 10, stop);
        w2.step(&[go]);
        assert_eq!(w2.worms[0].action, Action::Dash, "ready again after the cooldown");
    }

    #[test]
    fn jumping_out_of_a_dash_is_not_a_speed_boost() {
        let mut w = big_flat();
        w.step(&[Input { move_dir: Vec2::new(0.0, 1.0), ..Input::default() }]);
        assert_eq!(w.worms[0].action, Action::Dash);
        run(&mut w, 3, Input::default());
        w.step(&[Input { jump: true, ..Input::default() }]);
        assert_eq!(w.worms[0].action, Action::Jump);
        let speed = Vec2::new(w.worms[0].vel.x, w.worms[0].vel.z).length();
        assert!(speed <= 12.0, "a dash-jump leaves at running speed: {speed}");
    }

    #[test]
    fn landing_resets_the_double_jump() {
        let mut w = settled(flat, (16.0, 16.0));
        w.step(&[Input { jump: true, ..Input::default() }]);
        w.step(&[Input::default()]);
        w.step(&[Input { jump: true, ..Input::default() }]);
        assert_eq!(w.worms[0].action, Action::DoubleJump);
        for _ in 0..240 {
            w.step(&[Input::default()]);
            if w.worms[0].on_ground {
                break;
            }
        }
        assert!(w.worms[0].on_ground);
        assert_eq!(w.worms[0].air_jumps, 1, "the air jump comes back on landing");

        w.step(&[Input { jump: true, ..Input::default() }]);
        w.step(&[Input::default()]);
        w.step(&[Input { jump: true, ..Input::default() }]);
        assert_eq!(w.worms[0].action, Action::DoubleJump, "and works again");
    }

    #[test]
    fn a_double_jump_changes_no_horizontal_speed() {
        let mut w = settled(flat, (6.0, 16.0));
        let go = walk(FRAC_PI_2);
        run(&mut w, 60, go);
        w.step(&[Input { jump: true, ..go }]);
        // Fly a few ticks holding forward, then press again in mid-air.
        run(&mut w, 10, go);
        let before = Vec2::new(w.worms[0].vel.x, w.worms[0].vel.z).length();
        w.step(&[Input { jump: true, ..go }]);
        assert_eq!(w.worms[0].action, Action::DoubleJump);
        let after = Vec2::new(w.worms[0].vel.x, w.worms[0].vel.z).length();
        assert!((after - before).abs() < 1.0e-4, "not a speed boost: {before} -> {after}");
    }

    /// Run until the worm lands, returning the highest point above `ground`.
    fn jump_until_landed(w: &mut World, ground: f32, input: Input, ticks: usize) -> f32 {
        let mut top: f32 = 0.0;
        for _ in 0..ticks {
            w.step(&[input]);
            top = top.max(w.worms[0].pos.y - ground);
            if w.worms[0].on_ground {
                break;
            }
        }
        top
    }

    #[test]
    fn a_jump_changes_no_horizontal_speed() {
        let mut w = settled(flat, (6.0, 16.0));
        let go = walk(FRAC_PI_2);
        run(&mut w, 60, go);
        let before = w.worms[0].forward_vel;
        assert!(before > 11.0, "at a run: {before}");
        w.step(&[Input { jump: true, ..go }]);
        assert_eq!(w.worms[0].action, Action::Jump);
        let flat = Vec2::new(w.worms[0].vel.x, w.worms[0].vel.z).length();
        assert!((flat - before).abs() < 1.0e-4, "neither slower nor faster: {before} -> {flat}");
    }

    #[test]
    fn landing_does_not_hand_out_a_speed_boost() {
        let mut w = settled(flat, (6.0, 16.0));
        let go = walk(FRAC_PI_2);
        run(&mut w, 60, go);
        let before = w.worms[0].forward_vel;
        w.step(&[Input { jump: true, ..go }]);
        // Fly the whole arc holding W. The sim only jumps on the press, so the
        // held button does not turn this into a second jump.
        for _ in 0..240 {
            if w.worms[0].on_ground {
                break;
            }
            w.step(&[go]);
        }
        let landed = Vec2::new(w.worms[0].vel.x, w.worms[0].vel.z).length();
        assert!((landed - before).abs() < 0.1, "landed at the speed you left with: {before} -> {landed}");
        run(&mut w, 20, go);
        let after = Vec2::new(w.worms[0].vel.x, w.worms[0].vel.z).length();
        assert!((after - before).abs() < 0.1, "and it stays there: {after}");
    }

    #[test]
    fn hopping_is_not_a_faster_way_to_travel() {
        let go = walk(FRAC_PI_2);
        let travel = |hop: bool| {
            let mut w = big_flat();
            // Both start at a run, facing the way they will travel: the air
            // does not turn the body (a jump never does), so a standstill hop
            // would just drift sideways and prove nothing.
            run(&mut w, 60, go);
            let start = w.worms[0].pos.x;
            for i in 0..120 {
                // Pulse the button: a held button is one press, and a press in
                // mid-air does nothing (no air jump).
                w.step(&[Input { jump: hop && i % 2 == 0, ..go }]);
            }
            w.worms[0].pos.x - start
        };
        let run_ = travel(false);
        let hop = travel(true);
        assert!(run_ > 20.0, "running covers ground: {run_}");
        assert!(hop <= run_ + 0.3, "hopping is not a shortcut: run {run_}, hop {hop}");
    }

    #[test]
    fn air_steering_cannot_exceed_running_speed() {
        let mut w = settled(flat, (6.0, 16.0));
        let go = walk(FRAC_PI_2);
        run(&mut w, 60, go);
        w.step(&[Input { jump: true, ..go }]);
        let mut top: f32 = 0.0;
        for _ in 0..40 {
            // Hold forward AND sideways: the body keeps its facing, so the
            // sideways part is the drift that used to be free extra speed.
            w.step(&[Input { move_dir: Vec2::new(1.0, 1.0), yaw: FRAC_PI_2, ..Input::default() }]);
            top = top.max(Vec2::new(w.worms[0].vel.x, w.worms[0].vel.z).length());
        }
        assert!(top <= 12.16, "drift comes out of the budget: {top}");
    }

    #[test]
    fn walking_off_a_ledge_leaves_the_air_jump_available() {
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
        // Within the coyote window this is a ground jump, which is even better:
        // it leaves the real mid-air jump unused.
        assert_eq!(w.worms[0].action, Action::Jump, "a late jump off the edge still counts");
        assert!(w.worms[0].vel.y > 0.0, "it pushes up: {}", w.worms[0].vel.y);
        assert_eq!(w.worms[0].air_jumps, 1, "and the air jump is still there");
    }

    #[test]
    fn a_jump_pressed_just_before_landing_fires_on_touchdown() {
        let mut w = settled(flat, (16.0, 16.0));
        {
            // Falling the last half metre with no air jump left.
            let worm = &mut w.worms[0];
            worm.pos.y = 4.6;
            worm.vel.y = -8.0;
            worm.on_ground = false;
            worm.action = Action::Freefall;
            worm.air_jumps = 0;
            worm.coyote = 0;
        }
        w.step(&[Input { jump: true, ..Input::default() }]);
        for _ in 0..30 {
            w.step(&[Input::default()]);
            if w.worms[0].vel.y > 0.0 {
                break;
            }
        }
        assert!(w.worms[0].vel.y > 0.0, "the buffered jump fired on landing");
        assert_eq!(w.worms[0].action, Action::Jump);
    }

    #[test]
    fn jumping_at_a_climbed_wall_kicks_away_and_up() {
        let mut w = settled(walled, (16.0, 16.0));
        // 110 ticks, not more: the start dash reaches the wall sooner, and a
        // climb near the world's ceiling would clamp the kick's height.
        run(&mut w, 110, walk(FRAC_PI_2));
        assert_eq!(w.worms[0].action, Action::Climb);
        let y = w.worms[0].pos.y;
        w.step(&[Input { jump: true, ..walk(FRAC_PI_2) }]);
        let worm = &w.worms[0];
        assert_eq!(worm.action, Action::WallKick);
        assert!(worm.vel.y > 18.0, "up: {}", worm.vel.y);
        assert!(worm.pos.x < 20.0 - WORM_RADIUS - 0.01, "away from the wall: x = {}", worm.pos.x);
        let top = jump_until_landed(&mut w, 4.0, Input::default(), 300);
        assert!(top > y - 4.0 + 2.0, "the kick carries it higher: {top}");
    }

    #[test]
    fn climbing_over_the_top_pulls_the_worm_onto_the_ledge() {
        // Ground at y = 4, and a block from y = 4 to 8 for x > 18.
        fn block(p: Vec3) -> f32 {
            (p.y - 4.0).min((p.y - 8.0).max(18.0 - p.x))
        }
        let mut w = settled(block, (16.0, 16.0));
        run(&mut w, 300, walk(FRAC_PI_2));
        let worm = &w.worms[0];
        assert!(worm.pos.y > 7.9, "mantled onto the top: y = {}", worm.pos.y);
        assert!(worm.pos.x > 18.0, "and moved onto it: x = {}", worm.pos.x);
        assert!(worm.on_ground);
    }

    #[test]
    fn speed_from_the_air_survives_a_landing_while_steering_but_not_without_keys() {
        let landing = |input: Input| {
            let mut w = settled(flat, (6.0, 16.0));
            w.worms[0].pos.y += 0.3;
            w.worms[0].on_ground = false;
            w.worms[0].vel = Vec3::new(16.0, -2.0, 0.0);
            run(&mut w, 60, input);
            w.worms[0].vel.x
        };
        assert!(landing(walk(FRAC_PI_2)) > 10.0, "steering keeps momentum: {}", landing(walk(FRAC_PI_2)));
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
    fn a_fast_landing_on_a_slope_grounds_the_worm_instead_of_skimming() {
        // Hitting a slope turns horizontal speed into upward speed (the
        // collision pushes the body out along the surface). The landing test
        // used to read that as "taking off", so the worm rode the surface
        // without ever being on the ground: it could not jump, and only the
        // light air drag slowed it down. It skated up the whole hill.
        let mut w = World::new(Grid::from_fn(IVec3::new(16, 8, 8), ramp), &[(18.0, 16.0)], 1);
        let surface = 4.0 + 18.0 * 30f32.to_radians().tan();
        {
            let worm = &mut w.worms[0];
            worm.pos = Vec3::new(18.0, surface + 1.07, 16.0);
            worm.vel = Vec3::new(14.0, 0.0, 0.0);
            worm.on_ground = false;
            worm.action = Action::Freefall;
            worm.air_jumps = 0; // spent: a stuck worm cannot jump at all
        }
        let mut ticks = 0;
        while ticks < 60 && !w.worms[0].on_ground {
            w.step(&[Input::default()]);
            ticks += 1;
        }
        assert!(w.worms[0].on_ground, "landed on the hill");
        assert!(ticks < 10, "landed at once instead of skating along the slope: {ticks} ticks");
        // Touching the ground gives the air jump back.
        w.step(&[Input { jump: true, ..Input::default() }]);
        assert_eq!(w.worms[0].action, Action::Jump, "and can jump again");
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
    fn a_shot_can_aim_where_the_look_angles_do_not() {
        // The viewer's camera sits over the shoulder, so it sends the crosshair
        // direction separately from `yaw` (which stays the movement frame).
        let mut w = settled(walled, (17.5, 16.0));
        // Looking along +z, aiming at the wall along +x.
        let aim = Input { yaw: 0.0, aim: Some(Vec3::X), ..Input::default() };
        w.step(&[aim]);
        assert!((w.worms[0].aim_dir() - Vec3::X).length() < 1.0e-6);
        assert!(w.worms[0].yaw.abs() < 1.0e-6, "the camera frame did not turn");

        // No override: the look angles are in charge again.
        w.step(&[Input { yaw: FRAC_PI_2, ..Input::default() }]);
        assert!((w.worms[0].aim_dir() - Vec3::X).length() < 1.0e-6, "yaw pi/2 looks along +x");

        // A zero or non-finite override is a polite client's "no opinion".
        for bad in [Vec3::ZERO, Vec3::new(f32::NAN, 0.0, 0.0)] {
            w.step(&[Input { yaw: 0.0, aim: Some(bad), ..Input::default() }]);
            assert!((w.worms[0].aim_dir() - Vec3::Z).length() < 1.0e-6, "{bad:?} falls back to yaw");
        }
    }

    #[test]
    fn digging_follows_the_face_not_the_crosshair() {
        // The shoulder offset means the crosshair is not the eye ray. Shots
        // follow the crosshair (see above); the mouth digs where the body is
        // going, so the tunnel is one the worm can walk down.
        let mut w = settled(walled, (17.5, 16.0));
        let eye = w.worms[0].eye();
        assert!(w.grid.sample(Vec3::new(20.5, eye.y, 16.0)) < 0.0, "rock to the +x side");
        let input = Input { yaw: FRAC_PI_2, aim: Some(Vec3::Z), dig: true, ..Input::default() };
        w.step(&[input]);
        assert!(w.grid.sample(Vec3::new(20.5, eye.y, 16.0)) > 0.0, "dug straight ahead");
        assert!(w.worms[0].digging);
        assert!((w.worms[0].aim_dir() - Vec3::Z).length() < 1.0e-6, "the shot still follows the aim");
    }

    #[test]
    fn digging_is_rate_limited_and_needs_rock_within_reach() {
        // Looking at the sky: nothing to dig.
        let mut w = settled(flat, (16.0, 16.0));
        run(&mut w, 30, Input { pitch: 1.0, dig: true, ..Input::default() });
        assert!(w.drain_events().is_empty());
        assert!(!w.worms[0].digging, "nothing in reach, so no dig pose");
        // Digging into a wall while walking forward for 3 s: one dig per
        // DIG_INTERVAL ticks at most, and plenty of them.
        let mut w = settled(walled, (17.5, 16.0));
        let dig_in = Input { yaw: FRAC_PI_2, pitch: -0.15, dig: true, move_dir: Vec2::new(0.0, 1.0), ..Input::default() };
        run(&mut w, 180, dig_in);
        assert!(w.worms[0].digging, "the pose flag is on while rock is in reach");
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
        // The ground plus a block for x > 18 at 0.4 m, 0.8 m and 1.2 m.
        // (`Grid::from_fn` wants a plain fn pointer, so no closure over the height.)
        fn low(p: Vec3) -> f32 {
            (p.y - 4.0).min((p.y - 4.4).max(18.0 - p.x))
        }
        fn bump(p: Vec3) -> f32 {
            (p.y - 4.0).min((p.y - 4.8).max(18.0 - p.x))
        }
        fn high(p: Vec3) -> f32 {
            (p.y - 4.0).min((p.y - 5.2).max(18.0 - p.x))
        }
        let mut w = settled(low, (16.0, 16.0));
        run(&mut w, 60, walk(FRAC_PI_2));
        let worm = &w.worms[0];
        assert!(worm.pos.x > 19.0, "got up the 0.4 m ledge: x = {}", worm.pos.x);
        // The stored field is quantized to 0.25 m voxels, so the ledge sits
        // within half a voxel of 4.4. What matters is that it is UP there.
        assert!(worm.pos.y > 4.3 && worm.pos.y < 4.7 && worm.on_ground, "stands on it: y = {}", worm.pos.y);

        // A knee-high step from a weapon crater: the mantle has to carry the
        // body forward as well as up, or the foot stays against the face.
        let mut w = settled(bump, (16.0, 16.0));
        run(&mut w, 60, walk(FRAC_PI_2));
        let worm = &w.worms[0];
        assert!(worm.pos.x > 19.0 && worm.pos.y > 4.7, "got over the 0.8 m bump: {:?}", worm.pos);

        let mut w = settled(high, (16.0, 16.0));
        run(&mut w, 60, walk(FRAC_PI_2));
        assert!(w.worms[0].pos.x < 18.0 - WORM_RADIUS + 0.05, "a 1.2 m wall stops it: x = {}", w.worms[0].pos.x);
    }

    /// A pit 10 m deep and 6 m wide in ground at y = 10, centred on (16, 16).
    fn pit(p: Vec3) -> f32 {
        let r = ((p.x - 16.0).powi(2) + (p.z - 16.0).powi(2)).sqrt();
        (p.y - 10.0).max(3.0 - r)
    }

    #[test]
    fn a_worm_gets_out_of_a_pit_by_climbing_or_by_roping() {
        let towards_wall = Input { yaw: FRAC_PI_2, pitch: 1.2, move_dir: Vec2::new(0.0, 1.0), ..Input::default() };

        // Without the rope: hold into the wall and climb the 10 m out.
        let mut w = settled(pit, (16.0, 16.0));
        run(&mut w, 300, towards_wall);
        let worm = &w.worms[0];
        assert!(worm.pos.y > 9.9, "climbed out of the pit: y = {}", worm.pos.y);
        assert!(worm.on_ground, "standing above it");
        assert!(worm.pos.x > 16.0 + 3.2, "beyond the rim: x = {}", worm.pos.x);

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
        assert!(pumped > 11.0, "pumped swing {pumped}");
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
        assert!(ticks < 200, "took {ticks} ticks to walk {} m", deep - 19.0);
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
        assert!(worm.vel.length() <= 40.01);
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

    #[test]
    fn the_shoulder_offset_does_not_stop_the_worm_walking_into_its_tunnel() {
        // Mirrors the viewer's rig: the camera is 5 m back, 0.75 m to the right
        // and 0.55 m above the target, and `Input::aim` points from the eye at
        // whatever the camera's centre ray hits. Before the dig stopped using
        // the aim, the worm dug a hole beside and above itself, drifted into
        // the wall and got stuck 2 m in.
        let mut w = settled(walled, (17.5, 16.0));
        for _ in 0..300 {
            let worm = &w.worms[0];
            let eye = worm.eye();
            let forward = Vec3::X;
            let right = forward.cross(Vec3::Y);
            let cam = worm.pos + Vec3::Y * (EYE_HEIGHT + 0.2 + 0.35) + right * 0.75 - forward * 5.0;
            let aim = w.grid.raycast(cam, forward, 100.0).map(|t| (cam + forward * t - eye).normalize());
            w.step(&[Input {
                yaw: FRAC_PI_2,
                aim,
                dig: true,
                move_dir: Vec2::new(0.0, 1.0),
                ..Input::default()
            }]);
        }
        let worm = &w.worms[0];
        assert!(worm.pos.x > 28.0, "walked into its tunnel, x = {}", worm.pos.x);
        assert!((worm.pos.z - 16.0).abs() < 0.5, "and straight, z = {}", worm.pos.z);
    }

    #[test]
    fn digging_does_not_bob_the_worm() {
        // A straight tunnel through a hill. The old spheres left a 0.2 m cusp
        // at every joint: the worm bumped over them, the next bite followed its
        // raised eye, and the tunnel climbed away from the floor.
        let mut w = World::new(Grid::from_fn(IVec3::new(24, 4, 8), walled), &[(17.5, 16.0)], 1);
        w.worms[0].pos.y = 4.05;
        run(&mut w, 60, Input::default());
        let dig_in = Input { yaw: FRAC_PI_2, dig: true, move_dir: Vec2::new(0.0, 1.0), ..Input::default() };
        run(&mut w, 120, dig_in); // inside the rock and up to speed
        let start = w.worms[0].pos;
        run(&mut w, 180, dig_in);
        let end = w.worms[0].pos;
        assert!(end.x - start.x > 30.0, "kept walking while digging: {:.1} -> {:.1}", start.x, end.x);
        assert!((end.y - start.y).abs() < 0.2, "the tunnel floor stays level: {:.3} -> {:.3}", start.y, end.y);
    }

    #[test]
    fn weapon_craters_do_not_trap_the_worm() {
        // Three overlapping blast holes in flat ground: a shallow bowl whose
        // scalloped rim used to stop the worm dead. It should walk up the
        // steep wall and mantle the lip.
        let mut grid = Grid::from_fn(IVec3::new(8, 4, 8), flat);
        for x in [14.0, 17.0, 20.0] {
            grid.apply(Shape::Sphere { center: Vec3::new(x, 4.0, 16.0), radius: 3.0 }, Op::Carve);
        }
        let mut w = World::new(grid, &[(17.0, 16.0)], 1);
        run(&mut w, 90, Input::default()); // fall in
        assert!(w.worms[0].pos.y < 2.5, "starts at the bottom: {}", w.worms[0].pos.y);
        run(&mut w, 300, walk(FRAC_PI_2));
        let worm = &w.worms[0];
        assert!(worm.pos.y > 3.8, "climbed out: y = {}", worm.pos.y);
        assert!(worm.pos.x > 24.0, "and kept going: x = {}", worm.pos.x);
    }

    #[test]
    fn a_jump_grabs_a_ledge_above_the_step_height() {
        // A 2.2 m wall: too tall to walk up (STEP_UP is 0.9), and the jump hits
        // its face before the feet clear the top. The grab should pull the worm
        // over instead of cancelling its speed and dropping it back.
        fn wall(p: Vec3) -> f32 {
            (p.y - 4.0).min((p.y - 6.2).max(18.0 - p.x))
        }
        let mut w = settled(wall, (17.4, 16.0));
        let jump_in = Input { yaw: FRAC_PI_2, move_dir: Vec2::new(0.0, 1.0), jump: true, ..Input::default() };
        run(&mut w, 30, jump_in);
        let worm = &w.worms[0];
        assert!(worm.pos.y > 6.1 && worm.on_ground, "grabbed the ledge: {:?}", worm.pos);
        assert!(worm.pos.x > 18.0, "and is standing on top of it: {:?}", worm.pos);
    }
}
