//! The language server base protocol on stdio: each message is
//! `Content-Length: N\r\n\r\n` followed by N bytes of JSON. Websocket
//! clients send and receive the JSON alone.

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Largest message accepted from a server, so a bad header cannot make
/// the relay allocate without bound.
pub const MAX_MESSAGE: usize = 64 * 1024 * 1024;

/// Largest header block accepted from a server, so a server that never
/// ends a header line cannot make the relay buffer it without bound.
pub const MAX_HEADER: usize = 8 * 1024;

/// Writes one message with its header.
pub async fn write_message<W: AsyncWrite + Unpin>(w: &mut W, body: &str) -> std::io::Result<()> {
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    w.write_all(header.as_bytes()).await?;
    w.write_all(body.as_bytes()).await?;
    w.flush().await
}

/// Reads the next message. `None` at end of stream. Headers other than
/// `Content-Length` are ignored, and so is a header block without one,
/// as `jupyter-lsp` does; the body is trimmed of surrounding whitespace.
pub async fn read_message<R: AsyncBufRead + Unpin>(r: &mut R) -> std::io::Result<Option<String>> {
    loop {
        let mut length: Option<usize> = None;
        let mut saw_header = false;
        let mut header_bytes = 0;
        loop {
            let mut line = String::new();
            let room = MAX_HEADER - header_bytes;
            let n = (&mut *r).take(room as u64 + 1).read_line(&mut line).await?;
            if n == 0 {
                return Ok(None);
            }
            if saw_header || !line.trim().is_empty() {
                header_bytes += n;
            }
            if header_bytes > MAX_HEADER {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("message header longer than {MAX_HEADER} bytes"),
                ));
            }
            let line = line.trim();
            if line.is_empty() {
                if saw_header {
                    break;
                }
                // Blank lines between messages.
                continue;
            }
            saw_header = true;
            if let Some((name, value)) = line.split_once(':') {
                if name.trim().eq_ignore_ascii_case("content-length") {
                    length = value.trim().parse().ok();
                }
            }
        }
        let Some(length) = length.filter(|n| *n > 0) else {
            continue;
        };
        if length > MAX_MESSAGE {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("message of {length} bytes is too large"),
            ));
        }
        let mut body = vec![0; length];
        r.read_exact(&mut body).await?;
        return Ok(Some(String::from_utf8_lossy(&body).trim().to_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn round_trip_and_odd_headers() {
        let mut out = Vec::new();
        write_message(&mut out, r#"{"id":1}"#).await.unwrap();
        write_message(&mut out, "{\"x\":\"é\"}").await.unwrap();
        assert!(out.starts_with(b"Content-Length: 8\r\n\r\n{\"id\":1}"));
        // A second header, lower case, and a block with no length.
        out.extend_from_slice(b"X-Other: 1\r\n\r\n");
        out.extend_from_slice(b"content-type: utf-8\r\ncontent-length: 2\r\n\r\n{}");
        let mut r = tokio::io::BufReader::new(&out[..]);
        assert_eq!(read_message(&mut r).await.unwrap().unwrap(), r#"{"id":1}"#);
        assert_eq!(
            read_message(&mut r).await.unwrap().unwrap(),
            "{\"x\":\"é\"}"
        );
        assert_eq!(read_message(&mut r).await.unwrap().unwrap(), "{}");
        assert_eq!(read_message(&mut r).await.unwrap(), None);
    }

    #[tokio::test]
    async fn truncated_body_is_an_error() {
        let mut r = tokio::io::BufReader::new(&b"Content-Length: 10\r\n\r\n{}"[..]);
        assert!(read_message(&mut r).await.is_err());
        let mut r = tokio::io::BufReader::new(&b"Content-Length: 99999999999\r\n\r\n"[..]);
        assert!(read_message(&mut r).await.is_err());
    }

    /// A header line, or block, that never ends is refused once it passes
    /// `MAX_HEADER`, instead of being buffered without bound.
    #[tokio::test]
    async fn an_endless_header_is_an_error() {
        let long = vec![b'x'; MAX_HEADER * 4];
        let mut r = tokio::io::BufReader::new(&long[..]);
        let err = read_message(&mut r).await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);

        let mut many = Vec::new();
        for _ in 0..MAX_HEADER {
            many.extend_from_slice(b"X-A: 1\r\n");
        }
        let mut r = tokio::io::BufReader::new(&many[..]);
        assert!(read_message(&mut r).await.is_err());

        // A header just under the limit still reads, and so do blank
        // lines between messages, however many.
        let mut ok = vec![b'\n'; MAX_HEADER * 2];
        ok.extend_from_slice(format!("X-Pad: {}\r\n", "p".repeat(MAX_HEADER - 64)).as_bytes());
        ok.extend_from_slice(b"Content-Length: 2\r\n\r\n{}");
        let mut r = tokio::io::BufReader::new(&ok[..]);
        assert_eq!(read_message(&mut r).await.unwrap().unwrap(), "{}");
    }
}
