// The host's side of an online game: open a room on the relay and let friends
// into the match that is already running.
//
//   friend's game ──> relay ──> Share ──> Host::handle(slot, msg)
//   friend's game <── relay <── Share <── Host::tick() snapshot
//
// `Share` is `host::Session` (who is in the match, what to send them) plus a
// connection to the relay (room code, keep-alive, what to do when it drops).
// The session is pure and shared with the server; this file is the part that
// needs a socket and a clock.

use host::{Host, Session};
use protocol::{FromRelay, Reject, Snapshot, ToRelay, PROTOCOL_VERSION};

use crate::net::{relay_url, RelayLink};
use crate::socket::SocketState;

/// Ping the relay this often (in host ticks: 20 s). A host alone in its room
/// would otherwise send nothing at all, and an idle WebSocket is cut by the
/// proxies of hosting services, which also let a free server fall asleep after
/// 15 minutes without incoming messages. A tiny ping is the cheapest "I am
/// still here" there is.
const KEEPALIVE_TICKS: u32 = 20 * 60;

pub struct Share {
    link: RelayLink,
    session: Session,
    ticks_since_ping: u32,
    /// Have we asked the relay for a room yet? (Only possible once it is open.)
    asked: bool,
    code: Option<String>,
    problem: Option<String>,
}

impl Share {
    /// Start connecting to the relay. The game does not wait for it: the host
    /// keeps playing while the connection (and a sleeping free server) wakes up.
    pub fn open() -> Share {
        Share {
            link: RelayLink::connect(relay_url()),
            session: Session::new(),
            ticks_since_ping: 0,
            asked: false,
            code: None,
            problem: None,
        }
    }

    /// The code friends type, once the relay has made the room.
    pub fn code(&self) -> Option<&str> {
        self.code.as_deref()
    }

    /// How many friends are in the match.
    pub fn friends(&self) -> usize {
        self.session.friends()
    }

    /// What went wrong, if the room is not working. Shown instead of the code.
    pub fn problem(&self) -> Option<&str> {
        self.problem.as_deref()
    }

    /// Take in everything the relay has delivered: friends arriving, leaving,
    /// and their messages. Called before each host tick.
    pub fn receive(&mut self, host: &mut Host) {
        match self.link.state() {
            SocketState::Connecting => return,
            SocketState::Closed => {
                // The relay is gone, so every friend is too.
                self.session.drop_everyone(host);
                self.code = None;
                if self.problem.is_none() {
                    self.problem = Some("LOST THE RELAY - PLAYING OFFLINE".into());
                }
                return;
            }
            SocketState::Open => {}
        }

        if !self.asked {
            self.asked = true;
            self.link.send(&ToRelay::CreateRoom { version: PROTOCOL_VERSION });
        }

        self.ticks_since_ping += 1;
        if self.ticks_since_ping >= KEEPALIVE_TICKS {
            self.ticks_since_ping = 0;
            self.link.send(&ToRelay::Ping { token: 0 });
        }

        while let Some(msg) = self.link.recv() {
            match msg {
                FromRelay::RoomCreated { code } => {
                    // Also on the terminal, for whoever started the game from one.
                    println!("room open: friends join with the code {code} (--join {code})");
                    self.code = Some(code);
                }
                FromRelay::Rejected(why) => self.problem = Some(rejection(why).into()),
                msg => {
                    for reply in self.session.on_relay(host, msg) {
                        self.link.send(&reply);
                    }
                }
            }
        }
    }

    /// Hand over this tick's snapshot: the session sends friends every second
    /// one, and any Welcomes that were waiting for a good moment.
    pub fn broadcast(&mut self, host: &mut Host, snapshot: Snapshot) {
        for msg in self.session.after_tick(host, snapshot) {
            self.link.send(&msg);
        }
    }
}

fn rejection(why: Reject) -> &'static str {
    match why {
        Reject::ServerBusy => "THE SERVER IS BUSY - TRY AGAIN IN A MINUTE",
        Reject::AlreadyInRoom => "ALREADY IN A ROOM",
        // Only a joiner can be told these; a host never sees them.
        Reject::NoSuchRoom | Reject::RoomFull | Reject::VersionMismatch => "THE RELAY REFUSED THE ROOM",
    }
}
