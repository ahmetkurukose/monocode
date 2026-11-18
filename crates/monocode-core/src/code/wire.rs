//! The `Content-Length` framing the Language Server Protocol and the Debug
//! Adapter Protocol share. What a message holds is each protocol's business.
use std::io::{self, BufRead, Write};

/// A server that announces more than this is not one to believe.
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;

/// The longest header line, and the longest line of a peer's error stream
/// taken at once. A peer that writes on without a line end must not be able
/// to grow a buffer without limit.
pub const MAX_LINE_BYTES: usize = 64 * 1024;

/// Reads the next line into `line` (cleared first), its `\n` included, but
/// never more than `limit` bytes: a longer line is handed over in pieces.
/// Returns how many bytes were read, zero at the end of the stream.
pub fn read_line_capped(
    input: &mut impl BufRead,
    line: &mut Vec<u8>,
    limit: usize,
) -> io::Result<usize> {
    line.clear();
    while line.len() < limit {
        let available = match input.fill_buf() {
            Ok(available) => available,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        if available.is_empty() {
            break;
        }
        let room = limit - line.len();
        let end = available.iter().take(room).position(|byte| *byte == b'\n');
        let taken = end.map_or(available.len().min(room), |end| end + 1);
        line.extend_from_slice(&available[..taken]);
        input.consume(taken);
        if end.is_some() {
            break;
        }
    }
    Ok(line.len())
}

pub fn write_message(out: &mut impl Write, body: &[u8]) -> io::Result<()> {
    write!(out, "Content-Length: {}\r\n\r\n", body.len())?;
    out.write_all(body)?;
    out.flush()
}

/// The next message's body, or `None` once the stream has ended between
/// messages. Headers other than `Content-Length` are skipped.
pub fn read_message(input: &mut impl BufRead) -> io::Result<Option<Vec<u8>>> {
    let mut length = None;
    let mut bytes = Vec::new();
    let mut any = false;
    loop {
        if read_line_capped(input, &mut bytes, MAX_LINE_BYTES)? == 0 {
            return if any {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "message header cut short",
                ))
            } else {
                Ok(None)
            };
        }
        if bytes.last() != Some(&b'\n') && bytes.len() == MAX_LINE_BYTES {
            // No header is this long: the stream is not the protocol's.
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "message header too long",
            ));
        }
        let line = std::str::from_utf8(&bytes)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "unreadable message header"))?;
        let header = line.trim_end_matches(['\r', '\n']);
        if header.is_empty() {
            if any {
                break;
            }
            // Stray blank lines between messages are tolerated.
            continue;
        }
        any = true;
        if let Some((name, value)) = header.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                length = value.trim().parse::<usize>().ok();
            }
        }
    }
    let length = length.ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "message without Content-Length")
    })?;
    if length > MAX_MESSAGE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "message too large",
        ));
    }
    let mut body = vec![0; length];
    input.read_exact(&mut body)?;
    Ok(Some(body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn messages_round_trip() {
        let mut wire = Vec::new();
        write_message(&mut wire, b"{\"a\":1}").unwrap();
        write_message(&mut wire, "{\"ş\":2}".as_bytes()).unwrap();
        let mut input = Cursor::new(wire);
        assert_eq!(read_message(&mut input).unwrap().unwrap(), b"{\"a\":1}");
        assert_eq!(
            read_message(&mut input).unwrap().unwrap(),
            "{\"ş\":2}".as_bytes()
        );
        assert!(read_message(&mut input).unwrap().is_none());
    }

    #[test]
    fn other_headers_are_skipped() {
        let mut input = Cursor::new(
            b"Content-Type: application/vscode-jsonrpc; charset=utf-8\r\ncontent-length: 2\r\n\r\n{}".to_vec(),
        );
        assert_eq!(read_message(&mut input).unwrap().unwrap(), b"{}");
    }

    #[test]
    fn a_cut_message_is_an_error() {
        assert!(read_message(&mut Cursor::new(b"Content-Length: 9\r\n\r\n{}".to_vec())).is_err());
        assert!(read_message(&mut Cursor::new(b"Content-Length: 9\r\n".to_vec())).is_err());
        assert!(read_message(&mut Cursor::new(b"X: 1\r\n\r\n{}".to_vec())).is_err());
    }

    /// A stream that never ends and never sends a line end.
    struct Endless(usize);

    impl io::Read for Endless {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            self.0 += out.len();
            assert!(self.0 < 8 * MAX_LINE_BYTES, "read on without limit");
            out.fill(b'x');
            Ok(out.len())
        }
    }

    #[test]
    fn a_header_without_a_line_end_is_a_broken_stream() {
        let mut input = io::BufReader::new(Endless(0));
        let error = read_message(&mut input).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        // Just under the limit a long header is still only skipped.
        let mut wire = format!("X-Long: {}\r\n", "y".repeat(MAX_LINE_BYTES - 100)).into_bytes();
        wire.extend_from_slice(b"Content-Length: 2\r\n\r\n{}");
        assert_eq!(
            read_message(&mut Cursor::new(wire)).unwrap().unwrap(),
            b"{}"
        );
    }

    #[test]
    fn long_lines_are_read_in_pieces() {
        let mut input = Cursor::new(b"ab\ncdefgh\nij".to_vec());
        let mut line = Vec::new();
        let mut pieces = Vec::new();
        while read_line_capped(&mut input, &mut line, 4).unwrap() > 0 {
            pieces.push(String::from_utf8(line.clone()).unwrap());
        }
        assert_eq!(pieces, ["ab\n", "cdef", "gh\n", "ij"]);
    }
}
