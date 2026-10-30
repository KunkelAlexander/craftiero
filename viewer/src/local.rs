// A match that runs inside this process, with one human playing it.
//
// Even offline, the human does NOT touch the world directly. The whole online
// pipeline runs, just over an in-memory wire:
//
//   keyboard -> Input -> [wire] -> Host (steps the sim) -> Snapshot -> [wire] -> Replica -> screen
//
// Why bother? Because then online play is not a second code path to get right
// later. Going online means swapping the wire and moving the Host to another
// machine; everything else already works, and runs on every offline game.
// The renderer, the effects and the audio read `replica.world` and the
// replica's events, and cannot tell this from a match over the internet.

use std::collections::VecDeque;

use host::Host;
use protocol::replica::Replica;
use protocol::{
    decode_client, decode_host, encode_client, encode_host, welcome_messages, ClientMsg, HostMsg, PlayerId,
    WelcomeBuilder,
};
use sdf::{Grid, Op, Shape};
use sim::{Input, DT};

use crate::share::Share;

/// The wire between a client and a host.
///
/// `Loopback` is a fake network that lives inside one process: two queues of
/// bytes, one per direction. It behaves like a perfect connection (nothing is
/// lost, reordered or delayed), which is also roughly what a WebSocket gives
/// you: WebSocket runs over TCP, which is *reliable* (every message arrives)
/// and *ordered* (in the order sent). The price of that comfort is that one
/// delayed packet holds up everything behind it ("head-of-line blocking").
///
///   client_send ─ up ──> host_recv
///   client_recv <─ down ─ host_send
///
/// Messages are encoded to bytes on the way in and decoded on the way out even
/// here, so the encoding is exercised on every run.
#[derive(Default)]
struct Loopback {
    up: VecDeque<Vec<u8>>,
    down: VecDeque<Vec<u8>>,
}

impl Loopback {
    fn client_send(&mut self, msg: &ClientMsg) {
        self.up.push_back(encode_client(msg));
    }

    fn client_recv(&mut self) -> Option<HostMsg> {
        // Garbage is skipped, never fatal (see `protocol::decode_host`).
        while let Some(bytes) = self.down.pop_front() {
            if let Some(msg) = decode_host(&bytes) {
                return Some(msg);
            }
        }
        None
    }

    fn host_send(&mut self, msg: &HostMsg) {
        self.down.push_back(encode_host(msg));
    }

    fn host_recv(&mut self) -> Option<ClientMsg> {
        while let Some(bytes) = self.up.pop_front() {
            if let Some(msg) = decode_client(&bytes) {
                return Some(msg);
            }
        }
        None
    }
}

pub struct LocalGame {
    host: Host,
    link: Loopback,
    /// Which worm the host gave this connection (host-side knowledge: it is
    /// how `host.handle` is told who a message came from).
    peer: PlayerId,
    /// The client's copy of the world. This is what gets drawn.
    pub replica: Replica,
    /// The room on the relay, once the player has chosen to host one. The
    /// local player stays on the loopback (zero lag, every snapshot); friends
    /// come in through this.
    share: Option<Share>,
}

impl LocalGame {
    /// A match on `map` with a worm per spawn column; the human gets the first.
    pub fn new(map: Grid, spawns: &[(f32, f32)], seed: u32) -> LocalGame {
        let mut link = Loopback::default();
        // The client needs the untouched map too (`Welcome` does not carry it:
        // online, the client builds it from the same generator). Both sides
        // start from one copy of the same grid instead of generating it twice.
        let mut host = Host::new(map.clone(), spawns, seed);

        // The handshake, exactly as it will happen online: the newcomer
        // arrives, the host answers with a Welcome (the full state of the
        // match), and the client builds its replica from it.
        let welcome = host.join().expect("a fresh match has free slots");
        let peer = welcome.you;
        for msg in welcome_messages(welcome) {
            link.host_send(&msg);
        }
        // The Welcome travels in pieces (see `welcome_messages`); the client
        // puts it back together.
        let mut builder = WelcomeBuilder::default();
        let welcome = std::iter::from_fn(|| link.client_recv())
            .find_map(|msg| builder.push(&msg))
            .expect("the loopback delivers the Welcome we just sent");
        // Delay 0: no interpolation. The wire is a function call, so there
        // is no jitter to hide, and the local player gets no extra lag.
        let replica = Replica::new(welcome, map, 0);
        LocalGame { host, link, peer, replica, share: None }
    }

    /// One 1/60 s tick of the whole pipeline: the player's wishes go to the
    /// host, the host steps the sim, and the new snapshot comes back and is
    /// shown. Call it once per fixed step.
    pub fn tick(&mut self, input: Input) {
        // The client's half: "this is what I want".
        self.link.client_send(&ClientMsg::Input(input));

        // The host's half: read the inputs that arrived (friends' too, if
        // there is a room), step the sim, and send the result out (to the
        // local player every tick: that wire is free).
        while let Some(msg) = self.link.host_recv() {
            self.host.handle(self.peer, msg);
        }
        if let Some(share) = &mut self.share {
            share.receive(&mut self.host);
        }
        let snapshot = self.host.tick();
        if let Some(share) = &mut self.share {
            share.broadcast(&mut self.host, snapshot.clone());
        }
        self.link.host_send(&HostMsg::Snapshot(snapshot));

        // The client's half again: apply everything the host said.
        while let Some(msg) = self.link.client_recv() {
            self.replica.apply(msg);
        }
        self.replica.advance(DT);
    }

    /// Open a room on the relay so friends can join this match. The game goes
    /// on while the connection is made.
    pub fn open_room(&mut self) {
        self.share = Some(Share::open());
    }

    /// The room, if one was opened (its code, friends and problems).
    pub fn room(&self) -> Option<&Share> {
        self.share.as_ref()
    }

    /// The infinite-ammo cheat (see `Host::toggle_infinite_ammo`). Returns whether it is on.
    pub fn toggle_infinite_ammo(&mut self) -> bool {
        self.host.toggle_infinite_ammo()
    }

    /// The fly-mode brush: a developer tool that edits the host's terrain (see
    /// `Host::debug_edit`). It shows up on screen with the next tick.
    pub fn debug_edit(&mut self, shape: Shape, op: Op) {
        self.host.debug_edit(shape, op);
    }
}

// End to end over real sockets: a relay, a hosted room and a joiner, all in
// this process. Nothing here needs a window or a GPU, so it is the nearest a
// test gets to two players on two machines. (The pieces are tested alone in
// `host/src/session.rs` and `join.rs`; this proves they fit together through
// the socket and the relay.)
#[cfg(test)]
mod tests {
    use super::*;
    use crate::join::{JoinGame, Status};
    use crate::net::set_relay_url;
    use glam::{IVec3, Vec3};
    use std::sync::OnceLock;
    use std::time::{Duration, Instant};

    /// One relay for all the tests in this file. The relay address is a
    /// process-wide setting (`net::relay_url`), so it is set once, here.
    fn relay() {
        static STARTED: OnceLock<()> = OnceLock::new();
        STARTED.get_or_init(|| {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            listener.set_nonblocking(true).unwrap();
            std::thread::spawn(move || {
                tokio::runtime::Runtime::new().unwrap().block_on(async {
                    server::serve(tokio::net::TcpListener::from_std(listener).unwrap()).await;
                });
            });
            set_relay_url(format!("ws://127.0.0.1:{port}/ws"));
        });
    }

    fn ground() -> Grid {
        Grid::from_fn(IVec3::new(8, 4, 8), |p| p.y - 4.0)
    }

    fn host() -> LocalGame {
        relay();
        LocalGame::new(ground(), &[(8.0, 16.0), (24.0, 16.0)], 7)
    }

    /// Tick everything the way the game loop does, until `done` or a deadline.
    fn run(host: &mut LocalGame, joiner: &mut JoinGame, what: &str, mut done: impl FnMut(&LocalGame, &JoinGame) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done(host, joiner) {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            host.tick(Input::default());
            joiner.tick(Input::default());
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// A room with a joiner already playing in it.
    fn room_with_friend() -> (LocalGame, JoinGame) {
        let mut host = host();
        host.open_room();
        let deadline = Instant::now() + Duration::from_secs(10);
        let code = loop {
            host.tick(Input::default());
            if let Some(code) = host.room().and_then(|r| r.code()) {
                break code.to_string();
            }
            assert!(Instant::now() < deadline, "the relay never made the room");
            std::thread::sleep(Duration::from_millis(2));
        };
        let mut joiner = JoinGame::new(code, ground());
        run(&mut host, &mut joiner, "the join to start", |_, j| j.status() == Status::Playing);
        (host, joiner)
    }

    #[test]
    fn a_friend_joins_a_hosted_room_and_sees_the_same_terrain() {
        let (mut host, mut joiner) = room_with_friend();
        assert_eq!(host.room().unwrap().friends(), 1);
        assert_eq!(joiner.replica().unwrap().me, 1, "the friend took the second seat");

        // The host digs; the friend's copy of the terrain follows.
        host.debug_edit(Shape::Sphere { center: Vec3::new(16.0, 4.0, 16.0), radius: 3.0 }, Op::Carve);
        run(&mut host, &mut joiner, "the crater to reach the friend", |h, j| {
            j.replica().unwrap().world.grid.checksum() == h.replica.world.grid.checksum()
        });
        assert!(!joiner.replica().unwrap().desynced());
        // The first ping goes out after a second's worth of ticks.
        run(&mut host, &mut joiner, "a ping to come back", |_, j| j.rtt_ms().is_some());
    }

    #[test]
    fn a_friend_who_joins_late_gets_the_craters_already_dug() {
        let mut host = host();
        host.open_room();
        let code = loop {
            host.tick(Input::default());
            if let Some(code) = host.room().and_then(|r| r.code()) {
                break code.to_string();
            }
            std::thread::sleep(Duration::from_millis(2));
        };
        for i in 0..40 {
            host.debug_edit(Shape::Sphere { center: Vec3::new(3.0 + i as f32 * 0.6, 4.0, 10.0), radius: 0.8 }, Op::Carve);
            host.tick(Input::default());
        }
        let mut joiner = JoinGame::new(code, ground());
        run(&mut host, &mut joiner, "the late join", |h, j| {
            j.replica().is_some_and(|r| r.world.grid.checksum() == h.replica.world.grid.checksum())
        });
        assert!(!joiner.replica().unwrap().desynced());
    }

    #[test]
    fn joining_a_room_that_does_not_exist_says_so() {
        relay();
        let mut host = host(); // only here to pump the loop
        let mut joiner = JoinGame::new("ZZZZ".into(), ground());
        run(&mut host, &mut joiner, "the refusal", |_, j| matches!(j.status(), Status::Failed(_)));
        assert_eq!(joiner.status(), Status::Failed("NO ROOM WITH THAT CODE"));
    }

    /// The server's own match, asked for by a joiner. The map here is the real
    /// one (the server builds it too), which takes a while in a debug build.
    #[test]
    fn a_player_plays_in_a_match_the_server_hosts() {
        relay();
        static MAP: OnceLock<Grid> = OnceLock::new();
        let mut joiner = JoinGame::hosted(MAP.get_or_init(sim::map::build).clone());

        let deadline = Instant::now() + Duration::from_secs(120);
        while joiner.status() != Status::Playing {
            if let Status::Failed(why) = joiner.status() {
                panic!("the hosted match did not start: {why}");
            }
            assert!(Instant::now() < deadline, "the server never started the match");
            joiner.tick(Input::default());
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(joiner.code().len(), protocol::relay::CODE_LEN, "the server told us the room's code");
        assert_eq!(joiner.replica().unwrap().me, 0);

        // The server keeps playing: the match's clock moves on.
        let start = joiner.replica().unwrap().world.tick;
        while joiner.replica().unwrap().world.tick < start + 30 {
            assert!(Instant::now() < deadline, "the snapshots stopped coming");
            joiner.tick(Input::default());
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(!joiner.replica().unwrap().desynced());
    }

    #[test]
    fn the_host_leaving_ends_the_friends_match() {
        let (host, mut joiner) = room_with_friend();
        drop(host); // closes the socket, so the relay closes the room
        let deadline = Instant::now() + Duration::from_secs(10);
        while !matches!(joiner.status(), Status::Failed(_)) {
            assert!(Instant::now() < deadline, "the joiner never noticed");
            joiner.tick(Input::default());
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(joiner.status(), Status::Failed("THE HOST LEFT - THE MATCH IS OVER"));
    }
}
