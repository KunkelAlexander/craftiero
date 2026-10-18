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
pub mod rng;
pub mod weapons;
pub mod world;

pub use glam::{vec3, Vec3};
pub use combat::{Projectile, MAX_HP, RESPAWN_TIME};
pub use input::Input;
pub use weapons::{WeaponDef, WeaponState, ARSENAL, WEAPONS};
pub use world::{
    dig_target, Event, Rope, World, Worm, DIG_RADIUS, DT, EYE_HEIGHT, WORM_HEIGHT, WORM_RADIUS,
};
