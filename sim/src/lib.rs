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
// M2 has the worm: walking, jumping, digging and the rope. Weapons, projectiles
// and bots from the 2D game are in `sim/old2d/` until their milestones (M3+).

pub mod input;
pub mod rng;
pub mod world;

pub use glam::{vec3, Vec3};
pub use input::Input;
pub use world::{
    dig_target, Event, Rope, World, Worm, DIG_RADIUS, DT, EYE_HEIGHT, WORM_HEIGHT, WORM_RADIUS,
};
