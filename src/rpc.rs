use serde_json::Value;
use std::io::{self, BufRead, Read, Write};

/// Reads one `Content-Length`-framed JSON-RPC message; `Ok(None)` at end of stream.
/// A body that is not valid JSON yields `InvalidData` after it was consumed, so callers can skip it.
/// A malformed header yields another error kind: the frame boundary is lost, so callers must stop.
pub fn read_message(reader: &mut impl BufRead) -> io::Result<Option<Value>> {
    let mut content_length = None;
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).map_err(io::Error::other)? == 0 {
            return Ok(None);
        }
        let header = line.trim_end_matches(['\r', '\n']);
        if header.is_empty() {
            if content_length.is_some() {
                break;
            }
            continue;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            let length = value
                .trim()
                .parse::<usize>()
                .map_err(|error| io::Error::other(format!("Content-Length: {error}")))?;
            content_length = Some(length);
        }
    }
    // The buffer grows with the bytes that arrive, not with the length the peer claims.
    let length = content_length.unwrap_or_default();
    let mut body = Vec::new();
    if reader.take(length as u64).read_to_end(&mut body)? < length {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

pub fn write_message(writer: &mut impl Write, message: &Value) -> io::Result<()> {
    let body = serde_json::to_vec(message)?;
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(&body)?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Cursor;

    #[test]
    fn round_trips_messages() {
        let mut buffer = Vec::new();
        write_message(&mut buffer, &json!({"jsonrpc": "2.0", "method": "a"})).unwrap();
        write_message(
            &mut buffer,
            &json!({"jsonrpc": "2.0", "id": 1, "result": "ü"}),
        )
        .unwrap();
        let mut reader = Cursor::new(buffer);
        assert_eq!(read_message(&mut reader).unwrap().unwrap()["method"], "a");
        assert_eq!(read_message(&mut reader).unwrap().unwrap()["result"], "ü");
        assert!(read_message(&mut reader).unwrap().is_none());
    }

    #[test]
    fn accepts_extra_headers_in_any_case() {
        let body = r#"{"id":7}"#;
        let raw = format!(
            "content-type: application/vscode-jsonrpc\r\ncontent-length: {}\r\n\r\n{body}",
            body.len()
        );
        let message = read_message(&mut Cursor::new(raw)).unwrap().unwrap();
        assert_eq!(message["id"], 7);
    }

    #[test]
    fn skips_invalid_json_without_losing_the_frame() {
        let raw = "Content-Length: 3\r\n\r\nnopContent-Length: 2\r\n\r\n{}";
        let mut reader = Cursor::new(raw);
        let error = read_message(&mut reader).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(read_message(&mut reader).unwrap().unwrap(), json!({}));
    }

    #[test]
    fn stops_at_a_malformed_header() {
        let raw = "Content-Length: abc\r\n\r\n{}Content-Length: 2\r\n\r\n{}";
        let error = read_message(&mut Cursor::new(raw)).unwrap_err();
        assert_ne!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn allocates_only_what_arrives() {
        let raw = format!("Content-Length: {}\r\n\r\n{{}}", usize::MAX);
        let error = read_message(&mut Cursor::new(raw)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
    }
}
