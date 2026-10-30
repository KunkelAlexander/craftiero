// A WebSocket the game loop can poll without ever waiting.
//
// The loop must not block on the network (a frame is 16 ms, and a free server
// that is waking up takes a minute to answer), so the API is four calls:
//
//     Socket::connect(url)   start connecting; returns at once
//     state()                Connecting, Open or Closed
//     send(bytes)            queue a binary message (dropped unless Open)
//     recv()                 the next message that arrived, if any
//
// Anything that goes wrong is just `Closed`: the caller shows a message, there
// is nothing to catch.
//
// Why WebSocket at all, and not UDP: see docs/NETWORKING.md. In short, a
// browser may only open WebSockets, and one code path for native and web beats
// squeezing out the last milliseconds. It is TCP underneath (reliable and
// ordered), which is exactly what the terrain edits need; the price is that one
// lost packet delays everything behind it.
//
// Two implementations, because the platforms have nothing in common: a
// background thread around `tungstenite` natively, and the browser's own
// `WebSocket` on the web.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SocketState {
    Connecting,
    Open,
    Closed,
}

#[cfg(not(target_arch = "wasm32"))]
pub use native::Socket;
#[cfg(target_arch = "wasm32")]
pub use web::Socket;

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::io::ErrorKind;
    use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
    use std::sync::{mpsc, Arc};
    use std::time::Duration;

    use tungstenite::stream::MaybeTlsStream;
    use tungstenite::{Error, Message};

    use super::SocketState;

    const CONNECTING: u8 = 0;
    const OPEN: u8 = 1;
    const CLOSED: u8 = 2;

    /// How long the socket thread waits for incoming data before it looks at
    /// the outgoing queue again. This bounds how long an input can sit unsent,
    /// so keep it well under a tick (16 ms).
    const POLL: Duration = Duration::from_millis(2);

    /// Two threads touch a socket: the game loop and a worker that does the
    /// blocking network calls. They talk through channels and one atomic.
    pub struct Socket {
        outgoing: mpsc::Sender<Vec<u8>>,
        incoming: mpsc::Receiver<Vec<u8>>,
        state: Arc<AtomicU8>,
        /// Set on drop: the worker closes the connection and ends.
        stop: Arc<AtomicBool>,
    }

    impl Socket {
        pub fn connect(url: &str) -> Socket {
            let (outgoing, to_send) = mpsc::channel();
            let (to_game, incoming) = mpsc::channel();
            let state = Arc::new(AtomicU8::new(CONNECTING));
            let stop = Arc::new(AtomicBool::new(false));

            let (url, worker_state, worker_stop) = (url.to_string(), state.clone(), stop.clone());
            std::thread::spawn(move || {
                run(&url, &to_send, &to_game, &worker_state, &worker_stop);
                // However `run` ended (refused, closed, error), the game sees `Closed`.
                worker_state.store(CLOSED, Ordering::SeqCst);
            });
            Socket { outgoing, incoming, state, stop }
        }

        pub fn state(&self) -> SocketState {
            match self.state.load(Ordering::SeqCst) {
                CONNECTING => SocketState::Connecting,
                OPEN => SocketState::Open,
                _ => SocketState::Closed,
            }
        }

        pub fn send(&mut self, bytes: &[u8]) {
            if self.state() == SocketState::Open {
                let _ = self.outgoing.send(bytes.to_vec());
            }
        }

        pub fn recv(&mut self) -> Option<Vec<u8>> {
            self.incoming.try_recv().ok()
        }
    }

    impl Drop for Socket {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
        }
    }

    /// The worker: connect, then alternate between writing what the game
    /// queued and reading what the server sent, until either side is done.
    fn run(
        url: &str,
        to_send: &mpsc::Receiver<Vec<u8>>,
        to_game: &mpsc::Sender<Vec<u8>>,
        state: &AtomicU8,
        stop: &AtomicBool,
    ) {
        // Blocks through DNS, TCP, TLS (for wss://) and the upgrade request.
        let Ok((mut ws, _)) = tungstenite::connect(url) else { return };

        // Networking concept: Nagle's algorithm. TCP likes to hold a small
        // write back until the previous one is acknowledged, so it can send
        // them together. For a file that saves packets; for a game that sends
        // a 50-byte input 60 times a second it batches inputs for up to a
        // whole round trip. `nodelay` turns it off. (Browsers already do.)
        let tcp = match ws.get_mut() {
            MaybeTlsStream::Plain(tcp) => Some(tcp),
            MaybeTlsStream::Rustls(tls) => Some(tls.get_mut()),
            _ => None,
        };
        if let Some(tcp) = tcp {
            if tcp.set_nodelay(true).is_err() || tcp.set_read_timeout(Some(POLL)).is_err() {
                return;
            }
        }
        state.store(OPEN, Ordering::SeqCst);

        while !stop.load(Ordering::SeqCst) {
            while let Ok(bytes) = to_send.try_recv() {
                if ws.send(Message::Binary(bytes)).is_err() {
                    return;
                }
            }
            match ws.read() {
                Ok(Message::Binary(bytes)) => {
                    if to_game.send(bytes).is_err() {
                        return; // the game dropped its end
                    }
                }
                Ok(Message::Close(_)) => return,
                // Text, ping and pong: tungstenite answers pings itself.
                Ok(_) => {}
                // Nothing arrived within `POLL`: not an error, go round again.
                Err(Error::Io(e)) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                Err(_) => return,
            }
        }
        let _ = ws.close(None);
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::net::TcpListener;
        use std::time::Instant;

        /// Poll like the game loop does, with a deadline so a bug cannot hang the test.
        fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !done() {
                assert!(Instant::now() < deadline, "timed out waiting for {what}");
                std::thread::sleep(Duration::from_millis(2));
            }
        }

        /// A tiny echo server: sends every binary message back with a byte appended.
        fn echo_server() -> u16 {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            std::thread::spawn(move || {
                let (stream, _) = listener.accept().unwrap();
                let mut ws = tungstenite::accept(stream).unwrap();
                while let Ok(msg) = ws.read() {
                    if let Message::Binary(mut bytes) = msg {
                        bytes.push(0xEE);
                        if ws.send(Message::Binary(bytes)).is_err() {
                            break;
                        }
                    }
                }
            });
            port
        }

        #[test]
        fn messages_travel_both_ways_without_blocking() {
            let port = echo_server();
            let mut socket = Socket::connect(&format!("ws://127.0.0.1:{port}/ws"));
            // `connect` returned at once; the connection finishes in the background.
            wait_for("the socket to open", || socket.state() == SocketState::Open);

            socket.send(&[1, 2, 3]);
            socket.send(&[4]);
            let mut heard = Vec::new();
            wait_for("both echoes", || {
                heard.extend(socket.recv());
                heard.len() == 2
            });
            assert_eq!(heard, vec![vec![1, 2, 3, 0xEE], vec![4, 0xEE]], "in order");
        }

        #[test]
        fn a_refused_connection_ends_up_closed() {
            // Bind and drop to get a port nobody is listening on.
            let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
            let mut socket = Socket::connect(&format!("ws://127.0.0.1:{port}/ws"));
            wait_for("the failure", || socket.state() == SocketState::Closed);
            socket.send(&[1]); // harmless
            assert!(socket.recv().is_none());
        }

        #[test]
        fn a_server_that_hangs_up_closes_the_socket() {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            std::thread::spawn(move || {
                let (stream, _) = listener.accept().unwrap();
                drop(tungstenite::accept(stream).unwrap()); // handshake, then hang up
            });
            let socket = Socket::connect(&format!("ws://127.0.0.1:{port}/ws"));
            wait_for("the close to be noticed", || socket.state() == SocketState::Closed);
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod web {
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::rc::Rc;

    use wasm_bindgen::closure::Closure;
    use wasm_bindgen::JsCast;
    use web_sys::{BinaryType, MessageEvent, WebSocket};

    use super::SocketState;

    /// The browser does the networking; we only hand it callbacks. Everything
    /// runs on the one browser thread, so plain `Rc<RefCell>` is enough.
    pub struct Socket {
        ws: Option<WebSocket>,
        state: Rc<Cell<SocketState>>,
        inbox: Rc<RefCell<VecDeque<Vec<u8>>>>,
        // The callbacks live here: dropping a `Closure` frees the function the
        // browser would still call.
        _on_open: Closure<dyn FnMut()>,
        _on_message: Closure<dyn FnMut(MessageEvent)>,
        _on_end: Closure<dyn FnMut()>,
    }

    impl Socket {
        pub fn connect(url: &str) -> Socket {
            let state = Rc::new(Cell::new(SocketState::Connecting));
            let inbox = Rc::new(RefCell::new(VecDeque::new()));

            let open_state = state.clone();
            let on_open = Closure::<dyn FnMut()>::new(move || open_state.set(SocketState::Open));

            let message_inbox = inbox.clone();
            let on_message = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
                // `binary_type` is "arraybuffer" (set below), so text frames
                // (which we never send) are the only thing that is not one.
                if let Ok(buffer) = event.data().dyn_into::<js_sys::ArrayBuffer>() {
                    message_inbox.borrow_mut().push_back(js_sys::Uint8Array::new(&buffer).to_vec());
                }
            });

            // Both an error and a close mean the connection is over (an error
            // is always followed by a close, but do not rely on the order).
            let end_state = state.clone();
            let on_end = Closure::<dyn FnMut()>::new(move || end_state.set(SocketState::Closed));

            // `new` throws for a bad URL or a blocked `ws://` on an https page.
            let ws = WebSocket::new(url).ok();
            match &ws {
                Some(ws) => {
                    ws.set_binary_type(BinaryType::Arraybuffer);
                    ws.set_onopen(Some(on_open.as_ref().unchecked_ref()));
                    ws.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
                    ws.set_onerror(Some(on_end.as_ref().unchecked_ref()));
                    ws.set_onclose(Some(on_end.as_ref().unchecked_ref()));
                }
                None => state.set(SocketState::Closed),
            }
            Socket { ws, state, inbox, _on_open: on_open, _on_message: on_message, _on_end: on_end }
        }

        pub fn state(&self) -> SocketState {
            self.state.get()
        }

        pub fn send(&mut self, bytes: &[u8]) {
            if let (Some(ws), SocketState::Open) = (&self.ws, self.state.get()) {
                let _ = ws.send_with_u8_array(bytes);
            }
        }

        pub fn recv(&mut self) -> Option<Vec<u8>> {
            self.inbox.borrow_mut().pop_front()
        }
    }

    impl Drop for Socket {
        fn drop(&mut self) {
            if let Some(ws) = &self.ws {
                // Unhook first, so a late event cannot call a freed closure.
                ws.set_onopen(None);
                ws.set_onmessage(None);
                ws.set_onerror(None);
                ws.set_onclose(None);
                let _ = ws.close();
            }
        }
    }
}
