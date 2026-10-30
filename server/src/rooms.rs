// The relay's brain: who is in which room, and who gets which message.
//
// This is plain synchronous code with no sockets in it. Each connection is
// represented by a `Tx`, a channel that queues bytes to send to that client.
// Keeping it free of networking means the whole protocol can be unit-tested by
// calling functions (see the tests at the bottom).

use std::collections::{BTreeMap, HashMap};

use protocol::relay::{CODE_ALPHABET, CODE_LEN, HOST_PEER, MAX_PEERS};
use protocol::{encode_from_relay, FromRelay, Reject, ToRelay};
use rand::seq::SliceRandom;
use tokio::sync::mpsc::UnboundedSender;

use protocol::relay::PeerId;

/// Bytes waiting to be written to one client's WebSocket.
pub type Tx = UnboundedSender<Vec<u8>>;

/// What one connection currently is. Starts as `Lobby` (connected, no room).
///
/// Pattern: State. What a message means depends on the sender's role: `Data`
/// from a host fans out to peers, `Data` from a joiner goes to the host.
pub enum Role {
    Lobby,
    Host { code: String },
    Joiner { code: String, peer: PeerId },
}

struct Room {
    host: Tx,
    /// The host's `PROTOCOL_VERSION`; joiners must match it.
    version: u16,
    peers: BTreeMap<PeerId, Tx>,
}

#[derive(Default)]
pub struct Rooms {
    rooms: HashMap<String, Room>,
    /// Matches the server itself is running (see matches.rs), counted so it
    /// can refuse more than it can carry.
    pub hosted_matches: usize,
}

fn send(tx: &Tx, msg: &FromRelay) {
    // If the receiver is gone the connection is closing and `leave` will tidy
    // up, so a failed send is not an error worth handling here.
    let _ = tx.send(encode_from_relay(msg));
}

impl Rooms {
    /// Handle one message from a connection. `tx` is that connection's own
    /// outgoing queue; `role` is updated if the message changes it.
    pub fn handle(&mut self, role: &mut Role, tx: &Tx, msg: ToRelay) {
        match msg {
            ToRelay::CreateRoom { version } => {
                if !matches!(role, Role::Lobby) {
                    return send(tx, &FromRelay::Rejected(Reject::AlreadyInRoom));
                }
                let code = self.new_code();
                let room = Room { host: tx.clone(), version, peers: BTreeMap::new() };
                self.rooms.insert(code.clone(), room);
                send(tx, &FromRelay::RoomCreated { code: code.clone() });
                *role = Role::Host { code };
            }

            // Needs a task that runs the match, which this synchronous code
            // cannot start; `connection` in lib.rs hands it to `matches::start`
            // before it ever gets here.
            ToRelay::CreateHostedRoom { .. } => {}

            ToRelay::JoinRoom { code, version } => {
                if !matches!(role, Role::Lobby) {
                    return send(tx, &FromRelay::Rejected(Reject::AlreadyInRoom));
                }
                // Be forgiving: people type codes in lower case.
                let code = code.trim().to_uppercase();
                let Some(room) = self.rooms.get_mut(&code) else {
                    return send(tx, &FromRelay::Rejected(Reject::NoSuchRoom));
                };
                if room.version != version {
                    return send(tx, &FromRelay::Rejected(Reject::VersionMismatch));
                }
                // Lowest free id from 1 up. Ids are reused after someone leaves.
                let free = (1..=MAX_PEERS as PeerId).find(|p| !room.peers.contains_key(p));
                let Some(peer) = free else {
                    return send(tx, &FromRelay::Rejected(Reject::RoomFull));
                };
                room.peers.insert(peer, tx.clone());
                send(tx, &FromRelay::RoomJoined);
                send(&room.host, &FromRelay::PeerJoined(peer));
                *role = Role::Joiner { code, peer };
            }

            ToRelay::Data { to, bytes } => match role {
                Role::Lobby => {}
                Role::Host { code } => {
                    let Some(room) = self.rooms.get(code.as_str()) else { return };
                    let msg = FromRelay::Data { from: HOST_PEER, bytes };
                    match to {
                        Some(peer) => {
                            if let Some(tx) = room.peers.get(&peer) {
                                send(tx, &msg);
                            }
                        }
                        None => room.peers.values().for_each(|tx| send(tx, &msg)),
                    }
                }
                Role::Joiner { code, peer } => {
                    // The relay stamps `from` itself, so a joiner cannot
                    // pretend to be someone else.
                    if let Some(room) = self.rooms.get(code.as_str()) {
                        send(&room.host, &FromRelay::Data { from: *peer, bytes });
                    }
                }
            },

            ToRelay::Ping { token } => send(tx, &FromRelay::Pong { token }),
        }
    }

    /// A connection ended (cleanly or not). Tell the others and tidy up.
    pub fn leave(&mut self, role: &Role) {
        match role {
            Role::Lobby => {}
            // The host is the authority: without it there is no match, so the
            // room closes and everyone in it is told.
            Role::Host { code } => {
                if let Some(room) = self.rooms.remove(code.as_str()) {
                    room.peers.values().for_each(|tx| send(tx, &FromRelay::RoomClosed));
                }
            }
            Role::Joiner { code, peer } => {
                if let Some(room) = self.rooms.get_mut(code.as_str()) {
                    room.peers.remove(peer);
                    send(&room.host, &FromRelay::PeerLeft(*peer));
                }
            }
        }
    }

    pub fn room_count(&self) -> usize {
        self.rooms.len()
    }

    fn new_code(&self) -> String {
        let letters: Vec<char> = CODE_ALPHABET.chars().collect();
        let mut rng = rand::thread_rng();
        loop {
            let code: String = (0..CODE_LEN).map(|_| *letters.choose(&mut rng).unwrap()).collect();
            if !self.rooms.contains_key(&code) {
                return code;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{decode_from_relay, PROTOCOL_VERSION};
    use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

    /// A fake connection: its role, its outgoing queue, and the receiving end
    /// of that queue so the test can see what the relay sent it.
    struct Conn {
        role: Role,
        tx: Tx,
        rx: UnboundedReceiver<Vec<u8>>,
    }

    fn conn() -> Conn {
        let (tx, rx) = unbounded_channel();
        Conn { role: Role::Lobby, tx, rx }
    }

    impl Conn {
        fn say(&mut self, rooms: &mut Rooms, msg: ToRelay) {
            rooms.handle(&mut self.role, &self.tx, msg);
        }

        /// Everything the relay has sent this connection so far.
        fn heard(&mut self) -> Vec<FromRelay> {
            let mut out = Vec::new();
            while let Ok(bytes) = self.rx.try_recv() {
                out.push(decode_from_relay(&bytes).unwrap());
            }
            out
        }
    }

    /// Host creates a room; returns its code.
    fn open_room(rooms: &mut Rooms, host: &mut Conn) -> String {
        host.say(rooms, ToRelay::CreateRoom { version: PROTOCOL_VERSION });
        match host.heard().pop() {
            Some(FromRelay::RoomCreated { code }) => code,
            other => panic!("expected RoomCreated, got {other:?}"),
        }
    }

    #[test]
    fn host_creates_room_and_friend_joins_by_code() {
        let mut rooms = Rooms::default();
        let (mut host, mut friend) = (conn(), conn());
        let code = open_room(&mut rooms, &mut host);
        assert_eq!(code.len(), CODE_LEN);

        friend.say(&mut rooms, ToRelay::JoinRoom { code: code.to_lowercase(), version: PROTOCOL_VERSION });
        assert!(matches!(friend.heard()[..], [FromRelay::RoomJoined]));
        assert!(matches!(host.heard()[..], [FromRelay::PeerJoined(1)]));
    }

    #[test]
    fn data_flows_host_to_peer_and_back_with_the_sender_stamped() {
        let mut rooms = Rooms::default();
        let (mut host, mut a, mut b) = (conn(), conn(), conn());
        let code = open_room(&mut rooms, &mut host);
        a.say(&mut rooms, ToRelay::JoinRoom { code: code.clone(), version: PROTOCOL_VERSION });
        b.say(&mut rooms, ToRelay::JoinRoom { code, version: PROTOCOL_VERSION });
        host.heard();
        a.heard();
        b.heard();

        // A joiner's data goes to the host, stamped with its peer id.
        a.say(&mut rooms, ToRelay::Data { to: Some(2), bytes: vec![7] }); // `to` is ignored
        assert!(matches!(&host.heard()[..], [FromRelay::Data { from: 1, bytes }] if bytes == &[7]));
        assert!(b.heard().is_empty(), "joiners cannot talk to each other");

        // The host can answer one peer...
        host.say(&mut rooms, ToRelay::Data { to: Some(2), bytes: vec![8] });
        assert!(a.heard().is_empty());
        assert!(matches!(&b.heard()[..], [FromRelay::Data { from: 0, bytes }] if bytes == &[8]));
        // ...or everybody.
        host.say(&mut rooms, ToRelay::Data { to: None, bytes: vec![9] });
        assert_eq!(a.heard().len(), 1);
        assert_eq!(b.heard().len(), 1);
    }

    #[test]
    fn bad_requests_are_rejected() {
        let mut rooms = Rooms::default();
        let (mut host, mut friend) = (conn(), conn());
        friend.say(&mut rooms, ToRelay::JoinRoom { code: "ZZZZ".into(), version: PROTOCOL_VERSION });
        assert!(matches!(friend.heard()[..], [FromRelay::Rejected(Reject::NoSuchRoom)]));

        open_room(&mut rooms, &mut host);
        host.say(&mut rooms, ToRelay::CreateRoom { version: PROTOCOL_VERSION });
        assert!(matches!(host.heard()[..], [FromRelay::Rejected(Reject::AlreadyInRoom)]));
    }

    #[test]
    fn a_joiner_from_another_build_is_turned_away() {
        let mut rooms = Rooms::default();
        let (mut host, mut friend) = (conn(), conn());
        let code = open_room(&mut rooms, &mut host);

        friend.say(&mut rooms, ToRelay::JoinRoom { code, version: PROTOCOL_VERSION + 1 });
        assert!(matches!(friend.heard()[..], [FromRelay::Rejected(Reject::VersionMismatch)]));
        // The host never hears about it, and the friend can still try again
        // from the lobby (with the right build).
        assert!(host.heard().is_empty());
        assert!(matches!(friend.role, Role::Lobby));
    }

    #[test]
    fn room_fills_up_and_ids_are_reused() {
        let mut rooms = Rooms::default();
        let mut host = conn();
        let code = open_room(&mut rooms, &mut host);
        let mut joiners: Vec<Conn> = (0..MAX_PEERS).map(|_| conn()).collect();
        for j in &mut joiners {
            j.say(&mut rooms, ToRelay::JoinRoom { code: code.clone(), version: PROTOCOL_VERSION });
        }
        let mut extra = conn();
        extra.say(&mut rooms, ToRelay::JoinRoom { code: code.clone(), version: PROTOCOL_VERSION });
        assert!(matches!(extra.heard()[..], [FromRelay::Rejected(Reject::RoomFull)]));

        // Peer 3 leaves; the next arrival takes id 3.
        rooms.leave(&joiners[2].role);
        extra.say(&mut rooms, ToRelay::JoinRoom { code, version: PROTOCOL_VERSION });
        assert!(matches!(extra.role, Role::Joiner { peer: 3, .. }));
    }

    #[test]
    fn host_leaving_closes_the_room() {
        let mut rooms = Rooms::default();
        let (mut host, mut friend) = (conn(), conn());
        let code = open_room(&mut rooms, &mut host);
        friend.say(&mut rooms, ToRelay::JoinRoom { code, version: PROTOCOL_VERSION });
        friend.heard();

        rooms.leave(&host.role);
        assert!(matches!(friend.heard()[..], [FromRelay::RoomClosed]));
        assert_eq!(rooms.room_count(), 0);
    }

    #[test]
    fn joiner_leaving_is_reported_to_the_host() {
        let mut rooms = Rooms::default();
        let (mut host, mut friend) = (conn(), conn());
        let code = open_room(&mut rooms, &mut host);
        friend.say(&mut rooms, ToRelay::JoinRoom { code, version: PROTOCOL_VERSION });
        host.heard();

        rooms.leave(&friend.role);
        assert!(matches!(host.heard()[..], [FromRelay::PeerLeft(1)]));
    }

    #[test]
    fn ping_is_answered_directly() {
        let mut rooms = Rooms::default();
        let mut c = conn();
        c.say(&mut rooms, ToRelay::Ping { token: 42 });
        assert!(matches!(c.heard()[..], [FromRelay::Pong { token: 42 }]));
    }
}
