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
use sim::map::{spawn_points, MAX_PLAYERS};
use sim::weapons::{valid_loadout, DEFAULT_LOADOUT};
use sim::{Event, Input, Vec3, World};

/// How long the players may take to pick their weapons before the match begins
/// without the ones who have not: 45 s of ticks.
const CHOOSE_TICKS: u32 = 45 * 60;

/// Picks `count` (x, z) columns for worms to start at on map number `map`, from
/// a seed. The demo maps' is `sim::map::spawn_points`; tests with a small flat
/// map bring their own.
pub type Places = fn(map: u8, seed: u32, count: usize) -> Vec<Vec3>;

/// `Places` for a small flat test map: up to eight fixed spots, the same for
/// every seed. (The demo map's valley does not fit in a test's 32 m of ground.)
pub fn flat_places(_map: u8, _seed: u32, count: usize) -> Vec<Vec3> {
    let all = [(8.0, 16.0), (24.0, 16.0), (16.0, 8.0), (16.0, 24.0), (10.0, 10.0), (22.0, 22.0), (10.0, 22.0), (22.0, 10.0)];
    // They drop from the top of the 16 m high test map.
    all[..count].iter().map(|&(x, z)| Vec3::new(x, 15.5, z)).collect()
}

/// A fresh world with `seats` worms at random places, and a few more random
/// spots to come back at after dying. Everybody carries the default weapons
/// until they pick others.
fn new_world(map: &Grid, map_id: u8, seats: usize, seed: u32, places: Places) -> World {
    // The worms take the first points and the spare ones are only for coming
    // back, so a two-worm match does not respawn on the same two spots forever.
    let points = places(map_id, seed, seats + 4);
    let mut world = World::from_points(map.clone(), &points[..seats], seed);
    world.add_respawn_points(&points[seats..]);
    for i in 0..seats {
        world.set_loadout(i, &DEFAULT_LOADOUT);
    }
    world
}

/// Who drives a worm slot.
///
/// Pattern: State. A slot is either empty or a connected human, and what
/// `tick` does depends on which, without scattering `if is_human` flags.
enum Controller {
    /// Nobody: the worm stands there as a target dummy. This is an NPC seat, or
    /// a human seat nobody has taken yet. (The 2D game put a bot here; they
    /// come back with M6, and become a second kind of controller.)
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
    /// What a rematch starts from: the untouched map and the way the first
    /// match began. (Digging changes `world.grid`, so it cannot be rebuilt
    /// from that.)
    pristine: Grid,
    /// The first this many seats are for humans; the rest are NPCs for good.
    /// A human who joins takes the first free human seat, which is "replacing
    /// an NPC" on screen: until then the seat is a dummy like the NPCs.
    humans: usize,
    places: Places,
    /// Which of `sim::map::MAPS` the match is on, repeated in every Welcome.
    map: u8,
    seed: u32,
    /// Does a match start with the players picking their weapons (the world
    /// frozen until they have)? The real game does; the tests' hosts play at once.
    choose_first: bool,
    /// Which humans have picked in this match. Only the first pick counts,
    /// because picking refills the magazines (a client could otherwise
    /// "reload" by choosing again).
    chose: Vec<bool>,
    /// Ticks left before the match begins without the humans who have not picked.
    choose_left: u32,
    /// Which match this is: 0, then one more after each rematch. Whoever hands
    /// out Welcomes (the `Session`, the local client) compares it with the
    /// number it last saw, so a rematch asked for by anyone reaches everyone.
    match_number: u32,
}

impl Host {
    /// A new match on `map` with `humans` seats for people (idle until they
    /// join) and `npcs` more worms nobody drives. The worms start at random
    /// places picked from `seed`.
    ///
    /// `map` is the grid built for map number `map_id` of `sim::map::MAPS`. The
    /// match starts with everybody choosing weapons: it begins once every
    /// human has picked (or time is up).
    pub fn new(map: Grid, map_id: u8, humans: usize, npcs: usize, seed: u32) -> Host {
        let mut host = Host::on(map, map_id, humans, npcs, seed, spawn_points);
        host.choose_first = true;
        host.start_choosing();
        host
    }

    /// `new` for a map that is not one of the demo maps (a test's flat ground),
    /// and with no choosing: the match is on from the first tick.
    pub fn with_places(map: Grid, humans: usize, npcs: usize, seed: u32, places: Places) -> Host {
        Host::on(map, 0, humans, npcs, seed, places)
    }

    fn on(map: Grid, map_id: u8, humans: usize, npcs: usize, seed: u32, places: Places) -> Host {
        let seats = (humans + npcs).min(MAX_PLAYERS);
        let humans = humans.min(seats);
        let world = new_world(&map, map_id, seats, seed, places);
        Host {
            world,
            controllers: (0..seats).map(|_| Controller::Idle).collect(),
            edits: Vec::new(),
            pristine: map,
            humans,
            places,
            map: map_id,
            seed,
            choose_first: false,
            chose: vec![false; seats],
            choose_left: CHOOSE_TICKS,
            match_number: 0,
        }
    }

    /// Freeze the world until the humans have picked their weapons.
    fn start_choosing(&mut self) {
        self.world.choosing = true;
        self.chose = vec![false; self.controllers.len()];
        self.choose_left = CHOOSE_TICKS;
    }

    /// The kills that win a match (`SCORE_LIMIT` unless this says otherwise).
    /// A rule of the room, so it carries over to rematches.
    pub fn set_score_limit(&mut self, kills: i32) {
        self.world.score_limit = kills;
    }

    /// Which match is running (see the field). It changes exactly when
    /// everyone's copy of the world has to be replaced by a fresh Welcome.
    pub fn match_number(&self) -> u32 {
        self.match_number
    }

    /// Start the next match on the same map with the same people in the same
    /// seats, at new random places. Their last inputs are cleared, so a held
    /// trigger does not fire the moment the new match begins.
    fn rematch(&mut self) {
        self.match_number += 1;
        let mut world = new_world(
            &self.pristine,
            self.map,
            self.controllers.len(),
            self.seed.wrapping_add(self.match_number),
            self.places,
        );
        // The rules and the cheat belong to the room, not to one match.
        world.score_limit = self.world.score_limit;
        world.infinite_ammo = self.world.infinite_ammo;
        self.world = world;
        if self.choose_first {
            // Everyone picks again: that is half of why one restarts.
            self.start_choosing();
        }
        self.edits.clear();
        for c in &mut self.controllers {
            if let Controller::Human(input) = c {
                *input = Input::default();
            }
        }
    }

    /// A human arrives: give them the first human seat nobody holds.
    /// `None` means the match is full.
    pub fn join(&mut self) -> Option<Welcome> {
        let slot = self.controllers[..self.humans].iter().position(|c| matches!(c, Controller::Idle))?;
        self.controllers[slot] = Controller::Human(Input::default());
        // Whoever sat here before took their weapons with them.
        self.chose[slot] = false;
        self.world.set_loadout(slot, &DEFAULT_LOADOUT);
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
        Welcome { you, map: self.map, edits: self.edits.clone(), snapshot: self.snapshot(Vec::new()) }
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
            // The weapons a human picked. Never trust the client: it must be a
            // whole, allowed set, and only the first one of a match counts.
            ClientMsg::Loadout(ids) => {
                let human = matches!(self.controllers.get(id as usize), Some(Controller::Human(_)));
                if human && !self.chose[id as usize] && valid_loadout(&ids) {
                    self.world.set_loadout(id as usize, &ids);
                    self.chose[id as usize] = true;
                }
            }
            // A ping tests the connection, not the game, so whoever owns the
            // connection answers it (the host session), not this struct.
            // A resync is the same kind of thing: the session sends the reply
            // (`rewelcome`) down that client's own connection.
            ClientMsg::Ping { .. } | ClientMsg::Resync => {}
            // Anyone in the match may restart it for everybody, at any time (a
            // finished match or not). Only seated players, though: the host
            // still decides who counts as one.
            ClientMsg::Rematch => {
                if matches!(self.controllers.get(id as usize), Some(Controller::Human(_))) {
                    self.rematch();
                }
            }
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
        if self.world.choosing {
            self.wait_for_choices();
        }
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

    /// The match begins once every human has picked, or the time is up. The
    /// clock only runs while somebody is here to pick: a room nobody has
    /// entered yet waits for its first player.
    fn wait_for_choices(&mut self) {
        let humans: Vec<usize> =
            (0..self.controllers.len()).filter(|&i| matches!(self.controllers[i], Controller::Human(_))).collect();
        if humans.is_empty() {
            return;
        }
        self.choose_left = self.choose_left.saturating_sub(1);
        if self.choose_left == 0 || humans.iter().all(|&i| self.chose[i]) {
            self.world.choosing = false;
        }
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
            score_limit: self.world.score_limit,
            winner: self.world.winner,
            choosing: self.world.choosing,
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

    /// Like `flat_places`, but the row of worms shifts with the seed, so a new
    /// match starts somewhere else.
    fn shifting_spots(_map: u8, seed: u32, count: usize) -> Vec<Vec3> {
        (0..count).map(|i| Vec3::new(3.0 + i as f32 * 3.0 + (seed % 7) as f32, 15.5, 16.0)).collect()
    }

    /// Two human seats on the ground (the spawn drop finds rock at once, so put
    /// the worms on the surface by hand, like the sim's own tests do).
    fn host() -> Host {
        let mut host = Host::with_places(ground(), 2, 0, 7, flat_places);
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
    fn any_player_can_restart_a_running_match_for_everyone() {
        let mut host = host();
        host.join();
        host.join();
        host.tick();
        host.handle(1, ClientMsg::Rematch);
        assert_eq!(host.match_number(), 1);
        assert_eq!(host.world().tick, 0, "the match began again");
        assert!(host.rewelcome(0).is_some() && host.rewelcome(1).is_some(), "both are still seated");
    }

    #[test]
    fn a_rematch_starts_at_new_random_places() {
        let mut host = Host::with_places(ground(), 1, 1, 3, shifting_spots);
        host.join();
        let before: Vec<_> = host.world().worms.iter().map(|w| (w.pos.x, w.pos.z)).collect();
        host.handle(0, ClientMsg::Rematch);
        let after: Vec<_> = host.world().worms.iter().map(|w| (w.pos.x, w.pos.z)).collect();
        assert_ne!(before, after);
    }

    #[test]
    fn humans_take_the_seats_set_aside_for_them_and_npcs_stay_dummies() {
        let mut host = Host::with_places(ground(), 2, 2, 7, flat_places);
        assert_eq!(host.world().worms.len(), 4);
        assert_eq!(host.join().unwrap().you, 0);
        assert_eq!(host.join().unwrap().you, 1);
        assert!(host.join().is_none(), "seats 2 and 3 are NPCs");
        assert!(host.rewelcome(2).is_none());
        host.handle(2, ClientMsg::Rematch);
        assert_eq!(host.match_number(), 0, "an NPC cannot ask for a rematch");
    }

    /// A host that does what the real one does at the start of a match: the
    /// world waits for the humans to pick their weapons.
    fn choosing_host() -> Host {
        let mut host = host();
        host.choose_first = true;
        host.start_choosing();
        host
    }

    fn carried(host: &Host, slot: usize) -> Vec<u8> {
        host.world().worms[slot].weapons.iter().map(|g| g.id).collect()
    }

    #[test]
    fn a_human_may_pick_once_and_only_a_valid_set() {
        use sim::weapons::{BLASTER, BOMB, BUILD, CLUSTER, NOVA, RAILGUN, ROCKET, TELEPORT};
        let mut host = host();
        host.join();
        let mine = vec![NOVA, BOMB, TELEPORT, BLASTER, BUILD];
        host.handle(0, ClientMsg::Loadout(vec![ROCKET; 5]));
        host.handle(0, ClientMsg::Loadout(vec![BLASTER, ROCKET]));
        host.handle(0, ClientMsg::Loadout(vec![ROCKET, BOMB, CLUSTER, BLASTER, BUILD]));
        assert_eq!(carried(&host, 0), DEFAULT_LOADOUT, "invalid choices change nothing");
        host.handle(0, ClientMsg::Loadout(mine.clone()));
        assert_eq!(carried(&host, 0), mine);
        host.handle(0, ClientMsg::Loadout(DEFAULT_LOADOUT.to_vec()));
        assert_eq!(carried(&host, 0), mine, "a second pick would be a free reload");
        // Nobody sits in seat 1, so a forged pick for it does nothing.
        host.handle(1, ClientMsg::Loadout(vec![NOVA, BOMB, TELEPORT, BLASTER, RAILGUN]));
        assert_eq!(carried(&host, 1), DEFAULT_LOADOUT);
    }

    #[test]
    fn the_match_waits_for_every_human_to_pick() {
        let mut host = choosing_host();
        assert!(host.tick().choosing, "nobody is here yet: it waits for the first player");
        host.join();
        host.join();
        let start = host.world().worms[0].pos;
        for _ in 0..30 {
            host.tick();
        }
        assert_eq!(host.world().worms[0].pos, start, "frozen while they choose");
        assert!(host.world().tick > 30, "but the clock runs");

        host.handle(0, ClientMsg::Loadout(DEFAULT_LOADOUT.to_vec()));
        assert!(host.tick().choosing, "one of two is not enough");
        host.handle(1, ClientMsg::Loadout(DEFAULT_LOADOUT.to_vec()));
        assert!(!host.tick().choosing, "everyone is ready");
    }

    #[test]
    fn the_match_begins_when_time_runs_out_even_if_somebody_has_not_picked() {
        let mut host = choosing_host();
        host.join();
        for _ in 0..CHOOSE_TICKS - 1 {
            assert!(host.tick().choosing);
        }
        assert!(!host.tick().choosing);
    }

    #[test]
    fn a_restart_makes_everybody_pick_again() {
        use sim::weapons::{BLASTER, BOMB, BUILD, NOVA, TELEPORT};
        let mut host = choosing_host();
        host.join();
        host.handle(0, ClientMsg::Loadout(vec![NOVA, BOMB, TELEPORT, BLASTER, BUILD]));
        host.tick();
        assert!(!host.world().choosing);

        host.handle(0, ClientMsg::Rematch);
        assert!(host.world().choosing, "choosing again");
        assert_eq!(carried(&host, 0), DEFAULT_LOADOUT, "the old pick is gone");
        let welcome = host.rewelcome(0).unwrap();
        assert!(welcome.snapshot.choosing, "the Welcome tells the client to show the picker");
        host.handle(0, ClientMsg::Loadout(vec![BLASTER, BOMB, TELEPORT, NOVA, BUILD]));
        assert_eq!(carried(&host, 0), [BLASTER, BOMB, TELEPORT, NOVA, BUILD], "and they may pick again");
    }

    #[test]
    fn a_player_who_joins_takes_the_default_weapons_not_the_last_ones() {
        use sim::weapons::{BLASTER, BOMB, BUILD, NOVA, TELEPORT};
        let mut host = host();
        host.join();
        host.handle(0, ClientMsg::Loadout(vec![NOVA, BOMB, TELEPORT, BLASTER, BUILD]));
        host.leave(0);
        host.join();
        assert_eq!(carried(&host, 0), DEFAULT_LOADOUT);
        host.handle(0, ClientMsg::Loadout(vec![NOVA, BOMB, TELEPORT, BLASTER, BUILD]));
        assert_eq!(carried(&host, 0)[0], NOVA, "and the new player may pick");
    }

    #[test]
    fn the_welcome_names_the_map() {
        let mut host = host();
        host.map = 2;
        assert_eq!(host.join().unwrap().map, 2);
    }

    #[test]
    fn a_match_never_holds_more_than_the_most_players() {
        let host = Host::with_places(ground(), 4, 4, 7, flat_places);
        assert_eq!(host.world().worms.len(), MAX_PLAYERS);
    }

    #[test]
    fn a_rematch_starts_over_with_the_same_people_and_rules() {
        let mut host = host();
        host.join();
        host.join();
        host.world.score_limit = 3;
        host.world.worms[0].kills = 3;
        host.world.winner = Some(0);
        host.debug_edit(Shape::Sphere { center: Vec3::new(16.0, 4.0, 16.0), radius: 3.0 }, Op::Carve);
        host.tick();
        let dug = host.world().grid.checksum();

        // An empty seat cannot ask; a player can.
        host.leave(1);
        host.handle(1, ClientMsg::Rematch);
        assert_eq!(host.match_number(), 0, "nobody is in seat 1");
        host.handle(0, ClientMsg::Rematch);

        assert_eq!(host.match_number(), 1);
        let w = host.world();
        assert_eq!((w.winner, w.tick, w.worms[0].kills, w.score_limit), (None, 0, 0, 3));
        assert_ne!(w.grid.checksum(), dug, "the craters are gone");
        assert_eq!(w.grid.checksum(), ground().checksum());
        let welcome = host.rewelcome(0).expect("still seated");
        assert!(welcome.edits.is_empty(), "a fresh edit log");
        assert_eq!(welcome.snapshot.winner, None);
        assert_eq!(welcome.snapshot.score_limit, 3);
    }

    #[test]
    fn a_rematch_clears_what_the_players_were_holding() {
        let mut host = host();
        host.join();
        host.handle(0, ClientMsg::Input(Input { fire: true, weapon: ROCKET, ..Input::default() }));
        host.world.winner = Some(1);
        host.handle(0, ClientMsg::Rematch);
        let events = host.tick().events;
        assert!(events.iter().all(|e| !matches!(e, Event::Shot { .. })), "the trigger is not held: {events:?}");
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
