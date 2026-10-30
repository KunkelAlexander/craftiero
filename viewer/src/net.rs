// The wire to the relay. (The in-process wire, `Loopback`, lives in local.rs.)

use std::sync::OnceLock;

use protocol::{decode_from_relay, encode_to_relay, FromRelay, ToRelay};

use crate::socket::{Socket, SocketState};

/// Where the relay lives unless told otherwise. Local by default; a release
/// build for the web bakes in the real one: `RELAY_URL=wss://... cargo build ...`
/// (a page served over https may only open `wss://`).
const DEFAULT_RELAY_URL: &str = match option_env!("RELAY_URL") {
    Some(url) => url,
    None => "ws://127.0.0.1:9001/ws",
};

/// The `--relay` command-line override (native only: a browser has no
/// command line). Set once at startup, before anything connects.
static OVERRIDE: OnceLock<String> = OnceLock::new();

pub fn set_relay_url(url: String) {
    let _ = OVERRIDE.set(url);
}

pub fn relay_url() -> &'static str {
    OVERRIDE.get().map_or(DEFAULT_RELAY_URL, String::as_str)
}

/// Our connection to the relay server (`protocol::relay` explains what the
/// relay does). Same idea as `Loopback`, but the bytes really travel.
pub struct RelayLink {
    socket: Socket,
}

impl RelayLink {
    pub fn connect(url: &str) -> RelayLink {
        RelayLink { socket: Socket::connect(url) }
    }

    pub fn state(&self) -> SocketState {
        self.socket.state()
    }

    pub fn send(&mut self, msg: &ToRelay) {
        self.socket.send(&encode_to_relay(msg));
    }

    pub fn recv(&mut self) -> Option<FromRelay> {
        // Garbage is skipped, never fatal (see `protocol::decode_from_relay`).
        while let Some(bytes) = self.socket.recv() {
            if let Some(msg) = decode_from_relay(&bytes) {
                return Some(msg);
            }
        }
        None
    }
}
