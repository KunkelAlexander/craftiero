// The host: the authority of a match.
//
// "Host" is a ROLE, not a place. This struct runs the real `World`, collects
// the players' inputs and produces the snapshots everybody else draws. Where it
// runs is somebody else's problem:
//
//   * offline / loopback: inside the game process, wired straight to the local
//     client (today);
//   * one player's computer, with friends connected through a relay (M5; the
//     `Session` in session.rs is the glue between this struct and the relay);
//   * a dedicated server binary that links this crate (maybe, one day).
//
// To make that possible it knows nothing about windows, sockets or clocks. Its
// whole interface is plain function calls with already-decoded messages:
//
//     join()                  -> Welcome        a new player arrives
//     leave(id)                                 a player goes away
//     handle(id, ClientMsg)                     a message arrived from player `id`
//     tick()                  -> Snapshot       advance 1/60 s; the world afterwards
//
// The code around it moves bytes between these calls and the wire.
//
// `tick` returns a snapshot EVERY tick. How often a snapshot is really sent is
// a property of each link (the local player can have all 60 per second for
// free, a friend across the internet gets fewer), so it is decided by whoever
// owns the link, using a `Throttle`.

pub mod session;
pub mod throttle;

pub use session::Session;
pub use throttle::Throttle;

use protocol::{ClientMsg, PlayerId, Snapshot, Welcome};
use sdf::{Grid, Op, Shape};
use sim::{Event, Input, World};

/// Who drives a worm slot.
///
/// Pattern: State. A slot is either empty or a connected human, and what
/// `tick` does depends on which, without scattering `if is_human` flags.
enum Controller {
    /// Nobody: the worm stands there as a target dummy. (The 2D game put a bot
    /// here; they come back with M6, and become a second kind of controller.)
    Idle,
    /// A connected human: their most recent input.
    Human(Input),
}

pub struct Host {
    world: World,
    controllers: Vec<Controller>,
    /// Every terrain edit since the match began, in order: what a late joiner
    /// replays on top of the untouched map (see `Welcome::edits`).
    edits: Vec<(Shape, Op)>,
}

impl Host {
    /// A new match on `map` with a worm for each spawn column, all idle until
    /// humans join.
    pub fn new(map: Grid, spawns: &[(f32, f32)], seed: u32) -> Host {
        let world = World::new(map, spawns, seed);
        let controllers = spawns.iter().map(|_| Controller::Idle).collect();
        Host { world, controllers, edits: Vec::new() }
    }

    /// A human arrives: give them the first slot nobody holds.
    /// `None` means the match is full.
    pub fn join(&mut self) -> Option<Welcome> {
        let slot = self.controllers.iter().position(|c| matches!(c, Controller::Idle))?;
        self.controllers[slot] = Controller::Human(Input::default());
        Some(self.welcome(slot as PlayerId))
    }

    /// The Welcome again for a player who already has a slot: a rematch, or a
    /// client whose terrain went wrong and asked to start over
    /// (`ClientMsg::Resync`). `None` if nobody holds that slot.
    pub fn rewelcome(&self, id: PlayerId) -> Option<Welcome> {
        match self.controllers.get(id as usize) {
            Some(Controller::Human(_)) => Some(self.welcome(id)),
            _ => None,
        }
    }

    fn welcome(&self, you: PlayerId) -> Welcome {
        // No events: the edit log is already up to date, and replaying
        // explosions from before the newcomer arrived would be wrong.
        Welcome { you, edits: self.edits.clone(), snapshot: self.snapshot(Vec::new()) }
    }

    /// A human goes away: the worm stays as a dummy, so the match carries on.
    pub fn leave(&mut self, id: PlayerId) {
        if let Some(c) = self.controllers.get_mut(id as usize) {
            *c = Controller::Idle;
        }
    }

    /// A message arrived from player `id`.
    ///
    /// `id` comes from the transport (it knows which connection the bytes came
    /// from), NOT from the message. A client therefore cannot pose as another
    /// player.
    pub fn handle(&mut self, id: PlayerId, msg: ClientMsg) {
        match msg {
            ClientMsg::Input(input) => {
                // Only humans are driven by messages. Ignore anyone else.
                if let Some(Controller::Human(latest)) = self.controllers.get_mut(id as usize) {
                    // Keep only the newest input: if two arrive between ticks
                    // the older one is already out of date.
                    *latest = input;
                }
            }
            // A ping tests the connection, not the game, so whoever owns the
            // connection answers it (the host session), not this struct.
            // A resync is the same kind of thing: the session sends the reply
            // (`rewelcome`) down that client's own connection.
            ClientMsg::Ping { .. } | ClientMsg::Resync => {}
        }
    }

    /// Edit the terrain on behalf of a developer tool (the viewer's fly-mode
    /// brush). It is NOT a client message: a real client can never edit the
    /// world, only ask for things the rules allow. The edit goes out with the
    /// next snapshot like any other.
    pub fn debug_edit(&mut self, shape: Shape, op: Op) {
        self.world.edit(shape, op);
    }

    /// The infinite-ammo cheat, for the viewer's cheat code. Like `debug_edit`,
    /// it is a local developer switch and not something a client can ask for.
    /// Returns the new state.
    pub fn toggle_infinite_ammo(&mut self) -> bool {
        self.world.infinite_ammo = !self.world.infinite_ammo;
        self.world.infinite_ammo
    }

    /// Advance the match by one tick. Returns the world as it is now, plus the
    /// events of this tick. Sending it (or not) is up to the caller.
    pub fn tick(&mut self) -> Snapshot {
        // Pattern: Command. Every controller turns into the same `Input`.
        // A human whose packet is late simply repeats their last input.
        let inputs: Vec<Input> = self
            .controllers
            .iter()
            .map(|c| match c {
                Controller::Idle => Input::default(),
                Controller::Human(input) => *input,
            })
            .collect();

        self.world.step(&inputs);
        let events = self.world.drain_events();
        self.edits.extend(events.iter().filter_map(|e| match *e {
            Event::Edit { shape, op } => Some((shape, op)),
            _ => None,
        }));
        self.snapshot(events)
    }

    /// Read-only view of the real world (for tests and debugging).
    pub fn world(&self) -> &World {
        &self.world
    }

    fn snapshot(&self, events: Vec<Event>) -> Snapshot {
        Snapshot {
            tick: self.world.tick,
            worms: self.world.worms.clone(),
            projectiles: self.world.projectiles.clone(),
            zones: self.world.zones.clone(),
            lava: self.world.lava.clone(),
            events,
            edits_total: self.edits.len() as u32,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::{IVec3, Vec2, Vec3};
    use protocol::replica::Replica;
    use protocol::{
        decode_client, decode_host, encode_client, encode_host, welcome_messages, HostMsg, WelcomeBuilder,
    };
    use sim::weapons::ROCKET;
    use std::f32::consts::FRAC_PI_2;

    /// Flat ground at y = 4 m, 32 x 16 x 32 m of world.
    fn ground() -> Grid {
        Grid::from_fn(IVec3::new(8, 4, 8), |p| p.y - 4.0)
    }

    /// Two worms on the ground (the spawn drop finds rock at once, so put them
    /// on the surface by hand, like the sim's own tests do).
    fn host() -> Host {
        let mut host = Host::new(ground(), &[(8.0, 16.0), (24.0, 16.0)], 7);
        for w in &mut host.world.worms {
            w.pos.y = 4.05;
        }
        host
    }

    /// Send a host message through the byte encoding, like a real wire would.
    fn over_the_wire(msg: &HostMsg) -> HostMsg {
        decode_host(&encode_host(msg)).expect("host message survives encoding")
    }

    /// A welcome the way it really travels: cut into messages, each one
    /// encoded and decoded, and put back together by the client.
    fn over_the_wire_in_pieces(welcome: protocol::Welcome) -> protocol::Welcome {
        let mut builder = WelcomeBuilder::default();
        let mut done = None;
        for msg in welcome_messages(welcome) {
            done = builder.push(&over_the_wire(&msg));
        }
        done.expect("the last piece completes the welcome")
    }

    fn join_as_client(host: &mut Host) -> Replica {
        let welcome = host.join().expect("a free slot");
        Replica::new(over_the_wire_in_pieces(welcome), ground(), 0)
    }

    /// One tick of the whole pipeline: input out as bytes, snapshot back as bytes.
    fn tick(host: &mut Host, replica: &mut Replica, input: Input) {
        let sent = decode_client(&encode_client(&ClientMsg::Input(input))).unwrap();
        host.handle(replica.me, sent);
        replica.apply(over_the_wire(&HostMsg::Snapshot(host.tick())));
        replica.advance(0.0); // what the frame loop does before drawing
    }

    #[test]
    fn replica_follows_the_host_through_bytes() {
        let mut host = host();
        let mut replica = join_as_client(&mut host);
        assert_eq!(replica.me, 0);

        // Walk +x for a second.
        let walk = Input { move_dir: Vec2::new(0.0, 1.0), yaw: FRAC_PI_2, ..Input::default() };
        let start_x = replica.world.worms[0].pos.x;
        for _ in 0..60 {
            tick(&mut host, &mut replica, walk);
        }

        assert!(replica.world.worms[0].pos.x > start_x + 3.0, "worm 0 should have walked");
        // With a snapshot every tick, the copy is exactly the original.
        for (a, b) in replica.world.worms.iter().zip(&host.world().worms) {
            assert_eq!(a.pos, b.pos);
            assert_eq!(a.hp, b.hp);
            assert_eq!(a.action, b.action);
        }
        assert_eq!(replica.world.tick, host.world().tick);
    }

    #[test]
    fn craters_reach_the_replica() {
        let mut host = host();
        let mut replica = join_as_client(&mut host);
        let untouched = replica.world.grid.checksum();

        // Shoot the ground at our feet until the rocket has landed.
        let shoot = Input { fire: true, pitch: -1.3, weapon: ROCKET, ..Input::default() };
        for _ in 0..120 {
            tick(&mut host, &mut replica, shoot);
        }

        assert_ne!(host.world().grid.checksum(), untouched, "the rocket dug a hole");
        // Only the *events* can have changed the replica's grid.
        assert_eq!(replica.world.grid.checksum(), host.world().grid.checksum());
    }

    #[test]
    fn a_late_joiner_gets_the_current_terrain() {
        let mut host = host();
        host.join(); // slot 0 plays a while
        let shape = Shape::Sphere { center: Vec3::new(16.0, 4.0, 16.0), radius: 3.0 };
        host.debug_edit(shape, Op::Carve);
        host.tick();
        let replica = join_as_client(&mut host);
        assert_eq!(replica.me, 1);
        assert_eq!(replica.world.grid.checksum(), host.world().grid.checksum());
    }

    /// The case the old single-message Welcome could not carry: a match that
    /// has been dug up thousands of times. (The edits are made through the
    /// host, as a long match would, and cover all of the ground.)
    #[test]
    fn a_late_joiner_can_join_a_match_with_thousands_of_edits() {
        let mut host = host();
        host.join();
        for i in 0..3000 {
            let x = 1.0 + (i % 60) as f32 * 0.4;
            let z = 1.0 + (i / 60) as f32 * 0.5;
            host.debug_edit(Shape::Sphere { center: Vec3::new(x, 4.0, z), radius: 0.6 }, Op::Carve);
            if i % 10 == 0 {
                host.tick(); // edits reach the log when a tick drains them
            }
        }
        host.tick();
        let welcome = host.rewelcome(0).expect("slot 0 is held");
        assert_eq!(welcome.edits.len(), 3000);
        assert!(encode_host(&HostMsg::Snapshot(welcome.snapshot.clone())).len() < 64 * 1024);

        let replica = join_as_client(&mut host);
        assert_eq!(replica.world.grid.checksum(), host.world().grid.checksum());
        assert!(!replica.desynced());
    }

    #[test]
    fn a_resync_gives_a_held_slot_a_fresh_welcome() {
        let mut host = host();
        assert!(host.rewelcome(0).is_none(), "nobody holds slot 0 yet");
        host.join();
        host.debug_edit(Shape::Sphere { center: Vec3::new(16.0, 4.0, 16.0), radius: 3.0 }, Op::Carve);
        host.tick();
        let w = host.rewelcome(0).expect("slot 0 is held");
        assert_eq!(w.you, 0);
        assert_eq!(w.edits.len(), 1);
        assert_eq!(w.snapshot.edits_total, 1);
        assert!(host.rewelcome(1).is_none(), "the idle slot gets nothing");
    }

    #[test]
    fn join_leave_and_full_match() {
        let mut host = host();
        assert_eq!(host.join().unwrap().you, 0);
        assert_eq!(host.join().unwrap().you, 1);
        assert!(host.join().is_none(), "no free slot left");
        host.leave(0);
        assert_eq!(host.join().unwrap().you, 0, "the slot was given back");
    }

    #[test]
    fn inputs_for_empty_slots_are_ignored() {
        let mut host = host();
        // Nobody joined, so slot 1 is a dummy. A forged input must not move it.
        let forged = Input { move_dir: Vec2::new(0.0, 1.0), ..Input::default() };
        host.handle(1, ClientMsg::Input(forged));
        let start = host.world().worms[1].pos;
        for _ in 0..30 {
            host.tick();
        }
        // (It settles onto the ground, so only the horizontal position counts.)
        let end = host.world().worms[1].pos;
        assert_eq!((end.x, end.z), (start.x, start.z));
    }

    /// A bandwidth budget as a test: if someone adds a fat field to
    /// `Snapshot` or `Worm`, this fails and the cost shows up before the
    /// players feel it.
    #[test]
    fn one_second_of_snapshots_fits_the_budget() {
        let mut host = host();
        let mut every_tick = 0;
        let mut every_second_tick = 0;
        let mut throttle = Throttle::new(2);
        for _ in 0..60 {
            let snapshot = host.tick();
            every_tick += encode_host(&HostMsg::Snapshot(snapshot.clone())).len();
            if let Some(s) = throttle.push(snapshot) {
                every_second_tick += encode_host(&HostMsg::Snapshot(s)).len();
            }
        }
        println!("2 worms, 60 snapshots/s: {every_tick} B/s,  30 snapshots/s: {every_second_tick} B/s");
        // A worm is ~450 bytes on the wire, most of it the 15 weapon magazines.
        // Fine for the free loopback; M5 must shrink it before four players
        // share a home upload (send the weapon table only when it changes).
        assert!(every_tick < 70_000, "snapshots grew to {every_tick} B/s");
        // Events ride along, so halving the rate saves a bit less than half.
        assert!(every_second_tick * 10 < every_tick * 6);
    }

    #[test]
    fn garbage_bytes_are_dropped_not_fatal() {
        assert!(decode_client(&[1, 2, 3]).is_none());
        assert!(decode_host(&vec![0xFF; 100_000]).is_none());
    }
}
