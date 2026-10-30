// What the relay server and its clients say to each other.
//
// The relay is deliberately dumb. It knows about ROOMS (a host plus the
// friends who joined by code) and it forwards opaque bytes between them. It
// never looks inside `Data`: those bytes are `ClientMsg` / `HostMsg`, and only
// the host and the clients understand them. So the game can change its
// messages freely without touching the relay, and the relay holds no game
// rules for anybody to cheat on.
//
//   friend ──┐                       ┌── host (the browser running the Host)
//   friend ──┼── relay (rooms) ──────┘
//   friend ──┘
//
// Why a relay at all? Browsers cannot accept incoming connections, so two
// browsers cannot connect to each other directly. Both connect OUT to a
// server that both can reach, and that server passes messages along.

use serde::{Deserialize, Serialize};

/// How the relay names the connections inside one room. The host is always
/// `HOST_PEER`; joiners get 1, 2, 3... The relay (not the client) assigns
/// them, so nobody can forge a "from".
pub type PeerId = u8;
pub const HOST_PEER: PeerId = 0;

/// A room holds the host plus at most this many joiners.
pub const MAX_PEERS: usize = 5;

/// Room codes are this many characters from `CODE_ALPHABET`. We leave out
/// look-alikes (0/O, 1/I) because people read these codes aloud.
pub const CODE_LEN: usize = 4;
pub const CODE_ALPHABET: &str = "ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

/// Which build of the game's wire format and rules this is. The relay never
/// decodes game bytes, so it cannot tell an old client from a new one by
/// itself. Instead the host states its version when it opens a room and a
/// joiner states its own: the relay only compares the two numbers.
///
/// BUMP THIS whenever `ClientMsg`/`HostMsg`/`Snapshot`/`Event`, `sdf::Shape`
/// or the map generator change. Without it a stale browser tab would decode
/// the new bytes into nonsense (bincode has no field names) or fail silently.
pub const PROTOCOL_VERSION: u16 = 2;

/// Client -> relay.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum ToRelay {
    /// "Open a room and make me its host."
    CreateRoom { version: u16 },
    /// "Run a match for me on the server." The server opens a room, hosts it
    /// itself (see server/src/matches.rs) and puts this connection in it as an
    /// ordinary joiner: it answers `RoomCreated` and then `RoomJoined`. The
    /// code works for friends exactly as with `CreateRoom`.
    CreateHostedRoom { version: u16 },
    /// "Put me in the room with this code." Refused with
    /// `Reject::VersionMismatch` unless `version` equals the host's.
    JoinRoom { code: String, version: u16 },
    /// Forward `bytes` to someone in my room.
    ///   * from the host: `to` picks one peer, or `None` for everyone;
    ///   * from a joiner: always goes to the host and `to` is ignored (a
    ///     joiner can only talk to the host, never to other joiners).
    Data { to: Option<PeerId>, bytes: Vec<u8> },
    /// Round-trip test to the relay itself. Also keeps a free-tier server
    /// awake while a match is idle.
    Ping { token: u32 },
}

/// Relay -> client.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum FromRelay {
    /// Answer to `CreateRoom`: share this code with your friends.
    RoomCreated { code: String },
    /// Answer to `JoinRoom`: you are in.
    RoomJoined,
    /// A request was refused.
    Rejected(Reject),
    /// Host only: somebody joined your room.
    PeerJoined(PeerId),
    /// Host only: somebody left your room.
    PeerLeft(PeerId),
    /// Joiners only: the host left, so the room is gone.
    RoomClosed,
    /// Bytes from another member of the room. `from` is set by the relay.
    Data { from: PeerId, bytes: Vec<u8> },
    Pong { token: u32 },
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reject {
    NoSuchRoom,
    RoomFull,
    /// You already host or have joined a room on this connection.
    AlreadyInRoom,
    /// The room's host runs a different build (`PROTOCOL_VERSION`): one of you
    /// needs to reload or update.
    VersionMismatch,
    /// The server cannot take on another hosted match right now.
    ServerBusy,
}
