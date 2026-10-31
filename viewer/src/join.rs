// Joining a friend's game: connect to the relay, knock on the room, wait for
// the host's `Welcome`, then play by sending inputs and drawing snapshots.
//
// This is a pure client. It never calls `World::step`; the world it draws is a
// `Replica` that the host's snapshots overwrite (see `protocol::replica`).
//
//   keyboard ─ Input ──> relay ──> host
//   screen <─ Replica <─ Snapshot ─ relay <── host
//
// Networking concept: latency. Between pressing a key and seeing your worm
// move, the news travels to the host, the host simulates it, and the snapshot
// travels back: at least one round trip, plus up to one snapshot interval.
// There is no client-side prediction (yet), so you feel all of it. The ping on
// screen is that round trip, measured with a token the host echoes back.
//
// On top of that the replica plays snapshots back `INTERP_DELAY` ticks late so
// motion is smooth (see `protocol::replica`). The cost of smoothness is that
// everything, including your own worm, is drawn ~100 ms in the past.
//
// Split in two so the interesting half can be tested without a socket:
//   * `Joiner`   pure: relay messages in, relay messages and state out;
//   * `JoinGame` the connection around it (sockets, the first knock, the clock).

use protocol::replica::Replica;
use protocol::{
    decode_host, encode_client, ClientMsg, FromRelay, HostMsg, Reject, ToRelay, WelcomeBuilder, PROTOCOL_VERSION,
};
use sdf::Grid;
use sim::{Input, DT};
use web_time::Instant;

use crate::net::{relay_url, RelayLink};
use crate::socket::SocketState;

/// How far behind the newest snapshot we draw: 6 ticks = 100 ms. Snapshots
/// come every 2 ticks, so this keeps two or three in the buffer: enough to
/// ride out one late packet without a stall.
const INTERP_DELAY: u32 = 6;

/// Ping the host this often, in ticks (1 s).
const PING_TICKS: u32 = 60;

/// Where the join stands, for the screen that shows it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    /// The connection to the relay is still being made (a sleeping free
    /// server can take a minute).
    Connecting,
    /// Connected, asked to join, no answer yet.
    Knocking,
    /// The relay let us in; the host has not sent the match yet.
    WaitingForHost,
    /// We have a replica to draw.
    Playing,
    /// Gave up: the reason is for the player to read.
    Failed(&'static str),
}

impl Status {
    /// What to tell the player while they wait or when it failed. Upper case:
    /// that is all the HUD's font has (a test in hud.rs checks every string).
    pub fn lines(&self, code: &str) -> Vec<String> {
        // A server-hosted room has no code until the relay has made it.
        let head = if code.is_empty() { "OPENING A ROOM ON THE SERVER".to_string() } else { format!("JOINING {code}") };
        match self {
            Status::Connecting => {
                vec![head, "CONNECTING TO THE RELAY...".into(), "A FREE SERVER CAN TAKE A MINUTE TO WAKE UP".into()]
            }
            Status::Knocking => vec![head, "KNOCKING...".into()],
            Status::WaitingForHost => vec![head, "IN THE ROOM - WAITING FOR THE HOST...".into()],
            Status::Playing => Vec::new(),
            Status::Failed(why) => vec![(*why).into()],
        }
    }
}

/// The state of a join, with no connection in it.
pub struct Joiner {
    /// The room to join; for a server-hosted room, empty until the relay
    /// has made the room and told us its code.
    code: String,
    /// Ask the server to run a new match (with this many human seats and
    /// NPCs, on this map) instead of joining a friend's.
    hosted: Option<(u8, u8, u8)>,
    /// The untouched map, cloned into each new `Replica` (the Welcome carries
    /// only the edits on top of it). Built once by the caller: generating it
    /// takes a second or more.
    map: Grid,
    /// Which of `sim::map::MAPS` `map` is. A Welcome names the host's map; if
    /// it is another one, the right one is built when it arrives.
    map_id: u8,
    in_room: bool,
    builder: WelcomeBuilder,
    replica: Option<Replica>,
    problem: Option<&'static str>,
    rtt_ms: Option<f32>,
    /// Told the host we need a fresh Welcome and it has not come yet. Without
    /// this we would ask again every tick while it is on its way.
    resync_asked: bool,
    seq: u32,
    /// How many replicas have been built (see `Play::replica_id`).
    generation: u32,
}

impl Joiner {
    pub fn new(code: String, map: Grid, map_id: u8) -> Joiner {
        Joiner {
            code,
            hosted: None,
            map,
            map_id,
            in_room: false,
            builder: WelcomeBuilder::default(),
            replica: None,
            problem: None,
            rtt_ms: None,
            resync_asked: false,
            seq: 0,
            generation: 0,
        }
    }

    /// A join to a match the server will run for us: we ask for a new room
    /// and are put in it like any other player (see server/src/matches.rs).
    /// `humans` seats are for people, `npcs` worms are dummies, and the match
    /// is on map number `map_id` (`map` is that map, built).
    pub fn hosted(map: Grid, map_id: u8, humans: u8, npcs: u8) -> Joiner {
        Joiner { hosted: Some((humans, npcs, map_id)), ..Joiner::new(String::new(), map, map_id) }
    }

    /// The first thing to say to the relay once connected.
    pub fn knock(&self) -> ToRelay {
        if let Some((humans, npcs, map)) = self.hosted {
            ToRelay::CreateHostedRoom { version: PROTOCOL_VERSION, humans, npcs, map }
        } else {
            ToRelay::JoinRoom { code: self.code.clone(), version: PROTOCOL_VERSION }
        }
    }

    /// Something arrived from the relay. `now_ms` is a wrapping millisecond
    /// clock, used to time pings.
    pub fn on_relay(&mut self, msg: FromRelay, now_ms: u32) {
        match msg {
            FromRelay::RoomJoined => self.in_room = true,
            // Only a server-hosted room tells its asker the code: friends
            // type it to join the same match.
            FromRelay::RoomCreated { code } => self.code = code,
            FromRelay::RoomClosed => self.problem = Some("THE HOST LEFT - THE MATCH IS OVER"),
            FromRelay::Rejected(why) => self.problem = Some(rejection(why)),
            FromRelay::Data { bytes, .. } => {
                if let Some(msg) = decode_host(&bytes) {
                    self.on_host(msg, now_ms);
                }
            }
            // For hosts, or about the connection itself.
            FromRelay::PeerJoined(_) | FromRelay::PeerLeft(_) | FromRelay::Pong { .. } => {}
        }
    }

    fn on_host(&mut self, msg: HostMsg, now_ms: u32) {
        // The first Welcome starts our match; a later one is the answer to a
        // resync (or a rematch). It arrives in pieces, collected by the
        // builder.
        if let Some(welcome) = self.builder.push(&msg) {
            // A friend's room may be on another map than the one we have: build
            // theirs (about a second; it is only ever done once per change).
            if welcome.map != self.map_id && (welcome.map as usize) < sim::map::MAPS.len() {
                self.map = sim::map::build_map(welcome.map);
                self.map_id = welcome.map;
            }
            self.replica = Some(Replica::new(welcome, self.map.clone(), INTERP_DELAY));
            self.generation += 1;
            self.resync_asked = false;
            return;
        }
        match msg {
            // Wrapping subtraction: the token is a clock reading that wraps.
            HostMsg::Pong { token } => self.rtt_ms = Some(now_ms.wrapping_sub(token) as f32),
            HostMsg::Full => self.problem = Some("THAT MATCH IS FULL"),
            // Snapshots that arrive before the Welcome are useless (there is
            // nothing to apply them to) and are dropped.
            HostMsg::Snapshot(_) => {
                if let Some(replica) = &mut self.replica {
                    replica.apply(msg);
                }
            }
            HostMsg::Welcome(_) | HostMsg::Edits(_) => {} // still being collected
        }
    }

    /// One fixed step of play: what to send the host (our input, and a resync
    /// request if the terrain went wrong), and the replica moves on a tick.
    pub fn tick(&mut self, input: Input) -> Vec<ToRelay> {
        let Some(replica) = &mut self.replica else { return Vec::new() };
        replica.advance(DT);

        self.seq = self.seq.wrapping_add(1);
        let mut out = vec![to_host(&ClientMsg::Input(Input { seq: self.seq, ..input }))];
        if replica.desynced() && !self.resync_asked {
            self.resync_asked = true;
            out.push(to_host(&ClientMsg::Resync));
        }
        out
    }

    pub fn ping(&self, now_ms: u32) -> ToRelay {
        to_host(&ClientMsg::Ping { token: now_ms })
    }

    /// "Start over": the host starts a new match, and answers with a new
    /// Welcome like any other.
    pub fn rematch(&self) -> ToRelay {
        to_host(&ClientMsg::Rematch)
    }

    /// "These are my weapons."
    pub fn loadout(&self, ids: Vec<u8>) -> ToRelay {
        to_host(&ClientMsg::Loadout(ids))
    }

    pub fn replica(&self) -> Option<&Replica> {
        self.replica.as_ref()
    }

    pub fn replica_mut(&mut self) -> Option<&mut Replica> {
        self.replica.as_mut()
    }

    pub fn generation(&self) -> u32 {
        self.generation
    }

    pub fn rtt_ms(&self) -> Option<f32> {
        self.rtt_ms
    }

    pub fn code(&self) -> &str {
        &self.code
    }

    /// `connection` is the state of the socket underneath.
    pub fn status(&self, connection: SocketState) -> Status {
        if let Some(problem) = self.problem {
            return Status::Failed(problem);
        }
        match connection {
            SocketState::Closed => Status::Failed("LOST THE CONNECTION TO THE RELAY"),
            SocketState::Connecting => Status::Connecting,
            SocketState::Open if self.replica.is_some() => Status::Playing,
            SocketState::Open if self.in_room => Status::WaitingForHost,
            SocketState::Open => Status::Knocking,
        }
    }
}

fn to_host(msg: &ClientMsg) -> ToRelay {
    // A joiner's `Data` always goes to the host, so `to` stays empty.
    ToRelay::Data { to: None, bytes: encode_client(msg) }
}

fn rejection(why: Reject) -> &'static str {
    match why {
        Reject::NoSuchRoom => "NO ROOM WITH THAT CODE",
        Reject::RoomFull => "THAT ROOM IS FULL",
        Reject::AlreadyInRoom => "ALREADY IN A ROOM",
        Reject::VersionMismatch => "THAT ROOM RUNS ANOTHER VERSION - RELOAD THE GAME",
        Reject::ServerBusy => "THE SERVER IS BUSY - TRY AGAIN IN A MINUTE",
    }
}

/// A `Joiner` with its connection.
pub struct JoinGame {
    link: RelayLink,
    joiner: Joiner,
    /// Have we knocked yet? (Only possible once the socket is open.)
    asked: bool,
    ticks_since_ping: u32,
    /// The zero of the millisecond clock the ping tokens are read from.
    started: Instant,
}

impl JoinGame {
    /// Start connecting to the relay and asking for room `code`. `map` is the
    /// untouched map the host's match started from.
    pub fn new(code: String, map: Grid, map_id: u8) -> JoinGame {
        JoinGame::with(Joiner::new(code, map, map_id))
    }

    /// Start connecting and ask the server to host a new match for us.
    pub fn hosted(map: Grid, map_id: u8, humans: u8, npcs: u8) -> JoinGame {
        JoinGame::with(Joiner::hosted(map, map_id, humans, npcs))
    }

    fn with(joiner: Joiner) -> JoinGame {
        JoinGame {
            link: RelayLink::connect(relay_url()),
            joiner,
            asked: false,
            ticks_since_ping: 0,
            started: Instant::now(),
        }
    }

    fn now_ms(&self) -> u32 {
        self.started.elapsed().as_millis() as u32
    }

    /// One fixed step: read what the relay delivered, send our input, show
    /// the next moment of the match. Safe to call while still connecting.
    pub fn tick(&mut self, input: Input) {
        if self.link.state() == SocketState::Open {
            if !self.asked {
                self.asked = true;
                self.link.send(&self.joiner.knock());
            }
            let now = self.now_ms();
            while let Some(msg) = self.link.recv() {
                self.joiner.on_relay(msg, now);
            }
            self.ticks_since_ping += 1;
            if self.ticks_since_ping >= PING_TICKS {
                self.ticks_since_ping = 0;
                self.link.send(&self.joiner.ping(now));
            }
        }
        // Not while failed: the match is over and the screen shows why.
        if self.status() == Status::Playing {
            for msg in self.joiner.tick(input) {
                self.link.send(&msg);
            }
        }
    }

    pub fn status(&self) -> Status {
        self.joiner.status(self.link.state())
    }

    /// Ask the host for another match (only meaningful once this one is over).
    pub fn rematch(&mut self) {
        if self.status() == Status::Playing {
            self.link.send(&self.joiner.rematch());
        }
    }

    /// Tell the host which weapons we picked.
    pub fn loadout(&mut self, ids: Vec<u8>) {
        if self.status() == Status::Playing {
            self.link.send(&self.joiner.loadout(ids));
        }
    }

    pub fn replica(&self) -> Option<&Replica> {
        self.joiner.replica()
    }

    pub fn replica_mut(&mut self) -> Option<&mut Replica> {
        self.joiner.replica_mut()
    }

    pub fn generation(&self) -> u32 {
        self.joiner.generation()
    }

    pub fn rtt_ms(&self) -> Option<f32> {
        self.joiner.rtt_ms()
    }

    pub fn code(&self) -> &str {
        self.joiner.code()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::{IVec3, Vec2, Vec3};
    use host::{Host, Session};
    use protocol::relay::PeerId;
    use protocol::{encode_host, encode_to_relay};
    use sdf::{Op, Shape};

    const PEER: PeerId = 1;

    fn ground() -> Grid {
        Grid::from_fn(IVec3::new(8, 4, 8), |p| p.y - 4.0)
    }

    /// Host, its session, and a joiner, wired together in memory the way the
    /// relay would: no sockets, but every message really is encoded and decoded.
    struct Match {
        host: Host,
        session: Session,
        joiner: Joiner,
        now_ms: u32,
    }

    impl Match {
        fn new() -> Match {
            let host = Host::with_places(ground(), 2, 0, 7, host::flat_places);
            Match { host, session: Session::new(), joiner: Joiner::new("ABCD".into(), ground(), 0), now_ms: 1000 }
        }

        /// The relay tells the host a friend joined, and the friend that it is in.
        fn knock(&mut self) {
            let ToRelay::JoinRoom { .. } = self.joiner.knock() else { panic!("a knock is a JoinRoom") };
            self.joiner.on_relay(FromRelay::RoomJoined, self.now_ms);
            self.session.on_relay(&mut self.host, FromRelay::PeerJoined(PEER));
        }

        /// A joiner's outgoing messages reach the host (via the relay).
        fn deliver_to_host(&mut self, msgs: Vec<ToRelay>) -> Vec<ToRelay> {
            let mut replies = Vec::new();
            for msg in msgs {
                let ToRelay::Data { bytes, .. } = msg else { continue };
                replies.extend(self.session.on_relay(&mut self.host, FromRelay::Data { from: PEER, bytes }));
            }
            replies
        }

        fn deliver_to_joiner(&mut self, msgs: Vec<ToRelay>) {
            for msg in msgs {
                let ToRelay::Data { to, bytes } = msg else { continue };
                if to.is_none_or(|t| t == PEER) {
                    self.joiner.on_relay(FromRelay::Data { from: 0, bytes }, self.now_ms);
                }
            }
        }

        /// One tick of everything: the joiner's input, the host's step, the
        /// snapshot back. Returns what the host sent, undelivered, so a test
        /// can drop it.
        fn host_tick(&mut self) -> Vec<ToRelay> {
            let snapshot = self.host.tick();
            self.session.after_tick(&mut self.host, snapshot)
        }

        fn step(&mut self, input: Input) {
            let sent = self.joiner.tick(input);
            let replies = self.deliver_to_host(sent);
            self.deliver_to_joiner(replies);
            let out = self.host_tick();
            self.deliver_to_joiner(out);
        }
    }

    fn walk() -> Input {
        Input { move_dir: Vec2::new(0.0, 1.0), yaw: 1.5, ..Input::default() }
    }

    #[test]
    fn a_joiner_gets_a_replica_and_steers_its_worm() {
        let mut m = Match::new();
        assert_eq!(m.joiner.status(SocketState::Open), Status::Knocking);
        m.knock();
        assert_eq!(m.joiner.status(SocketState::Open), Status::WaitingForHost);

        for _ in 0..6 {
            m.step(Input::default());
        }
        assert_eq!(m.joiner.status(SocketState::Open), Status::Playing);
        let start = m.joiner.replica().unwrap().world.worms[0].pos.x;
        for _ in 0..90 {
            m.step(walk());
        }
        let replica = m.joiner.replica().unwrap();
        assert_eq!(replica.me, 0);
        assert!(replica.world.worms[0].pos.x > start + 2.0, "our worm walked (on the host, then back to us)");
        assert!(!replica.desynced());
    }

    #[test]
    fn inputs_carry_a_growing_sequence_number() {
        let mut m = Match::new();
        m.knock();
        for _ in 0..4 {
            m.step(Input::default());
        }
        let sent = m.joiner.tick(Input::default());
        let ToRelay::Data { bytes, .. } = &sent[0] else { panic!("an input is Data") };
        let Some(ClientMsg::Input(a)) = protocol::decode_client(bytes) else { panic!("not an input") };
        let sent = m.joiner.tick(Input::default());
        let ToRelay::Data { bytes, .. } = &sent[0] else { panic!() };
        let Some(ClientMsg::Input(b)) = protocol::decode_client(bytes) else { panic!() };
        assert_eq!(b.seq, a.seq + 1);
    }

    #[test]
    fn nothing_is_sent_before_the_match_starts() {
        let mut j = Joiner::new("ABCD".into(), ground(), 0);
        assert!(j.tick(Input::default()).is_empty());
    }

    #[test]
    fn every_refusal_has_words() {
        for (why, text) in [
            (Reject::NoSuchRoom, "NO ROOM WITH THAT CODE"),
            (Reject::RoomFull, "THAT ROOM IS FULL"),
            (Reject::VersionMismatch, "THAT ROOM RUNS ANOTHER VERSION - RELOAD THE GAME"),
        ] {
            let mut j = Joiner::new("ABCD".into(), ground(), 0);
            j.on_relay(FromRelay::Rejected(why), 0);
            assert_eq!(j.status(SocketState::Open), Status::Failed(text));
        }
    }

    #[test]
    fn the_host_leaving_ends_the_match() {
        let mut m = Match::new();
        m.knock();
        for _ in 0..4 {
            m.step(Input::default());
        }
        m.joiner.on_relay(FromRelay::RoomClosed, 0);
        assert!(matches!(m.joiner.status(SocketState::Open), Status::Failed(_)));
    }

    #[test]
    fn a_full_match_says_so() {
        let mut m = Match::new();
        m.host.join();
        m.host.join(); // both seats taken
        m.knock();
        for _ in 0..4 {
            m.step(Input::default());
        }
        assert_eq!(m.joiner.status(SocketState::Open), Status::Failed("THAT MATCH IS FULL"));
    }

    #[test]
    fn a_dropped_connection_is_a_failure_whatever_else_is_true() {
        let mut m = Match::new();
        m.knock();
        for _ in 0..4 {
            m.step(Input::default());
        }
        assert!(matches!(m.joiner.status(SocketState::Closed), Status::Failed(_)));
        assert_eq!(m.joiner.status(SocketState::Connecting), Status::Connecting);
    }

    #[test]
    fn the_ping_answer_becomes_a_round_trip_time() {
        let mut m = Match::new();
        m.knock();
        for _ in 0..4 {
            m.step(Input::default());
        }
        let ping = m.joiner.ping(1000);
        let replies = m.deliver_to_host(vec![ping]);
        m.now_ms = 1042; // the answer comes back 42 ms later
        m.deliver_to_joiner(replies);
        assert_eq!(m.joiner.rtt_ms(), Some(42.0));
    }

    #[test]
    fn the_ping_clock_may_wrap_around() {
        let mut j = Joiner::new("ABCD".into(), ground(), 0);
        let token = u32::MAX - 10;
        let pong = encode_host(&HostMsg::Pong { token });
        j.on_relay(FromRelay::Data { from: 0, bytes: pong }, 20);
        assert_eq!(j.rtt_ms(), Some(31.0));
    }

    #[test]
    fn snapshots_before_the_welcome_are_ignored() {
        let mut m = Match::new();
        m.knock();
        let snapshot = m.host.tick();
        let bytes = encode_host(&HostMsg::Snapshot(snapshot));
        m.joiner.on_relay(FromRelay::Data { from: 0, bytes }, 0); // no panic, no replica
        assert!(m.joiner.replica().is_none());
    }

    /// The whole repair loop: a terrain edit never reaches the joiner, the
    /// replica notices, asks for a resync once, and ends up in sync.
    #[test]
    fn a_lost_edit_is_noticed_and_repaired_with_one_request() {
        let mut m = Match::new();
        m.knock();
        for _ in 0..6 {
            m.step(Input::default());
        }
        m.host.debug_edit(Shape::Sphere { center: Vec3::new(16.0, 4.0, 16.0), radius: 2.0 }, Op::Carve);
        // The first snapshot that carries the edit is lost on the way.
        loop {
            if !m.host_tick().is_empty() {
                break;
            }
        }
        // The next ones arrive. The replica plays back `INTERP_DELAY` ticks
        // late, so it only sees the counts disagree when it SHOWS such a
        // snapshot; and the repair may finish inside this loop, so watch for
        // the problem as it happens.
        let (mut resyncs, mut noticed) = (0, false);
        for _ in 0..30 {
            let sent = m.joiner.tick(Input::default());
            noticed |= m.joiner.replica().unwrap().desynced();
            resyncs += sent
                .iter()
                .filter(|s| matches!(s, ToRelay::Data { bytes, .. } if matches!(protocol::decode_client(bytes), Some(ClientMsg::Resync))))
                .count();
            let replies = m.deliver_to_host(sent);
            m.deliver_to_joiner(replies);
            let out = m.host_tick();
            m.deliver_to_joiner(out);
        }
        assert!(noticed, "the replica saw the counts disagree");
        assert_eq!(resyncs, 1, "asked once, not every tick");

        for _ in 0..6 {
            m.step(Input::default());
        }
        let replica = m.joiner.replica().unwrap();
        assert!(!replica.desynced(), "rebuilt from a fresh Welcome");
        assert_eq!(replica.world.grid.checksum(), m.host.world().grid.checksum());
    }

    #[test]
    fn a_server_hosted_join_asks_for_a_room_and_learns_its_code() {
        let mut j = Joiner::hosted(ground(), 2, 2, 1);
        let ToRelay::CreateHostedRoom { version, humans, npcs, map } = j.knock() else { panic!("expected a hosted-room request") };
        assert_eq!((version, humans, npcs, map), (PROTOCOL_VERSION, 2, 1, 2));
        assert_eq!(j.status(SocketState::Open).lines(j.code())[0], "OPENING A ROOM ON THE SERVER");

        j.on_relay(FromRelay::RoomCreated { code: "WXYZ".into() }, 0);
        assert_eq!(j.code(), "WXYZ");
        assert_eq!(j.status(SocketState::Open).lines(j.code())[0], "JOINING WXYZ");
    }

    #[test]
    fn the_knock_names_the_room_and_the_build() {
        let j = Joiner::new("WXYZ".into(), ground(), 0);
        let ToRelay::JoinRoom { code, version } = j.knock() else { panic!() };
        assert_eq!((code.as_str(), version), ("WXYZ", PROTOCOL_VERSION));
        // And it is something the relay can decode.
        assert!(protocol::decode_to_relay(&encode_to_relay(&j.knock())).is_some());
    }
}
