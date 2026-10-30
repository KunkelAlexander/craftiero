// The host session: the bridge between a `Host` (the match) and a relay room
// (the friends).
//
//   friend ──> relay ──> Session::on_relay ──> Host::handle(slot, msg)
//   friend <── relay <── Session::after_tick <── Host::tick() snapshot
//
// It is the only code that knows both the relay protocol (peers, rooms) and the
// match (slots). `Host` knows nothing about the relay and the relay knows
// nothing about the match. Like `Host` it owns no socket and no clock: it takes
// decoded relay messages and RETURNS the relay messages to send, so the same
// code serves a player's client hosting a room (viewer) and, later, a server
// hosting a match (server), and can be tested by plain function calls.
//
// Networking concept: late join. A newcomer is welcomed with "the whole state
// right now", but the match keeps moving, so the Welcome must be exactly
// consistent with the snapshots that follow it. See `after_tick` for the one
// trap in that.

use std::collections::HashMap;

use protocol::relay::PeerId;
use protocol::{
    decode_client, encode_host, welcome_messages, ClientMsg, FromRelay, HostMsg, PlayerId, Snapshot, ToRelay,
};

use crate::{Host, Throttle};

/// Send friends a snapshot every N sim ticks: 2 = 30 per second.
///
/// The trade-off (one worm is ~450 bytes on the wire, so 4 players are about
/// 2 KB a snapshot):
///   1 -> 60/s -> ~110 KB/s of host upload, friends see 60 Hz steps
///   2 -> 30/s -> ~55 KB/s,                 friends see 30 Hz steps
///   3 -> 20/s -> ~37 KB/s,                 friends see 20 Hz steps (choppy)
/// 30 is the usual compromise; interpolation hides the steps. Only the link to
/// friends is throttled: the host's own player is wired in memory and keeps 60.
const SEND_EVERY: u32 = 2;

pub struct Session {
    throttle: Throttle,
    /// Which player slot each friend got. The relay numbers connections
    /// (`PeerId`), the match numbers players (`PlayerId`); this is the bridge.
    peers: HashMap<PeerId, PlayerId>,
    /// Friends waiting for their Welcome (new arrivals, and friends who asked
    /// for a resync). Served by `after_tick`.
    welcome_due: Vec<PeerId>,
}

impl Default for Session {
    fn default() -> Session {
        Session::new()
    }
}

impl Session {
    pub fn new() -> Session {
        Session { throttle: Throttle::new(SEND_EVERY), peers: HashMap::new(), welcome_due: Vec::new() }
    }

    /// How many friends hold a slot.
    pub fn friends(&self) -> usize {
        self.peers.len()
    }

    /// Nobody is in the match and nobody is waiting to be let in.
    pub fn is_empty(&self) -> bool {
        self.peers.is_empty() && self.welcome_due.is_empty()
    }

    /// Something arrived from the relay. Returns what to send back.
    pub fn on_relay(&mut self, host: &mut Host, msg: FromRelay) -> Vec<ToRelay> {
        match msg {
            // Do not welcome yet: see `after_tick`.
            FromRelay::PeerJoined(peer) => {
                if !self.welcome_due.contains(&peer) {
                    self.welcome_due.push(peer);
                }
                Vec::new()
            }
            FromRelay::PeerLeft(peer) => {
                self.welcome_due.retain(|p| *p != peer);
                if let Some(slot) = self.peers.remove(&peer) {
                    host.leave(slot);
                }
                Vec::new()
            }
            FromRelay::Data { from, bytes } => self.on_data(host, from, &bytes),
            // Messages for the connection itself or for joiners: the code that
            // owns the connection handles them (room code, keep-alive...).
            FromRelay::RoomCreated { .. }
            | FromRelay::RoomJoined
            | FromRelay::RoomClosed
            | FromRelay::Rejected(_)
            | FromRelay::Pong { .. } => Vec::new(),
        }
    }

    /// Call after every `Host::tick` with its snapshot. Returns what to send:
    /// the snapshot for the friends (every `SEND_EVERY`-th tick) and the
    /// Welcomes that were waiting.
    ///
    /// Why Welcomes wait for a send tick: the throttle holds back the events
    /// of ticks it does not send. A Welcome made in the middle of that window
    /// would already contain those events' edits in its log, and the next
    /// snapshot would carry them again: the newcomer would dig the same holes
    /// twice and its edit count would not match the host's. Right after a send
    /// tick nothing is held back, so Welcome and snapshots line up exactly.
    /// The price is at most one extra tick (1/60 s) before a friend is let in.
    pub fn after_tick(&mut self, host: &mut Host, snapshot: Snapshot) -> Vec<ToRelay> {
        let mut out = Vec::new();
        let send_tick = snapshot.tick % SEND_EVERY == 0;

        if self.peers.is_empty() {
            // Nobody listening: spend no upload, and forget the events held
            // back for them. They are older than whatever a future joiner's
            // Welcome will say and replaying them would double the edits.
            self.throttle = Throttle::new(SEND_EVERY);
        } else if let Some(snapshot) = self.throttle.push(snapshot) {
            // One `Data { to: None }`: the relay copies it to everybody, so the
            // host uploads each snapshot once however many friends there are.
            let bytes = encode_host(&HostMsg::Snapshot(snapshot));
            out.push(ToRelay::Data { to: None, bytes });
        }

        if send_tick {
            for peer in std::mem::take(&mut self.welcome_due) {
                self.welcome(host, peer, &mut out);
            }
        }
        out
    }

    /// The relay connection is gone, and every friend with it: their worms go
    /// back to being dummies.
    pub fn drop_everyone(&mut self, host: &mut Host) {
        for (_, slot) in self.peers.drain() {
            host.leave(slot);
        }
        self.welcome_due.clear();
        self.throttle = Throttle::new(SEND_EVERY);
    }

    /// Give `peer` a slot (or find the one it has) and send the full state.
    fn welcome(&mut self, host: &mut Host, peer: PeerId, out: &mut Vec<ToRelay>) {
        let welcome = match self.peers.get(&peer) {
            // Resync: same slot, new Welcome.
            Some(&slot) => host.rewelcome(slot),
            None => host.join().inspect(|w| {
                self.peers.insert(peer, w.you);
            }),
        };
        match welcome {
            Some(welcome) => {
                for msg in welcome_messages(welcome) {
                    out.push(send_to(peer, &msg));
                }
            }
            None => out.push(send_to(peer, &HostMsg::Full)),
        }
    }

    fn on_data(&mut self, host: &mut Host, from: PeerId, bytes: &[u8]) -> Vec<ToRelay> {
        // `from` was stamped by the relay and the slot comes from OUR table,
        // never from the message, so a friend cannot act as another player.
        // Someone without a slot (the match was full, or not welcomed yet) is
        // ignored.
        let Some(&slot) = self.peers.get(&from) else { return Vec::new() };
        match decode_client(bytes) {
            Some(ClientMsg::Ping { token }) => vec![send_to(from, &HostMsg::Pong { token })],
            Some(ClientMsg::Resync) => {
                if !self.welcome_due.contains(&from) {
                    self.welcome_due.push(from);
                }
                Vec::new()
            }
            Some(msg) => {
                host.handle(slot, msg);
                Vec::new()
            }
            None => Vec::new(), // garbage is dropped, never fatal
        }
    }
}

fn send_to(peer: PeerId, msg: &HostMsg) -> ToRelay {
    ToRelay::Data { to: Some(peer), bytes: encode_host(msg) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::{IVec3, Vec2, Vec3};
    use protocol::replica::Replica;
    use protocol::{decode_host, encode_client, WelcomeBuilder};
    use sdf::{Grid, Op, Shape};
    use sim::Input;

    fn ground() -> Grid {
        Grid::from_fn(IVec3::new(8, 4, 8), |p| p.y - 4.0)
    }

    /// Two seats, like `host::tests`.
    fn host() -> Host {
        Host::new(ground(), &[(8.0, 16.0), (24.0, 16.0)], 7)
    }

    /// A friend's end of the connection: what `viewer/src/join.rs` does with the
    /// bytes, without a socket.
    #[derive(Default)]
    struct Friend {
        builder: WelcomeBuilder,
        replica: Option<Replica>,
        got_full: bool,
    }

    impl Friend {
        /// Take whatever the session addressed to `peer` (or to everyone).
        fn hear(&mut self, peer: PeerId, sent: &[ToRelay]) {
            for msg in sent {
                let ToRelay::Data { to, bytes } = msg else { continue };
                if to.is_some_and(|t| t != peer) {
                    continue;
                }
                let Some(msg) = decode_host(bytes) else { continue };
                if let Some(welcome) = self.builder.push(&msg) {
                    self.replica = Some(Replica::new(welcome, ground(), 0));
                } else if matches!(msg, HostMsg::Full) {
                    self.got_full = true;
                } else if let Some(replica) = &mut self.replica {
                    replica.apply(msg);
                    replica.advance(0.0);
                }
            }
        }
    }

    /// One host tick, with the session's output delivered to `friends`.
    fn tick(host: &mut Host, session: &mut Session, friends: &mut [(PeerId, &mut Friend)]) {
        let snapshot = host.tick();
        let sent = session.after_tick(host, snapshot);
        for (peer, friend) in friends.iter_mut() {
            friend.hear(*peer, &sent);
        }
    }

    fn to_host(session: &mut Session, host: &mut Host, from: PeerId, msg: &ClientMsg) -> Vec<ToRelay> {
        session.on_relay(host, FromRelay::Data { from, bytes: encode_client(msg) })
    }

    #[test]
    fn a_friend_is_welcomed_and_then_follows_the_match() {
        let (mut host, mut session) = (host(), Session::new());
        let mut a = Friend::default();

        session.on_relay(&mut host, FromRelay::PeerJoined(1));
        for _ in 0..4 {
            tick(&mut host, &mut session, &mut [(1, &mut a)]);
        }
        let replica = a.replica.as_ref().expect("welcomed within a couple of ticks");
        assert_eq!(replica.me, 0);
        assert_eq!(session.friends(), 1);
        assert_eq!(replica.world.tick, host.world().tick - host.world().tick % SEND_EVERY);
        assert!(!replica.desynced());
    }

    /// The trap `after_tick` documents. Every tick makes an edit, so whichever
    /// tick the second friend arrives on, there are edits held back for the
    /// first one. Neither may end up with a doubled or missing edit.
    #[test]
    fn a_friend_who_joins_mid_throttle_window_gets_no_edit_twice() {
        for settle_ticks in 0..3 {
            let (mut host, mut session) = (host(), Session::new());
            let (mut a, mut b) = (Friend::default(), Friend::default());
            let dig = |host: &mut Host, i: u32| {
                host.debug_edit(Shape::Sphere { center: Vec3::new(2.0 + i as f32 * 0.5, 4.0, 16.0), radius: 0.7 }, Op::Carve);
            };

            session.on_relay(&mut host, FromRelay::PeerJoined(1));
            let mut i = 0;
            for _ in 0..6 {
                dig(&mut host, i);
                i += 1;
                tick(&mut host, &mut session, &mut [(1, &mut a)]);
            }
            // B arrives; `settle_ticks` shifts which tick of the window that is.
            for _ in 0..settle_ticks {
                dig(&mut host, i);
                i += 1;
                tick(&mut host, &mut session, &mut [(1, &mut a)]);
            }
            session.on_relay(&mut host, FromRelay::PeerJoined(2));
            for _ in 0..20 {
                dig(&mut host, i);
                i += 1;
                tick(&mut host, &mut session, &mut [(1, &mut a), (2, &mut b)]);
            }

            for (name, friend) in [("a", &a), ("b", &b)] {
                let r = friend.replica.as_ref().unwrap();
                assert!(!r.desynced(), "friend {name} desynced (arrival shifted by {settle_ticks})");
            }
            assert_eq!(b.replica.as_ref().unwrap().me, 1);
        }
    }

    #[test]
    fn a_third_friend_is_told_the_match_is_full() {
        let (mut host, mut session) = (host(), Session::new());
        let (mut a, mut b, mut c) = (Friend::default(), Friend::default(), Friend::default());
        for peer in 1..=3 {
            session.on_relay(&mut host, FromRelay::PeerJoined(peer));
        }
        for _ in 0..3 {
            tick(&mut host, &mut session, &mut [(1, &mut a), (2, &mut b), (3, &mut c)]);
        }
        assert!(a.replica.is_some() && b.replica.is_some());
        assert!(c.got_full && c.replica.is_none());
        assert_eq!(session.friends(), 2);
    }

    #[test]
    fn a_friends_input_moves_their_own_worm_only() {
        let (mut host, mut session) = (host(), Session::new());
        let (mut a, mut b) = (Friend::default(), Friend::default());
        session.on_relay(&mut host, FromRelay::PeerJoined(1));
        session.on_relay(&mut host, FromRelay::PeerJoined(2));
        for _ in 0..3 {
            tick(&mut host, &mut session, &mut [(1, &mut a), (2, &mut b)]);
        }
        for w in &mut host.world.worms {
            w.pos.y = 4.05;
        }
        let start: Vec<Vec3> = host.world().worms.iter().map(|w| w.pos).collect();

        let walk = Input { move_dir: Vec2::new(0.0, 1.0), yaw: 1.5, ..Input::default() };
        to_host(&mut session, &mut host, 1, &ClientMsg::Input(walk));
        for _ in 0..30 {
            tick(&mut host, &mut session, &mut [(1, &mut a), (2, &mut b)]);
        }
        let end: Vec<Vec3> = host.world().worms.iter().map(|w| w.pos).collect();
        assert!(end[0].x > start[0].x + 1.0, "peer 1 drives worm 0");
        assert_eq!((end[1].x, end[1].z), (start[1].x, start[1].z), "worm 1 stays put");
    }

    #[test]
    fn someone_who_was_never_welcomed_is_ignored() {
        let (mut host, mut session) = (host(), Session::new());
        let out = to_host(&mut session, &mut host, 9, &ClientMsg::Ping { token: 5 });
        assert!(out.is_empty(), "no slot, no answer");
    }

    #[test]
    fn a_ping_is_answered_to_the_sender_alone() {
        let (mut host, mut session) = (host(), Session::new());
        session.on_relay(&mut host, FromRelay::PeerJoined(1));
        for _ in 0..2 {
            let s = host.tick();
            session.after_tick(&mut host, s);
        }
        let out = to_host(&mut session, &mut host, 1, &ClientMsg::Ping { token: 77 });
        let [ToRelay::Data { to: Some(1), bytes }] = &out[..] else { panic!("expected one reply, got {out:?}") };
        assert!(matches!(decode_host(bytes), Some(HostMsg::Pong { token: 77 })));
    }

    #[test]
    fn garbage_from_a_friend_is_dropped() {
        let (mut host, mut session) = (host(), Session::new());
        session.on_relay(&mut host, FromRelay::PeerJoined(1));
        let s = host.tick();
        session.after_tick(&mut host, s);
        let s = host.tick();
        session.after_tick(&mut host, s);
        let out = session.on_relay(&mut host, FromRelay::Data { from: 1, bytes: vec![0xFF; 50] });
        assert!(out.is_empty());
    }

    #[test]
    fn leaving_frees_the_slot_for_the_next_arrival() {
        let (mut host, mut session) = (host(), Session::new());
        let (mut a, mut b, mut c) = (Friend::default(), Friend::default(), Friend::default());
        session.on_relay(&mut host, FromRelay::PeerJoined(1));
        session.on_relay(&mut host, FromRelay::PeerJoined(2));
        for _ in 0..3 {
            tick(&mut host, &mut session, &mut [(1, &mut a), (2, &mut b)]);
        }
        session.on_relay(&mut host, FromRelay::PeerLeft(1));
        assert_eq!(session.friends(), 1);
        session.on_relay(&mut host, FromRelay::PeerJoined(3));
        for _ in 0..3 {
            tick(&mut host, &mut session, &mut [(2, &mut b), (3, &mut c)]);
        }
        assert_eq!(c.replica.as_ref().expect("welcomed").me, 0, "took the slot peer 1 left");
    }

    #[test]
    fn a_resync_sends_a_fresh_welcome_that_repairs_the_replica() {
        let (mut host, mut session) = (host(), Session::new());
        let mut a = Friend::default();
        session.on_relay(&mut host, FromRelay::PeerJoined(1));
        for _ in 0..4 {
            tick(&mut host, &mut session, &mut [(1, &mut a)]);
        }
        // The friend loses an edit: one the host made but never reached them.
        host.debug_edit(Shape::Sphere { center: Vec3::new(16.0, 4.0, 16.0), radius: 2.0 }, Op::Carve);
        // The edit reaches the next snapshot that is sent; that message is lost.
        loop {
            let snapshot = host.tick();
            if !session.after_tick(&mut host, snapshot).is_empty() {
                break;
            }
        }
        for _ in 0..4 {
            tick(&mut host, &mut session, &mut [(1, &mut a)]);
        }
        assert!(a.replica.as_ref().unwrap().desynced(), "the missing edit is noticed");

        to_host(&mut session, &mut host, 1, &ClientMsg::Resync);
        for _ in 0..4 {
            tick(&mut host, &mut session, &mut [(1, &mut a)]);
        }
        let r = a.replica.as_ref().unwrap();
        assert!(!r.desynced(), "a new Replica was built from the new Welcome");
        assert_eq!(r.world.grid.checksum(), host.world().grid.checksum());
        assert_eq!(session.friends(), 1, "same friend, same slot");
    }

    #[test]
    fn dropping_everyone_empties_the_session() {
        let (mut host, mut session) = (host(), Session::new());
        let mut a = Friend::default();
        session.on_relay(&mut host, FromRelay::PeerJoined(1));
        for _ in 0..3 {
            tick(&mut host, &mut session, &mut [(1, &mut a)]);
        }
        session.drop_everyone(&mut host);
        assert_eq!(session.friends(), 0);
        assert!(host.join().is_some() && host.join().is_some(), "both slots are free again");
    }

    #[test]
    fn nobody_listening_means_nothing_is_sent() {
        let (mut host, mut session) = (host(), Session::new());
        for _ in 0..4 {
            let s = host.tick();
            assert!(session.after_tick(&mut host, s).is_empty());
        }
    }
}
