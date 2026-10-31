// Matches the server hosts itself.
//
// With `CreateRoom` the HOST is a player's computer and the relay sits between
// it and its friends (two network legs for every friend, and the match freezes
// if the host's browser tab goes to the background). With `CreateHostedRoom`
// the server is the host: it runs the same `Host` and `Session` the client
// would, and every player, including the one who asked, is an ordinary joiner
// (one leg, and nobody's tab matters).
//
// The trick that keeps this small: the relay's `Rooms` already knows how to
// give a host a room, and a host is just a connection that has a queue of bytes
// to send and one to receive. So the match task is made a *virtual* host
// connection, with in-process channels where a socket would be, and the
// existing relay logic does the rest.
//
//   joiner's socket ──> Rooms ──> match task's inbox ──> Session ──> Host::handle
//   joiner's socket <── Rooms <── Session::after_tick <── Host::tick (60 Hz)
//
// "Host is a role": nothing in `host` or `sim` changed for this. The game code
// that runs here is the very code that runs inside the player's viewer.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use host::{Host, Session};
use protocol::{decode_from_relay, encode_from_relay, FromRelay, Reject, ToRelay};
use sdf::Grid;
use sim::{map, DT};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};
use tokio::time::MissedTickBehavior;

use crate::rooms::{Role, Rooms, Tx};

/// How many matches one server runs at once. Each holds its own copy of the
/// terrain (tens of megabytes) and a share of the CPU: the free tier of a
/// hosting service is small, so keep this low until it has been measured.
pub const MAX_MATCHES: usize = 4;

type Shared = Arc<Mutex<Rooms>>;

/// The untouched map, built once (the first time a match asks for it) and
/// copied for each match: generating one takes about a second of CPU.
fn pristine_map(map: u8) -> Grid {
    static MAPS: [OnceLock<Grid>; map::MAPS.len()] = [OnceLock::new(), OnceLock::new(), OnceLock::new()];
    MAPS[map as usize].get_or_init(|| map::build_map(map)).clone()
}

/// A connection asked for a hosted room: open it, start the match and put the
/// asker in it.
pub fn start(rooms: &Shared, role: &mut Role, tx: &Tx, version: u16, humans: u8, npcs: u8, map: u8) {
    let mut guard = rooms.lock().unwrap();
    if !matches!(role, Role::Lobby) {
        return reply(tx, FromRelay::Rejected(Reject::AlreadyInRoom));
    }
    if guard.hosted_matches >= MAX_MATCHES {
        return reply(tx, FromRelay::Rejected(Reject::ServerBusy));
    }

    // The match's own end of the relay: a fake connection whose "socket" is a
    // pair of channels.
    let (host_tx, mut host_rx) = unbounded_channel();
    let mut host_role = Role::Lobby;
    guard.handle(&mut host_role, &host_tx, ToRelay::CreateRoom { version });
    let Some(FromRelay::RoomCreated { code }) = next_message(&mut host_rx) else {
        unreachable!("a fresh Rooms always makes a room")
    };
    guard.hosted_matches += 1;

    // The asker: told the code like a host would be, then simply joins.
    reply(tx, FromRelay::RoomCreated { code: code.clone() });
    guard.handle(role, tx, ToRelay::JoinRoom { code, version });

    tokio::spawn(run(rooms.clone(), host_role, host_tx, host_rx, humans, npcs, map));
}

fn reply(tx: &Tx, msg: FromRelay) {
    let _ = tx.send(encode_from_relay(&msg));
}

fn next_message(rx: &mut UnboundedReceiver<Vec<u8>>) -> Option<FromRelay> {
    decode_from_relay(&rx.try_recv().ok()?)
}

/// The match: one task that owns a `Host` and ticks it 60 times a second until
/// everybody has left.
async fn run(rooms: Shared, mut role: Role, tx: Tx, mut inbox: UnboundedReceiver<Vec<u8>>, humans: u8, npcs: u8, map: u8) {
    // The map is a second of CPU: keep it off the threads that serve sockets.
    // The map number comes from a client too: an unknown one is the valley.
    let map = if (map as usize) < map::MAPS.len() { map } else { 0 };
    let grid = tokio::task::spawn_blocking(move || pristine_map(map)).await.expect("map generation panicked");
    // The seat counts come from a client, so they are clamped: at least one
    // human (the asker), at most `MAX_PLAYERS` worms in all (`Host::new` caps
    // the total itself).
    let humans = (humans as usize).max(1);
    let mut host = Host::new(grid, map, humans, npcs as usize, rand::random());
    let mut session = Session::new();

    let mut clock = tokio::time::interval(Duration::from_secs_f32(DT));
    // If a tick runs late (a busy server), do not rush to catch up: bursts of
    // ticks would make it later still. The match slows down instead.
    clock.set_missed_tick_behavior(MissedTickBehavior::Delay);

    // Somebody must have arrived before "nobody is here" can end the match
    // (the asker's arrival is already queued in the inbox).
    let mut anyone_came = false;
    loop {
        clock.tick().await;

        let mut out: Vec<ToRelay> = Vec::new();
        while let Ok(bytes) = inbox.try_recv() {
            let Some(msg) = decode_from_relay(&bytes) else { continue };
            anyone_came |= matches!(msg, FromRelay::PeerJoined(_));
            out.extend(session.on_relay(&mut host, msg));
        }

        let snapshot = host.tick();
        out.extend(session.after_tick(&mut host, snapshot));

        if anyone_came && session.is_empty() {
            break; // the last player left: the match is over
        }

        // One lock per tick, for plain function calls only (never held across
        // an await), like the connection tasks do.
        let mut guard = rooms.lock().unwrap();
        for msg in out {
            guard.handle(&mut role, &tx, msg);
        }
    }

    let mut guard = rooms.lock().unwrap();
    guard.leave(&role);
    guard.hosted_matches -= 1;
}
