// The relay server: a thin network shell around `rooms::Rooms`.
//
// All it does is turn WebSocket frames into `ToRelay` messages, hand them to
// `Rooms`, and write whatever `Rooms` queued back out. It runs no game logic.
//
// Why a WebSocket and not raw TCP/UDP: it is the only connection a browser is
// allowed to open to a server it can reach, and it works through the proxies
// of free hosting. It is TCP underneath, so frames arrive reliably and in
// order.

pub mod matches;
pub mod rooms;

use std::sync::{Arc, Mutex};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use axum::routing::get;
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use protocol::{decode_to_relay, ToRelay};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

use rooms::{Role, Rooms};

/// One `Rooms` shared by every connection. It is only ever locked for the
/// duration of a plain function call (never across an `await`), so a simple
/// std Mutex is enough.
type Shared = Arc<Mutex<Rooms>>;

pub async fn serve(listener: TcpListener) {
    let app = Router::new()
        // A plain page so a browser or a hosting health check can see we are up.
        .route("/", get(|| async { "craftiero relay" }))
        .route("/ws", get(upgrade))
        .with_state(Shared::default());
    axum::serve(listener, app).await.expect("relay server crashed");
}

async fn upgrade(ws: WebSocketUpgrade, State(rooms): State<Shared>) -> Response {
    // Our messages are tiny; a huge frame can only be abuse.
    ws.max_message_size(128 * 1024).on_upgrade(move |socket| connection(socket, rooms))
}

/// One task per connected client.
async fn connection(socket: WebSocket, rooms: Shared) {
    let (mut sink, mut stream) = socket.split();

    // Whatever `Rooms` wants to tell this client is queued here, and a second
    // task writes it out. That way a slow reader of one client never blocks
    // the logic that serves the others.
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let writer = tokio::spawn(async move {
        while let Some(bytes) = rx.recv().await {
            if sink.send(Message::Binary(bytes)).await.is_err() {
                break;
            }
        }
    });

    let mut role = Role::Lobby;
    while let Some(Ok(frame)) = stream.next().await {
        // We only speak binary. Text, ping and pong frames are ignored (the
        // WebSocket layer answers protocol pings by itself).
        let Message::Binary(bytes) = frame else { continue };
        // Garbage from a client is dropped, never fatal.
        let Some(msg) = decode_to_relay(&bytes) else { continue };
        // A hosted room needs a task to run the match, which `Rooms` (plain
        // synchronous code) cannot start, so it is answered here.
        if let ToRelay::CreateHostedRoom { version } = msg {
            matches::start(&rooms, &mut role, &tx, version);
            continue;
        }
        rooms.lock().unwrap().handle(&mut role, &tx, msg);
    }

    // The stream ended: the client closed the tab or lost its connection.
    rooms.lock().unwrap().leave(&role);
    writer.abort();
}
