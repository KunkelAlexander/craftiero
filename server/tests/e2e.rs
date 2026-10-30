// End-to-end: a real relay on a real port, two real WebSocket clients.
// The unit tests in rooms.rs cover the logic; this proves the network shell
// around it (frames in, frames out, disconnect handling) works too.

use std::net::TcpStream;
use std::time::Duration;

use protocol::{decode_from_relay, decode_host, encode_to_relay, FromRelay, HostMsg, Reject, ToRelay, PROTOCOL_VERSION};
use tungstenite::{connect, stream::MaybeTlsStream, Message, WebSocket};

type Client = WebSocket<MaybeTlsStream<TcpStream>>;

fn start_relay() -> u16 {
    let std_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = std_listener.local_addr().unwrap().port();
    std_listener.set_nonblocking(true).unwrap();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let listener = tokio::net::TcpListener::from_std(std_listener).unwrap();
            server::serve(listener).await;
        });
    });
    port
}

fn client(port: u16) -> Client {
    let (ws, _) = connect(format!("ws://127.0.0.1:{port}/ws")).expect("connect to relay");
    if let MaybeTlsStream::Plain(s) = ws.get_ref() {
        // Never hang the test forever if something is wrong.
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    }
    ws
}

fn say(ws: &mut Client, msg: ToRelay) {
    ws.send(Message::Binary(encode_to_relay(&msg))).unwrap();
}

fn hear(ws: &mut Client) -> FromRelay {
    loop {
        if let Message::Binary(bytes) = ws.read().expect("a message from the relay") {
            return decode_from_relay(&bytes).unwrap();
        }
    }
}

#[test]
fn two_clients_talk_through_a_real_relay() {
    let port = start_relay();
    let (mut host, mut friend) = (client(port), client(port));

    say(&mut host, ToRelay::CreateRoom { version: PROTOCOL_VERSION });
    let FromRelay::RoomCreated { code } = hear(&mut host) else { panic!("no room") };

    // A stale build is refused, and may retry on the same connection.
    say(&mut friend, ToRelay::JoinRoom { code: code.clone(), version: PROTOCOL_VERSION + 1 });
    assert!(matches!(hear(&mut friend), FromRelay::Rejected(Reject::VersionMismatch)));

    say(&mut friend, ToRelay::JoinRoom { code, version: PROTOCOL_VERSION });
    assert!(matches!(hear(&mut friend), FromRelay::RoomJoined));
    assert!(matches!(hear(&mut host), FromRelay::PeerJoined(1)));

    say(&mut friend, ToRelay::Data { to: None, bytes: b"hello host".to_vec() });
    match hear(&mut host) {
        FromRelay::Data { from: 1, bytes } => assert_eq!(bytes, b"hello host"),
        other => panic!("unexpected {other:?}"),
    }

    say(&mut host, ToRelay::Data { to: Some(1), bytes: b"hello friend".to_vec() });
    match hear(&mut friend) {
        FromRelay::Data { from: 0, bytes } => assert_eq!(bytes, b"hello friend"),
        other => panic!("unexpected {other:?}"),
    }

    // Closing the host's connection must close the room for the friend.
    drop(host);
    assert!(matches!(hear(&mut friend), FromRelay::RoomClosed));
}

// ---- matches the server hosts itself ----------------------------------------

/// The next game message the match sent, skipping the relay's own chatter.
fn hear_host(ws: &mut Client) -> HostMsg {
    loop {
        if let FromRelay::Data { from: 0, bytes } = hear(ws) {
            return decode_host(&bytes).expect("the match speaks protocol");
        }
    }
}

/// Like `client`, but patient: the match builds the whole map before it sends
/// anything, which takes seconds in a debug build.
fn patient_client(port: u16) -> Client {
    let ws = client(port);
    if let MaybeTlsStream::Plain(s) = ws.get_ref() {
        s.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
    }
    ws
}

fn open_hosted_room(ws: &mut Client) -> String {
    say(ws, ToRelay::CreateHostedRoom { version: PROTOCOL_VERSION });
    let FromRelay::RoomCreated { code } = hear(ws) else { panic!("no room") };
    assert!(matches!(hear(ws), FromRelay::RoomJoined), "the asker is put in the room");
    code
}

#[test]
fn a_hosted_match_welcomes_players_and_streams_snapshots() {
    let port = start_relay();
    let (mut a, mut b) = (patient_client(port), patient_client(port));

    let code = open_hosted_room(&mut a);
    // The server's own match is the host: it welcomes the asker, in pieces,
    // and then keeps sending snapshots.
    let HostMsg::Welcome(head) = hear_host(&mut a) else { panic!("the first thing sent is the Welcome") };
    assert_eq!(head.you, 0);
    let mut snapshots = 0;
    while snapshots < 5 {
        if matches!(hear_host(&mut a), HostMsg::Snapshot(_)) {
            snapshots += 1;
        }
    }

    // A friend joins by the same code and takes the next seat.
    say(&mut b, ToRelay::JoinRoom { code, version: PROTOCOL_VERSION });
    assert!(matches!(hear(&mut b), FromRelay::RoomJoined));
    // Snapshots are broadcast to everyone in the room, so the friend hears
    // some before their own Welcome arrives; a client ignores those.
    let head = loop {
        match hear_host(&mut b) {
            HostMsg::Welcome(head) => break head,
            HostMsg::Snapshot(_) => {}
            other => panic!("unexpected {other:?} before the Welcome"),
        }
    };
    assert_eq!(head.you, 1);
}

#[test]
fn the_server_refuses_more_matches_than_it_can_carry_and_frees_the_room_when_one_ends() {
    let port = start_relay();
    let mut players: Vec<Client> = (0..server::matches::MAX_MATCHES).map(|_| patient_client(port)).collect();
    for p in &mut players {
        open_hosted_room(p);
    }

    let mut extra = patient_client(port);
    say(&mut extra, ToRelay::CreateHostedRoom { version: PROTOCOL_VERSION });
    assert!(matches!(hear(&mut extra), FromRelay::Rejected(Reject::ServerBusy)));

    // The only player of one match leaves: the match ends by itself, and soon
    // there is room again. (A match cannot notice anything until its map is
    // built, and in a debug build that takes tens of seconds: hence the patience.
    // `cargo test --release -p server` is quick.)
    drop(players.pop());
    let mut got_room = false;
    for _ in 0..1200 {
        say(&mut extra, ToRelay::CreateHostedRoom { version: PROTOCOL_VERSION });
        match hear(&mut extra) {
            FromRelay::RoomCreated { .. } => {
                got_room = true;
                break;
            }
            FromRelay::Rejected(Reject::ServerBusy) => std::thread::sleep(Duration::from_millis(50)),
            other => panic!("unexpected {other:?}"),
        }
    }
    assert!(got_room, "the finished match never gave its place back");
}
