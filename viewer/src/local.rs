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
use protocol::{decode_client, decode_host, encode_client, encode_host, ClientMsg, HostMsg, PlayerId};
use sdf::{Grid, Op, Shape};
use sim::{Input, DT};

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
        link.host_send(&HostMsg::Welcome(welcome));
        let replica = match link.client_recv() {
            // Delay 0: no interpolation. The wire is a function call, so there
            // is no jitter to hide, and the local player gets no extra lag.
            Some(HostMsg::Welcome(w)) => Replica::new(w, map, 0),
            _ => unreachable!("the loopback delivers the Welcome we just sent"),
        };
        LocalGame { host, link, peer, replica }
    }

    /// One 1/60 s tick of the whole pipeline: the player's wishes go to the
    /// host, the host steps the sim, and the new snapshot comes back and is
    /// shown. Call it once per fixed step.
    pub fn tick(&mut self, input: Input) {
        // The client's half: "this is what I want".
        self.link.client_send(&ClientMsg::Input(input));

        // The host's half: read the inputs that arrived, step the sim, and
        // send the result out (every tick: the local wire is free).
        while let Some(msg) = self.link.host_recv() {
            self.host.handle(self.peer, msg);
        }
        self.link.host_send(&HostMsg::Snapshot(self.host.tick()));

        // The client's half again: apply everything the host said.
        while let Some(msg) = self.link.client_recv() {
            self.replica.apply(msg);
        }
        self.replica.advance(DT);
    }

    /// The fly-mode brush: a developer tool that edits the host's terrain (see
    /// `Host::debug_edit`). It shows up on screen with the next tick.
    pub fn debug_edit(&mut self, shape: Shape, op: Op) {
        self.host.debug_edit(shape, op);
    }
}
