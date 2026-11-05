//! Framing: one JSON value per line, with a hard size limit.
//!
//! The limit matters on both sides. A client reads from a daemon that is
//! trusted, but the daemon reads from whatever connected to its socket, and an
//! unbounded read there is a way to exhaust memory. `serde_json` escapes
//! newlines inside strings, so a line break always ends a frame.

use std::io::{self, BufRead, Read, Write};

use serde::de::DeserializeOwned;
use serde::Serialize;

/// The largest frame either side will write or read, before its newline.
pub const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum WireError {
    #[error("the connection failed: {0}")]
    Io(#[from] io::Error),
    #[error("a message was longer than the limit of {MAX_FRAME_BYTES} bytes")]
    TooLong,
    #[error("the connection ended in the middle of a message")]
    Incomplete,
    #[error("a message was not valid JSON for this protocol")]
    Malformed,
}

/// Writes one value as a line and flushes it.
pub fn write_frame<W: Write, T: Serialize>(writer: &mut W, value: &T) -> Result<(), WireError> {
    let mut bytes = serde_json::to_vec(value).map_err(|_| WireError::Malformed)?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(WireError::TooLong);
    }
    bytes.push(b'\n');
    writer.write_all(&bytes)?;
    writer.flush()?;
    Ok(())
}

/// Reads one value. `Ok(None)` means the other side closed cleanly.
pub fn read_frame<R: BufRead, T: DeserializeOwned>(
    reader: &mut R,
) -> Result<Option<T>, WireError> {
    let mut line = Vec::new();
    // One byte over the limit, so a frame of exactly the limit still reads and
    // anything longer is refused rather than silently cut.
    let read = (&mut *reader)
        .take((MAX_FRAME_BYTES + 1) as u64)
        .read_until(b'\n', &mut line)?;
    if read == 0 {
        return Ok(None);
    }
    if line.last() != Some(&b'\n') {
        // Told apart so that a client that closed mid-write is not reported as
        // having sent something enormous.
        return Err(if read > MAX_FRAME_BYTES {
            WireError::TooLong
        } else {
            WireError::Incomplete
        });
    }
    line.pop();
    serde_json::from_slice(&line)
        .map(Some)
        .map_err(|_| WireError::Malformed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_value_survives_a_round_trip() {
        let mut buffer = Vec::new();
        write_frame(&mut buffer, &"hello\nthere").unwrap();
        assert_eq!(buffer.iter().filter(|b| **b == b'\n').count(), 1);
        let value: String = read_frame(&mut buffer.as_slice()).unwrap().unwrap();
        assert_eq!(value, "hello\nthere");
    }

    #[test]
    fn a_closed_connection_reads_as_nothing() {
        let value: Option<String> = read_frame(&mut [].as_slice()).unwrap();
        assert!(value.is_none());
    }

    #[test]
    fn an_overlong_line_is_refused_rather_than_cut() {
        let line = vec![b'x'; MAX_FRAME_BYTES + 2];
        let result: Result<Option<String>, _> = read_frame(&mut line.as_slice());
        assert!(matches!(result, Err(WireError::TooLong)));
    }

    #[test]
    fn a_message_cut_off_by_a_closed_connection_is_told_apart() {
        let result: Result<Option<String>, _> = read_frame(&mut b"\"half".as_slice());
        assert!(matches!(result, Err(WireError::Incomplete)));
    }

    #[test]
    fn a_line_that_is_not_json_is_refused() {
        let result: Result<Option<String>, _> = read_frame(&mut b"not json\n".as_slice());
        assert!(matches!(result, Err(WireError::Malformed)));
    }

    #[test]
    fn writing_an_overlong_value_is_refused() {
        let big = "x".repeat(MAX_FRAME_BYTES);
        let mut buffer = Vec::new();
        assert!(matches!(
            write_frame(&mut buffer, &big),
            Err(WireError::TooLong)
        ));
        assert!(buffer.is_empty());
    }
}
