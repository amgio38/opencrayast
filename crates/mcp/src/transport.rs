//! Newline-delimited JSON-RPC framing with a hard per-message byte cap.

use std::io::{self, BufRead, Write};

/// Largest stdin message (bytes before the terminating newline) we will accept.
/// Longer input is refused without buffering the whole line (T-23 / MCP-05).
pub const MAX_MESSAGE_BYTES: usize = 8 * 1024 * 1024;

/// Result of one read attempt from the client.
#[derive(Debug, PartialEq, Eq)]
pub enum ReadOutcome {
    /// A complete line (without the trailing `\n`), within the size cap.
    Message(Vec<u8>),
    /// The line exceeded [`MAX_MESSAGE_BYTES`]; remaining bytes through newline were drained.
    TooLarge,
    /// stdin reached EOF with no pending bytes.
    Eof,
    /// stdin reached EOF with bytes buffered but **no** terminating newline.
    ///
    /// Treating that remainder as a complete message would accept a half-frame
    /// when a client dies mid-write. We refuse it instead (CR R2 ⑦).
    Incomplete,
}

/// Read one newline-delimited message from `input`.
///
/// When the line is over the cap, bytes after the limit are discarded until `\n`
/// or EOF so the next message can still be framed. Never buffers an unbounded line.
/// A non-empty buffer at EOF without a newline is [`ReadOutcome::Incomplete`].
pub fn read_message<R: BufRead>(input: &mut R) -> io::Result<ReadOutcome> {
    let mut buf: Vec<u8> = Vec::new();
    let mut too_large = false;
    loop {
        let (done, consumed) = {
            let data = input.fill_buf()?;
            if data.is_empty() {
                if buf.is_empty() && !too_large {
                    return Ok(ReadOutcome::Eof);
                }
                return Ok(if too_large {
                    ReadOutcome::TooLarge
                } else {
                    ReadOutcome::Incomplete
                });
            }
            let mut consumed = 0usize;
            let mut done = false;
            for &b in data {
                consumed += 1;
                if b == b'\n' {
                    done = true;
                    break;
                }
                if too_large {
                    continue;
                }
                if buf.len() >= MAX_MESSAGE_BYTES {
                    too_large = true;
                    buf.clear();
                    buf.shrink_to_fit();
                    continue;
                }
                buf.push(b);
            }
            (done, consumed)
        };
        input.consume(consumed);
        if done {
            return Ok(if too_large {
                ReadOutcome::TooLarge
            } else {
                ReadOutcome::Message(buf)
            });
        }
    }
}

/// Outcome of writing one protocol line.
#[derive(Debug, PartialEq, Eq)]
pub enum WriteOutcome {
    /// Line fully written (and flushed).
    Written,
    /// Peer closed the pipe (`EPIPE` / `BrokenPipe`). Not an error: stop cleanly.
    Closed,
}

/// Write one JSON-RPC message as a single line on `out`.
///
/// A closed stdout becomes [`WriteOutcome::Closed`] (no panic, no `Err`), so the
/// serve loop can exit 0 instead of spinning while stdin stays open (failure table).
pub fn write_message<W: Write>(out: &mut W, line: &str) -> io::Result<WriteOutcome> {
    match out.write_all(line.as_bytes()) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => return Ok(WriteOutcome::Closed),
        Err(e) => return Err(e),
    }
    match out.write_all(b"\n") {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => return Ok(WriteOutcome::Closed),
        Err(e) => return Err(e),
    }
    match out.flush() {
        Ok(()) => Ok(WriteOutcome::Written),
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Ok(WriteOutcome::Closed),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn reads_a_line() {
        let mut c = Cursor::new(b"{\"a\":1}\n");
        assert_eq!(
            read_message(&mut c).unwrap(),
            ReadOutcome::Message(b"{\"a\":1}".to_vec())
        );
        assert_eq!(read_message(&mut c).unwrap(), ReadOutcome::Eof);
    }

    #[test]
    fn oversize_is_refused_without_keeping_the_body() {
        let mut huge = vec![b'x'; MAX_MESSAGE_BYTES + 10];
        huge.push(b'\n');
        let mut c = Cursor::new(huge);
        assert_eq!(read_message(&mut c).unwrap(), ReadOutcome::TooLarge);
    }

    /// Off-by-one: exactly `MAX_MESSAGE_BYTES` (no newline) is accepted.
    #[test]
    fn exact_cap_is_accepted() {
        let mut body = vec![b'a'; MAX_MESSAGE_BYTES];
        body.push(b'\n');
        let mut c = Cursor::new(body);
        match read_message(&mut c).unwrap() {
            ReadOutcome::Message(m) => assert_eq!(m.len(), MAX_MESSAGE_BYTES),
            other => panic!("expected Message, got {other:?}"),
        }
    }

    /// Off-by-one: one byte over the cap is `TooLarge`, and the next line still frames.
    #[test]
    fn one_over_cap_then_next_message() {
        let mut body = vec![b'b'; MAX_MESSAGE_BYTES + 1];
        body.push(b'\n');
        body.extend_from_slice(br#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#);
        body.push(b'\n');
        let mut c = Cursor::new(body);
        assert_eq!(read_message(&mut c).unwrap(), ReadOutcome::TooLarge);
        match read_message(&mut c).unwrap() {
            ReadOutcome::Message(m) => {
                assert!(
                    m.starts_with(br#"{"jsonrpc""#),
                    "{}",
                    String::from_utf8_lossy(&m)
                );
            }
            other => panic!("expected following Message, got {other:?}"),
        }
    }

    #[test]
    fn incomplete_line_at_eof_is_not_a_message() {
        // Client died mid-write: no trailing newline → Incomplete, never Message.
        let mut c = Cursor::new(br#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#);
        assert_eq!(read_message(&mut c).unwrap(), ReadOutcome::Incomplete);
        assert_eq!(read_message(&mut c).unwrap(), ReadOutcome::Eof);
    }

    #[test]
    fn broken_pipe_is_closed_not_error() {
        struct Closed;
        impl Write for Closed {
            fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed"))
            }
        }
        assert_eq!(
            write_message(&mut Closed, r#"{"jsonrpc":"2.0"}"#).unwrap(),
            WriteOutcome::Closed
        );
    }
}
