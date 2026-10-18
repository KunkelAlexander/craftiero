use glam::Vec2;
use serde::{Deserialize, Serialize};

/// Everything a player can ask the game to do during one tick.
///
/// Pattern: Command. A button press becomes a plain data object instead of a
/// direct call like `worm.jump()`. Because it is just data:
///   * the keyboard, a bot and (later) a packet from a friend all produce the
///     same thing, so the sim has ONE code path for all of them;
///   * it can be sent over the network, stored, or replayed in a test.
///
/// Networking note: the sim treats an Input as *untrusted*. A cheating client
/// can send any numbers (even NaN), so the sim clamps and sanitises them
/// instead of assuming they are polite. See `World::step`.
#[derive(Clone, Copy, Default, Debug, PartialEq, Serialize, Deserialize)]
pub struct Input {
    /// Increases by one per input a client produces. The sim ignores it; the
    /// network layer uses it later to drop stale/duplicate packets.
    pub seq: u32,
    /// Desired movement relative to where the worm looks: x = right,
    /// y = forward. Length 0..=1 (the sim clamps it).
    pub move_dir: Vec2,
    /// Where the worm looks, as absolute angles in radians (same convention as
    /// the viewer's camera: yaw 0 looks along +Z, turning left is positive,
    /// pitch up is positive). Angles instead of a mouse position keep the input
    /// independent of screen size and camera. They are absolute instead of
    /// "turn by this much" so that a lost packet does not leave a client and
    /// the host disagreeing about where the worm looks (state beats events on
    /// an unreliable network).
    pub yaw: f32,
    pub pitch: f32,
    /// Jump button held. The sim jumps only while standing on something.
    pub jump: bool,
    /// Dig button held: carves the rock the worm looks at, a few times a second.
    pub dig: bool,
    /// Rope button held. The sim reacts to the press (the moment it goes
    /// down), not the hold: press to shoot, press again to let go.
    pub rope: bool,
    /// Reel the rope: +1 shorter, -1 longer.
    pub reel: f32,
    /// Trigger held: fires the current weapon whenever it is ready.
    pub fire: bool,
    /// The weapon wanted, as an id (see `weapons`). Sent every tick like the
    /// aim, not as "next weapon" events, for the same reason: a lost packet
    /// cannot leave the client and the host holding different guns. An id the
    /// sim does not know is ignored.
    pub weapon: u8,
}
