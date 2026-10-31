// The language that clients and the host speak. Nothing in here does anything:
// it is only message shapes plus turning them into bytes and back.
//
// Why a crate of its own: the game (client), the host and the relay server all
// have to agree on these shapes. One shared definition means they cannot drift
// apart, and the compiler checks every side whenever a message changes.
//
// The conversation, in full:
//
//   client                                  host
//     │ ── ClientMsg::Input ────────────────> │   "here is what my player wants"
//     │                                       │   (host steps the sim)
//     │ <──────────── HostMsg::Welcome ────── │   once, when you join
//     │ <──────────── HostMsg::Snapshot ───── │   again and again: "here is the world"
//
// Networking concepts used below:
//   * Authority: only the host changes the world. Clients send *wishes*
//     (inputs) and receive *facts* (snapshots). A client cannot say "I hit
//     him" or "I am at x=5"; it can only say "I am pressing fire".
//   * Tick: the sim's step counter. It labels snapshots so a client knows which
//     moment of the match it is looking at.

pub mod relay;
pub mod replica;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
pub use relay::{FromRelay, Reject, ToRelay, PROTOCOL_VERSION};
use sdf::{Op, Shape};
use sim::{Event, Input, LavaBit, Projectile, Worm, Zone};

/// Which player slot a connection controls. The host decides it, never the
/// client (otherwise a client could claim to be someone else).
pub type PlayerId = u8;

/// Client -> host.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum ClientMsg {
    /// The player's wishes for now. Sent once per rendered frame; the host
    /// keeps only the newest one per player.
    Input(Input),
    /// "Are you there? How long does a round trip take?" The token is echoed
    /// back unchanged in `HostMsg::Pong`.
    Ping { token: u32 },
    /// The weapons this player picked for the match (ids into `WEAPONS`).
    /// Sent once after each Welcome that says the players are still choosing.
    /// The host checks it and takes only the first one of a match: a second
    /// would be a free refill of every magazine.
    Loadout(Vec<u8>),
    /// "I think my terrain is wrong, start me over." Sent when the replica
    /// notices it missed an edit (`Replica::desynced`). The host answers with
    /// a fresh Welcome, exactly as for a rematch.
    Resync,
    /// "Play again." Only honoured once the match is over (`Snapshot::winner`),
    /// so a client cannot reset a match that is still running. Any player may
    /// ask: a match the server hosts has no owner to ask instead.
    Rematch,
}

/// Host -> client.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum HostMsg {
    /// First message of the late-join answer; `Edits` messages follow. See
    /// `welcome_messages` and `WelcomeBuilder`.
    Welcome(WelcomeHead),
    /// One slice of the edit log, in order, after a `Welcome`.
    Edits(Vec<(Shape, Op)>),
    Snapshot(Snapshot),
    Pong { token: u32 },
    /// Answer to a join when every slot is taken (there is no `Welcome`).
    /// Without it the newcomer would wait forever.
    Full,
}

/// Sent once to a new arrival: everything needed to start from scratch.
/// This is the "late joiner" message.
///
/// It is one value in memory, but too big for one wire message, so on the wire
/// it travels as a `WelcomeHead` plus `Edits` slices (see below).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Welcome {
    /// The slot this client controls.
    pub you: PlayerId,
    /// Which map the match is on (an index into `sim::map::MAPS`). Like the
    /// edits, only the number travels: the client builds the map itself.
    pub map: u8,
    /// Every terrain edit since the match began. The map itself is not sent:
    /// the generator is in both programs, so a client builds the untouched map
    /// and replays these on top. Snapshots only carry *new* edits
    /// (`Event::Edit`), so a newcomer needs the whole log exactly once.
    ///
    /// Size: ~50 bytes an edit, so a long match can dig tens of thousands of
    /// times: megabytes. That is why the wire cuts the log into slices of
    /// `EDITS_PER_MESSAGE` (each far below `MAX_MESSAGE_BYTES`). Sending the
    /// grid itself (or compacting the log) would be smaller still; not needed
    /// yet.
    pub edits: Vec<(Shape, Op)>,
    /// The world right now, so there is something valid to draw immediately.
    pub snapshot: Snapshot,
}

/// A `Welcome` without its edit log: the first wire message of a late join.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct WelcomeHead {
    pub you: PlayerId,
    pub map: u8,
    /// How many edits follow in `HostMsg::Edits` messages.
    pub edit_count: u32,
    pub snapshot: Snapshot,
}

/// How many edits go in one `HostMsg::Edits`. The biggest edit encodes to
/// about 60 bytes, so this stays around 30 KB: well under the 64 KB limit, and
/// a test keeps it that way if `Shape` ever grows.
pub const EDITS_PER_MESSAGE: usize = 500;

/// Cut a `Welcome` into the messages that go on the wire, in order. The host
/// sends them back to back, so (the connection being ordered) no snapshot can
/// arrive between them.
pub fn welcome_messages(welcome: Welcome) -> Vec<HostMsg> {
    let head = WelcomeHead { you: welcome.you, map: welcome.map, edit_count: welcome.edits.len() as u32, snapshot: welcome.snapshot };
    let mut msgs = vec![HostMsg::Welcome(head)];
    msgs.extend(welcome.edits.chunks(EDITS_PER_MESSAGE).map(|slice| HostMsg::Edits(slice.to_vec())));
    msgs
}

/// The receiving end of `welcome_messages`: feed it every host message and it
/// hands back the whole `Welcome` once the last slice has arrived.
#[derive(Default)]
pub struct WelcomeBuilder {
    /// The head we are waiting to complete, and the edits collected so far.
    pending: Option<(WelcomeHead, Vec<(Shape, Op)>)>,
}

impl WelcomeBuilder {
    /// Other messages (snapshots, pongs) are none of its business: ignored.
    pub fn push(&mut self, msg: &HostMsg) -> Option<Welcome> {
        match msg {
            // A new head always starts over: that is how a rematch or a
            // resync restarts a half-received one.
            HostMsg::Welcome(head) => {
                self.pending = Some((head.clone(), Vec::new()));
            }
            HostMsg::Edits(slice) => {
                if let Some((_, edits)) = &mut self.pending {
                    edits.extend_from_slice(slice);
                }
            }
            _ => return None,
        }
        let (head, edits) = self.pending.as_ref()?;
        if edits.len() < head.edit_count as usize {
            return None;
        }
        let (head, mut edits) = self.pending.take()?;
        // A host that sends more than it announced is broken; keep the promise.
        edits.truncate(head.edit_count as usize);
        Some(Welcome { you: head.you, map: head.map, edits, snapshot: head.snapshot })
    }
}

/// "This is what the world looks like." Everything that moves, plus what
/// happened since the previous snapshot.
///
/// The terrain is deliberately absent (see `Welcome`).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Snapshot {
    /// The sim tick this snapshot was taken after.
    pub tick: u32,
    pub worms: Vec<Worm>,
    pub projectiles: Vec<Projectile>,
    pub zones: Vec<Zone>,
    pub lava: Vec<LavaBit>,
    /// Events since the last snapshot. Clients turn them into explosions and
    /// sounds, and apply `Edit` to their copy of the terrain.
    pub events: Vec<Event>,
    /// How many terrain edits the host has made since the match began,
    /// counting this snapshot's. A client that has applied a different number
    /// by the time it shows this snapshot missed or doubled an edit, and its
    /// terrain is wrong (`Replica::desynced`). A count costs 4 bytes; hashing
    /// the grid to check the same thing took ~70 ms per call.
    pub edits_total: u32,
    /// The rules of the match and how it stands: the kills that win it, and
    /// who has, once somebody has. They ride in every snapshot (6 bytes)
    /// instead of in a message of their own, so a Welcome, which contains a
    /// snapshot, carries them to a late joiner with no extra code.
    pub score_limit: i32,
    pub winner: Option<u8>,
    /// The match has not begun: the players are picking their weapons and the
    /// world stands still until they have (or the time is up). Like `winner`
    /// it rides in every snapshot, so a late joiner learns it from the Welcome.
    pub choosing: bool,
}

// ---- bytes ---------------------------------------------------------------
//
// A network carries bytes, not Rust values. Every message is encoded right
// before sending and decoded right after receiving. We do this even for the
// in-process loopback so the encoding is exercised on every single run.

/// Anything bigger than this is rejected before decoding. The bytes come from
/// the outside world, so we never trust their size.
const MAX_MESSAGE_BYTES: usize = 64 * 1024;

fn encode<T: Serialize>(msg: &T) -> Vec<u8> {
    bincode::serialize(msg).expect("encoding a message cannot fail")
}

/// `None` means the bytes were garbage. Callers drop such messages: a peer
/// sending nonsense must never be able to crash us.
fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Option<T> {
    if bytes.len() > MAX_MESSAGE_BYTES {
        return None;
    }
    bincode::deserialize(bytes).ok()
}

pub fn encode_client(msg: &ClientMsg) -> Vec<u8> {
    encode(msg)
}

pub fn encode_host(msg: &HostMsg) -> Vec<u8> {
    encode(msg)
}

pub fn decode_client(bytes: &[u8]) -> Option<ClientMsg> {
    decode(bytes)
}

pub fn decode_host(bytes: &[u8]) -> Option<HostMsg> {
    decode(bytes)
}

pub fn encode_to_relay(msg: &ToRelay) -> Vec<u8> {
    encode(msg)
}

pub fn encode_from_relay(msg: &FromRelay) -> Vec<u8> {
    encode(msg)
}

pub fn decode_to_relay(bytes: &[u8]) -> Option<ToRelay> {
    decode(bytes)
}

pub fn decode_from_relay(bytes: &[u8]) -> Option<FromRelay> {
    decode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sim::{vec3, Vec3};

    fn snapshot(edits_total: u32) -> Snapshot {
        Snapshot {
            tick: 7,
            worms: Vec::new(),
            projectiles: Vec::new(),
            zones: Vec::new(),
            lava: Vec::new(),
            events: Vec::new(),
            edits_total,
            score_limit: 15,
            winner: None,
            choosing: false,
        }
    }

    /// `n` different edits, so a reordered or dropped one would show.
    fn edits(n: usize) -> Vec<(Shape, Op)> {
        (0..n).map(|i| (Shape::Sphere { center: vec3(i as f32, 0.0, 0.0), radius: 1.0 }, Op::Carve)).collect()
    }

    fn over_the_wire(msg: &HostMsg) -> HostMsg {
        decode_host(&encode_host(msg)).expect("host message survives encoding")
    }

    fn same_edits(a: &[(Shape, Op)], b: &[(Shape, Op)]) -> bool {
        // `Shape` has no `PartialEq` (floats), and the bytes are the truth anyway.
        encode(&a.to_vec()) == encode(&b.to_vec())
    }

    #[test]
    fn a_long_edit_log_arrives_in_order_as_several_messages() {
        let log = edits(EDITS_PER_MESSAGE * 2 + 17);
        let msgs = welcome_messages(Welcome { you: 2, map: 0, edits: log.clone(), snapshot: snapshot(log.len() as u32) });
        assert_eq!(msgs.len(), 1 + 3, "a head and three slices");

        let mut builder = WelcomeBuilder::default();
        let mut done = None;
        for msg in &msgs {
            assert!(done.is_none(), "complete only after the last slice");
            done = builder.push(&over_the_wire(msg));
        }
        let welcome = done.expect("the last slice completes the welcome");
        assert_eq!(welcome.you, 2);
        assert_eq!(welcome.snapshot.edits_total, log.len() as u32);
        assert!(same_edits(&welcome.edits, &log));
    }

    #[test]
    fn a_welcome_without_edits_is_complete_with_its_head() {
        let msgs = welcome_messages(Welcome { you: 0, map: 0, edits: Vec::new(), snapshot: snapshot(0) });
        assert_eq!(msgs.len(), 1);
        assert!(WelcomeBuilder::default().push(&msgs[0]).is_some());
    }

    #[test]
    fn a_new_head_restarts_a_half_received_welcome() {
        let log = edits(EDITS_PER_MESSAGE + 1);
        let first = welcome_messages(Welcome { you: 0, map: 0, edits: log.clone(), snapshot: snapshot(0) });
        let second = welcome_messages(Welcome { you: 1, map: 0, edits: log.clone(), snapshot: snapshot(0) });

        let mut builder = WelcomeBuilder::default();
        builder.push(&first[0]);
        builder.push(&first[1]); // ...and then the match restarts before the rest comes
        let mut done = None;
        for msg in &second {
            done = builder.push(msg);
        }
        let welcome = done.expect("the second welcome completes");
        assert_eq!(welcome.you, 1);
        assert_eq!(welcome.edits.len(), log.len(), "none of the first attempt's edits leaked in");
    }

    #[test]
    fn other_messages_do_not_disturb_a_welcome_in_progress() {
        let msgs = welcome_messages(Welcome { you: 0, map: 0, edits: edits(3), snapshot: snapshot(3) });
        let mut builder = WelcomeBuilder::default();
        assert!(builder.push(&HostMsg::Pong { token: 1 }).is_none());
        assert!(builder.push(&msgs[0]).is_none());
        assert!(builder.push(&HostMsg::Snapshot(snapshot(3))).is_none());
        assert!(builder.push(&msgs[1]).is_some());
    }

    /// The slice size is a promise about message size. If `Shape` or `Op`
    /// grows, this fails before a late join silently stops working.
    #[test]
    fn a_full_slice_of_the_biggest_edits_fits_in_a_message() {
        let v = Vec3::ONE;
        let biggest = Shape::Cylinder { a: v, b: v, axis: v, radius: 1.0, half_height: 1.0 };
        let blade = Shape::Blade { a: v, b: v, dir: v, depth: 1.0, half_thickness: 1.0 };
        let slice: Vec<(Shape, Op)> = (0..EDITS_PER_MESSAGE)
            .map(|i| (if i % 2 == 0 { biggest } else { blade }, Op::Smooth(0.5)))
            .collect();
        let bytes = encode_host(&HostMsg::Edits(slice)).len();
        assert!(bytes * 2 < MAX_MESSAGE_BYTES, "a slice is {bytes} bytes");
    }
}
