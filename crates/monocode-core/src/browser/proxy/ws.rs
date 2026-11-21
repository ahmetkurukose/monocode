//! One WebSocket client of the endpoint, on a thread of its own and one more
//! that reads its socket. Its messages go to the main thread as jobs; what
//! the main thread sends back comes in on the same channel as the socket's
//! bytes, so neither side polls.

use std::{
    io::{self, Read, Write},
    net::TcpStream,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
};

use base64::Engine as _;
use futures_util::FutureExt as _;
use serde_json::{Value, json};
use sha1::{Digest as _, Sha1};
use tungstenite::{
    Message, WebSocket,
    protocol::{Role, WebSocketConfig},
};

use super::{Grant, Input, MAX_MESSAGE, Server, hub};

static NEXT_CLIENT: AtomicU64 = AtomicU64::new(1);

/// RFC 6455 §4.2.2: the answer to a client's `Sec-WebSocket-Key`.
pub(super) fn accept_key(key: &str) -> String {
    let mut sha = Sha1::new();
    sha.update(key.as_bytes());
    sha.update(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
    base64::engine::general_purpose::STANDARD.encode(sha.finalize())
}

/// The socket as tungstenite sees it: reads come from what the reader thread
/// fed in (`WouldBlock` when there is nothing yet), writes go straight out.
struct Pipe {
    inbox: Vec<u8>,
    read: usize,
    socket: TcpStream,
}

impl Pipe {
    fn feed(&mut self, bytes: &[u8]) {
        if self.read == self.inbox.len() {
            self.inbox.clear();
            self.read = 0;
        }
        self.inbox.extend_from_slice(bytes);
    }
}

impl Read for Pipe {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let waiting = &self.inbox[self.read..];
        if waiting.is_empty() {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let n = waiting.len().min(buf.len());
        buf[..n].copy_from_slice(&waiting[..n]);
        self.read += n;
        Ok(n)
    }
}

impl Write for Pipe {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.socket.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.socket.flush()
    }
}

/// Accepts the upgrade and serves the client until either side closes.
pub(super) fn run(
    server: Arc<Server>,
    mut stream: TcpStream,
    key: &str,
    rest: Vec<u8>,
    grant: Grant,
    kind: hub::Kind,
) {
    let response = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
        accept_key(key)
    );
    if stream.write_all(response.as_bytes()).is_err() {
        return;
    }
    let _ = stream.set_read_timeout(None);
    let _ = stream.set_nodelay(true);
    let (Ok(mut reader), Ok(closer)) = (stream.try_clone(), stream.try_clone()) else {
        return;
    };
    let id = NEXT_CLIENT.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = mpsc::channel();

    let to_thread = tx.clone();
    let reading = thread::Builder::new()
        .name("cdp-endpoint-read".into())
        .spawn(move || {
            let mut chunk = vec![0u8; 64 * 1024];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if to_thread.send(Input::Bytes(chunk[..n].to_vec())).is_err() {
                            return;
                        }
                    }
                }
            }
            let _ = to_thread.send(Input::Eof);
        });
    if reading.is_err() {
        return;
    }
    server.add_client(id, closer, grant.internal);

    let (scope, context, browser_id, out) = (
        grant.scope,
        grant.context,
        server.browser_id.clone(),
        tx.clone(),
    );
    let opened =
        server.on_main(move |host| hub::open(host, id, scope, context, browser_id, kind, out));

    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_MESSAGE))
        .max_frame_size(Some(MAX_MESSAGE));
    let pipe = Pipe {
        inbox: rest,
        read: 0,
        socket: stream,
    };
    let mut socket = WebSocket::from_raw_socket(pipe, Role::Server, Some(config));
    if opened && take_messages(&mut socket, &server, id, &tx) {
        while let Ok(input) = rx.recv() {
            match input {
                Input::Bytes(bytes) => {
                    socket.get_mut().feed(&bytes);
                    if !take_messages(&mut socket, &server, id, &tx) {
                        break;
                    }
                }
                Input::Eof => break,
                Input::Send(message) => {
                    if socket.send(Message::text(message.to_string())).is_err() {
                        break;
                    }
                }
                Input::Close => {
                    let _ = socket.close(None);
                    let _ = socket.flush();
                    break;
                }
            }
        }
    }
    server.remove_client(id);
    server.on_main(move |_| {
        hub::close(id);
        async {}.boxed_local()
    });
}

/// Reads every whole message fed in so far; false once the client is gone.
fn take_messages(
    socket: &mut WebSocket<Pipe>,
    server: &Server,
    id: u64,
    tx: &mpsc::Sender<Input>,
) -> bool {
    loop {
        let text = match socket.read() {
            Ok(Message::Text(text)) => text.as_str().to_owned(),
            Ok(Message::Binary(bytes)) => match String::from_utf8(bytes.to_vec()) {
                Ok(text) => text,
                Err(_) => continue,
            },
            Ok(Message::Close(_)) => {
                let _ = socket.flush();
                return false;
            }
            Ok(_) => continue,
            Err(tungstenite::Error::Io(e)) if e.kind() == io::ErrorKind::WouldBlock => {
                return true;
            }
            Err(_) => return false,
        };
        match command(&text) {
            Ok(message) => {
                server.on_main(move |_| hub::message(id, message));
            }
            Err(error) => {
                let _ = tx.send(Input::Send(error));
            }
        }
    }
}

/// A client's message if it is a command, else Chromium's error answer.
fn command(text: &str) -> Result<Value, Value> {
    let Ok(message) = serde_json::from_str::<Value>(text) else {
        return Err(
            json!({"error": {"code": -32700, "message": "Message must be in JSON format"}}),
        );
    };
    let Some(id) = message
        .get("id")
        .filter(|id| id.is_i64() || id.is_u64())
        .cloned()
    else {
        return Err(json!({
            "error": {"code": -32600, "message": "Message must have integer 'id' property"},
        }));
    };
    if !message.get("method").is_some_and(Value::is_string) {
        return Err(json!({
            "id": id,
            "error": {"code": -32600, "message": "Message must have string 'method' property"},
        }));
    }
    if message.get("sessionId").is_some_and(|s| !s.is_string()) {
        return Err(json!({
            "id": id,
            "error": {"code": -32600, "message": "Message has property other than 'id', 'method', 'sessionId', 'params'"},
        }));
    }
    Ok(message)
}
