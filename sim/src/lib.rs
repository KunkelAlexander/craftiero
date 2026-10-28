// The `sim` crate is the game's rules and nothing else.
//
// What it must NEVER depend on: windows, GPU, audio, sockets, wall-clock time.
// Its whole interface is:
//
//     World::step(&mut self, inputs: &[Input])     <- inputs in
//     world.worms / world.grid                     <- state out (read-only for others)
//     world.drain_events()                         <- things that just happened
//
// Rendering, sound and (later) networking are all *consumers* of that
// interface. This separation is what lets the same code run in a browser tab,
// in a bot, in a unit test, or on a dedicated server.
//
// M2 has the worm: walking, jumping, digging and the rope. M3 adds weapons,
// projectiles, explosions that carve the terrain, damage and respawning. Bots
// from the 2D game are in `sim/old2d/` until their milestone (M6).

pub mod combat;
pub mod input;
pub mod lava;
pub mod movement;
pub mod rng;
pub mod weapons;
pub mod world;

pub use glam::{vec3, Vec3};
pub use combat::{
    whip_arc, whip_dir, whip_frame, whip_reach, AimPath, Projectile, Swing, WhipStyle, MAX_HP, RESPAWN_TIME,
    WHIP_ACTIVE, WHIP_LEAN, WHIP_REST, WHIP_SMASH_DOWN, WHIP_SMASH_UP, WHIP_SWEEP, WHIP_SWING_TIME,
};
pub use input::Input;
pub use lava::{LavaBit, STAMP_RADIUS};
pub use weapons::{WeaponDef, WeaponState, ARSENAL, WEAPONS};
pub use world::{
    dig_target, Action, Event, Rope, World, Worm, Zone, ZoneKind, DIG_RADIUS, DT, EYE_HEIGHT,
    WORM_HEIGHT, WORM_RADIUS,
};
