//! The `Content-Length` framing the Language Server Protocol and the Debug
//! Adapter Protocol share. What a message holds is each protocol's business.
use std::io::{self, BufRead, Write};

/// A server that announces more than this is not one to believe.
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;

pub fn write_message(out: &mut impl Write, body: &[u8]) -> io::Result<()> {
    write!(out, "Content-Length: {}\r\n\r\n", body.len())?;
    out.write_all(body)?;
    out.flush()
}

/// The next message's body, or `None` once the stream has ended between
/// messages. Headers other than `Content-Length` are skipped.
pub fn read_message(input: &mut impl BufRead) -> io::Result<Option<Vec<u8>>> {
    let mut length = None;
    let mut line = String::new();
    let mut any = false;
    loop {
        line.clear();
        if input.read_line(&mut line)? == 0 {
            return if any {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "message header cut short",
                ))
            } else {
                Ok(None)
            };
        }
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
}
