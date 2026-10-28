// The wire to the relay. (The in-process wire, `Loopback`, was ported to
// `viewer/src/local.rs` at M4.)

use protocol::{decode_from_relay, encode_to_relay, FromRelay, ToRelay};

use crate::socket::{Socket, SocketState};

/// Where the relay lives. Local by default; a release build for the web
/// overrides it with `RELAY_URL=wss://... cargo build ...`.
pub const RELAY_URL: &str = match option_env!("RELAY_URL") {
    Some(url) => url,
    None => "ws://127.0.0.1:9001/ws",
};

/// Turns a growing byte total into "bytes per second", refreshed once a second.
#[derive(Default)]
pub struct RateMeter {
    last_time: f64,
    last_total: u64,
    pub per_second: f32,
}

impl RateMeter {
    pub fn update(&mut self, now: f64, total: u64) {
        if now - self.last_time >= 1.0 {
            self.per_second = (total - self.last_total) as f32 / (now - self.last_time) as f32;
            self.last_time = now;
            self.last_total = total;
        }
    }
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
        while let Some(bytes) = self.socket.recv() {
            if let Some(msg) = decode_from_relay(&bytes) {
                return Some(msg);
            }
        }
        None
    }
}
