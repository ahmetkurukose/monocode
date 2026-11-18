//! A scripted debug adapter for tests: the other end of a client, driven by
//! a closure that reads requests and writes responses, events and requests
//! of its own, over an in-memory pipe or a local socket.
use super::client::{Client, Incoming};
use crate::code::wire;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub struct PipeWriter(mpsc::Sender<Vec<u8>>);

pub struct PipeReader {
    chunks: mpsc::Receiver<Vec<u8>>,
    current: Vec<u8>,
    position: usize,
}

pub fn pipe() -> (PipeWriter, PipeReader) {
    let (sender, chunks) = mpsc::channel();
    let reader = PipeReader {
        chunks,
        current: Vec::new(),
        position: 0,
    };
    (PipeWriter(sender), reader)
}

impl Write for PipeWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .send(bytes.to_vec())
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::BrokenPipe))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Read for PipeReader {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        while self.position == self.current.len() {
            match self.chunks.recv() {
                Ok(chunk) => {
                    self.current = chunk;
                    self.position = 0;
                }
                // The writer is gone: the end of the stream.
                Err(_) => return Ok(0),
            }
        }
        let count = buffer.len().min(self.current.len() - self.position);
        buffer[..count].copy_from_slice(&self.current[self.position..self.position + count]);
        self.position += count;
        Ok(count)
    }
}

/// The adapter's side of a connection.
pub struct Peer {
    reader: Box<dyn BufRead + Send>,
    writer: Box<dyn Write + Send>,
    seq: i64,
}

impl Peer {
    /// The next message from the client, or `None` once it hung up.
    pub fn next(&mut self) -> Option<Value> {
        let body = wire::read_message(&mut self.reader).ok()??;
        serde_json::from_slice(&body).ok()
    }

    pub fn expect(&mut self, command: &str) -> Value {
        let message = self.next().expect("the client hung up");
        assert_eq!(message["command"], json!(command), "{message}");
        message
    }

    /// Sends any message, numbered; its `seq` is returned.
    pub fn write(&mut self, mut message: Value) -> i64 {
        self.seq += 1;
        message["seq"] = json!(self.seq);
        let body = serde_json::to_vec(&message).unwrap();
        // A client that is gone is the script's cue to end, on its next read.
        wire::write_message(&mut self.writer, &body).ok();
        self.seq
    }

    pub fn respond(&mut self, request: &Value, body: Value) {
        self.write(json!({
            "type": "response", "request_seq": request["seq"], "success": true,
            "command": request["command"], "body": body,
        }));
    }

    pub fn refuse(&mut self, request: &Value, message: &str) {
        self.write(json!({
            "type": "response", "request_seq": request["seq"], "success": false,
            "command": request["command"], "message": message,
        }));
    }

    pub fn event(&mut self, event: &str, body: Value) {
        self.write(json!({ "type": "event", "event": event, "body": body }));
    }

    /// A request to the client; its `seq` is returned.
    pub fn request(&mut self, command: &str, arguments: Value) -> i64 {
        self.write(json!({ "type": "request", "command": command, "arguments": arguments }))
    }
}

/// A client connected to `script` over a pipe. Join the handle at the end
/// of the test so that a failed expectation in the script fails the test.
pub fn fake(
    script: impl FnOnce(&mut Peer) + Send + 'static,
) -> (Client, mpsc::Receiver<Incoming>, JoinHandle<()>) {
    fake_with_process(script, None)
}

/// `fake`, with `process` standing in for the adapter's own process: the
/// client owns it and ends it as it would a real adapter.
pub fn fake_with_process(
    script: impl FnOnce(&mut Peer) + Send + 'static,
    process: Option<std::process::Child>,
) -> (Client, mpsc::Receiver<Incoming>, JoinHandle<()>) {
    let (to_adapter, from_client) = pipe();
    let (to_client, from_adapter) = pipe();
    let handle = std::thread::spawn(move || {
        let mut peer = Peer {
            reader: Box::new(BufReader::new(from_client)),
            writer: Box::new(to_client),
            seq: 0,
        };
        script(&mut peer);
    });
    let (client, incoming) = Client::over(from_adapter, to_adapter, process);
    (client, incoming, handle)
}

/// `script` behind a local port, serving the one client that connects.
pub fn fake_listening(script: impl FnOnce(&mut Peer) + Send + 'static) -> (u16, JoinHandle<()>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        let (socket, _) = listener.accept().unwrap();
        let mut peer = Peer {
            reader: Box::new(BufReader::new(socket.try_clone().unwrap())),
            writer: Box::new(socket),
            seq: 0,
        };
        script(&mut peer);
    });
    (port, handle)
}

/// Polls until `done` holds; panics with `what` after five seconds.
pub fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}
