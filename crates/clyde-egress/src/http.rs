//! A minimal HTTP/1.1 codec.
//!
//! Only the proxy needs this: `CONNECT` parsing for every profile, and request
//! and response framing for the one profile whose TLS the proxy terminates
//! (`model-api`, D7 amendment). A full HTTP stack would be a larger trusted
//! surface than the carve-out justifies, so this handles exactly the shapes the
//! proxy must understand and refuses everything else.
//!
//! Framing is the security-relevant part. A proxy that miscounts a body length
//! desynchronises the stream, and a desynchronised stream is how request
//! smuggling works — so both `Content-Length` and `Transfer-Encoding: chunked`
//! are parsed strictly, and a message carrying both is refused.

use std::fmt::Write as _;

/// Upper bound on a request or response head. An actor is untrusted input, so
/// header parsing must not allocate without limit.
pub const MAX_HEAD_BYTES: usize = 64 * 1024;

/// Upper bound on a single header line.
const MAX_HEADER_LINE: usize = 8 * 1024;

/// A parsed request line plus headers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestHead {
    pub method: String,
    pub target: String,
    pub version: String,
    pub headers: Vec<(String, String)>,
}

/// A parsed status line plus headers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseHead {
    pub version: String,
    pub status: u16,
    pub reason: String,
    pub headers: Vec<(String, String)>,
}

/// What went wrong parsing a message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HttpError {
    #[error("the message head is malformed")]
    Malformed,
    #[error("the message head exceeds {MAX_HEAD_BYTES} bytes")]
    HeadTooLarge,
    #[error("a header line exceeds {MAX_HEADER_LINE} bytes")]
    HeaderTooLong,
    #[error("the message carries both Content-Length and Transfer-Encoding, which is ambiguous")]
    AmbiguousFraming,
    #[error("Content-Length is not a plain decimal length")]
    BadContentLength,
    #[error("the transfer encoding {0:?} is not supported")]
    UnsupportedTransferEncoding(String),
    #[error("a chunk header is malformed")]
    BadChunkHeader,
}

/// Splits a buffer at the end of the message head.
///
/// Returns the head bytes and the index at which the body begins, or `None` if
/// the terminator has not arrived yet.
pub fn split_head(buffer: &[u8]) -> Result<Option<(&[u8], usize)>, HttpError> {
    if let Some(index) = find_double_crlf(buffer) {
        let head = buffer.get(..index).ok_or(HttpError::Malformed)?;
        return Ok(Some((head, index + 4)));
    }
    if buffer.len() > MAX_HEAD_BYTES {
        return Err(HttpError::HeadTooLarge);
    }
    Ok(None)
}

fn find_double_crlf(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

fn parse_headers(lines: &[&str]) -> Result<Vec<(String, String)>, HttpError> {
    lines
        .iter()
        .map(|line| {
            if line.len() > MAX_HEADER_LINE {
                return Err(HttpError::HeaderTooLong);
            }
            let (name, value) = line.split_once(':').ok_or(HttpError::Malformed)?;
            if name.is_empty() || name.contains(char::is_whitespace) {
                return Err(HttpError::Malformed);
            }
            Ok((name.to_owned(), value.trim().to_owned()))
        })
        .collect()
}

impl RequestHead {
    /// Parses a request head. `head` excludes the terminating CRLFCRLF.
    pub fn parse(head: &[u8]) -> Result<Self, HttpError> {
        let text = std::str::from_utf8(head).map_err(|_| HttpError::Malformed)?;
        let mut lines = text.split("\r\n");
        let request_line = lines.next().ok_or(HttpError::Malformed)?;
        let mut parts = request_line.split(' ');
        let method = parts
            .next()
            .filter(|s| !s.is_empty())
            .ok_or(HttpError::Malformed)?;
        let target = parts
            .next()
            .filter(|s| !s.is_empty())
            .ok_or(HttpError::Malformed)?;
        let version = parts
            .next()
            .filter(|s| !s.is_empty())
            .ok_or(HttpError::Malformed)?;
        if parts.next().is_some() {
            return Err(HttpError::Malformed);
        }
        let header_lines: Vec<&str> = lines.filter(|line| !line.is_empty()).collect();
        Ok(Self {
            method: method.to_owned(),
            target: target.to_owned(),
            version: version.to_owned(),
            headers: parse_headers(&header_lines)?,
        })
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// Replaces or inserts a header.
    ///
    /// Used to inject `Authorization` host-side, so the agent never holds the
    /// model API credential (D11). Any client-supplied value is removed first:
    /// the injected credential is the only one that reaches upstream.
    pub fn set_header(&mut self, name: &str, value: &str) {
        self.headers
            .retain(|(key, _)| !key.eq_ignore_ascii_case(name));
        self.headers.push((name.to_owned(), value.to_owned()));
    }

    pub fn remove_header(&mut self, name: &str) {
        self.headers
            .retain(|(key, _)| !key.eq_ignore_ascii_case(name));
    }

    /// Serialises the head, including the terminating CRLFCRLF.
    pub fn render(&self) -> String {
        let mut out = String::new();
        let _ = write!(out, "{} {} {}\r\n", self.method, self.target, self.version);
        for (name, value) in &self.headers {
            let _ = write!(out, "{name}: {value}\r\n");
        }
        out.push_str("\r\n");
        out
    }

    /// How the body is framed.
    pub fn framing(&self) -> Result<BodyFraming, HttpError> {
        framing_from_headers(&self.headers, BodyFraming::None)
    }

    /// The `host:port` a `CONNECT` names.
    pub fn connect_target(&self) -> Option<(&str, u16)> {
        if !self.method.eq_ignore_ascii_case("CONNECT") {
            return None;
        }
        let (host, port) = self.target.rsplit_once(':')?;
        // An IPv6 literal in a CONNECT target is bracketed; rejecting it keeps
        // allowlisting to hostnames, which is what the model promises.
        if host.contains('[') || host.contains(']') || host.is_empty() {
            return None;
        }
        let port: u16 = port.parse().ok()?;
        Some((host, port))
    }
}

impl ResponseHead {
    pub fn parse(head: &[u8]) -> Result<Self, HttpError> {
        let text = std::str::from_utf8(head).map_err(|_| HttpError::Malformed)?;
        let mut lines = text.split("\r\n");
        let status_line = lines.next().ok_or(HttpError::Malformed)?;
        let mut parts = status_line.splitn(3, ' ');
        let version = parts
            .next()
            .filter(|s| !s.is_empty())
            .ok_or(HttpError::Malformed)?;
        let status: u16 = parts
            .next()
            .ok_or(HttpError::Malformed)?
            .parse()
            .map_err(|_| HttpError::Malformed)?;
        let reason = parts.next().unwrap_or("").to_owned();
        let header_lines: Vec<&str> = lines.filter(|line| !line.is_empty()).collect();
        Ok(Self {
            version: version.to_owned(),
            status,
            reason,
            headers: parse_headers(&header_lines)?,
        })
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        let _ = write!(out, "{} {} {}\r\n", self.version, self.status, self.reason);
        for (name, value) in &self.headers {
            let _ = write!(out, "{name}: {value}\r\n");
        }
        out.push_str("\r\n");
        out
    }

    /// How the body is framed.
    ///
    /// A response with neither `Content-Length` nor chunked encoding runs until
    /// the connection closes, which is the HTTP/1.1 default for responses.
    pub fn framing(&self) -> Result<BodyFraming, HttpError> {
        if matches!(self.status, 204 | 304) || (100..200).contains(&self.status) {
            return Ok(BodyFraming::None);
        }
        framing_from_headers(&self.headers, BodyFraming::UntilClose)
    }
}

/// How a message body is delimited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyFraming {
    /// No body.
    None,
    Length(u64),
    Chunked,
    /// Ends when the peer closes the connection.
    UntilClose,
}

fn framing_from_headers(
    headers: &[(String, String)],
    default: BodyFraming,
) -> Result<BodyFraming, HttpError> {
    let find = |name: &str| {
        headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    };
    let length = find("content-length");
    let encoding = find("transfer-encoding");
    match (length, encoding) {
        // Both present is the classic request-smuggling shape. Refused rather
        // than resolved by precedence, because "which one wins" is exactly the
        // disagreement an attacker exploits.
        (Some(_), Some(_)) => Err(HttpError::AmbiguousFraming),
        (Some(length), None) => {
            let length: u64 = length
                .trim()
                .parse()
                .map_err(|_| HttpError::BadContentLength)?;
            Ok(BodyFraming::Length(length))
        }
        (None, Some(encoding)) => {
            if encoding.trim().eq_ignore_ascii_case("chunked") {
                Ok(BodyFraming::Chunked)
            } else {
                Err(HttpError::UnsupportedTransferEncoding(encoding.to_owned()))
            }
        }
        (None, None) => Ok(default),
    }
}

/// Parses a chunk size line, returning the size in bytes.
pub fn parse_chunk_size(line: &str) -> Result<u64, HttpError> {
    // Chunk extensions after `;` are permitted by the specification and ignored.
    let size = line.split(';').next().unwrap_or("").trim();
    if size.is_empty() {
        return Err(HttpError::BadChunkHeader);
    }
    u64::from_str_radix(size, 16).map_err(|_| HttpError::BadChunkHeader)
}

/// Renders a proxy-generated response, used for refusals.
pub fn simple_response(status: u16, reason: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]
    use super::*;

    #[test]
    fn a_connect_request_parses_and_names_its_target() {
        let raw = b"CONNECT api.example.test:443 HTTP/1.1\r\nHost: api.example.test:443\r\n\r\n";
        let (head, body_at) = split_head(raw).unwrap().unwrap();
        assert_eq!(body_at, raw.len());
        let request = RequestHead::parse(head).unwrap();
        assert_eq!(request.method, "CONNECT");
        assert_eq!(request.connect_target(), Some(("api.example.test", 443u16)));
    }

    #[test]
    fn a_non_connect_method_has_no_connect_target() {
        let request = RequestHead::parse(b"GET /v1/messages HTTP/1.1\r\nHost: x\r\n").unwrap();
        assert_eq!(request.connect_target(), None);
    }

    #[test]
    fn malformed_connect_targets_are_refused() {
        for target in ["api.example.test", "api.example.test:", ":443", "[::1]:443"] {
            let raw = format!("CONNECT {target} HTTP/1.1\r\nHost: x\r\n");
            let request = RequestHead::parse(raw.as_bytes()).unwrap();
            assert_eq!(
                request.connect_target(),
                None,
                "{target} must not resolve to an allowlist decision"
            );
        }
    }

    #[test]
    fn an_incomplete_head_yields_none_until_the_terminator_arrives() {
        assert_eq!(split_head(b"CONNECT x:443 HTTP/1.1\r\n").unwrap(), None);
        let complete = b"CONNECT x:443 HTTP/1.1\r\n\r\n";
        assert!(split_head(complete).unwrap().is_some());
    }

    #[test]
    fn an_oversized_head_is_refused_rather_than_buffered() {
        let huge = vec![b'a'; MAX_HEAD_BYTES + 1];
        assert_eq!(split_head(&huge), Err(HttpError::HeadTooLarge));
    }

    #[test]
    fn malformed_request_lines_are_refused() {
        for raw in [
            &b"CONNECT\r\n"[..],
            &b"CONNECT x:443\r\n"[..],
            &b"CONNECT x:443 HTTP/1.1 extra\r\n"[..],
            &b"\r\n"[..],
        ] {
            assert!(RequestHead::parse(raw).is_err(), "{raw:?} must be refused");
        }
    }

    #[test]
    fn header_injection_replaces_any_client_supplied_value() {
        let mut request = RequestHead::parse(
            b"POST /v1/messages HTTP/1.1\r\nHost: api.test\r\nAuthorization: Bearer attacker\r\n",
        )
        .unwrap();
        request.set_header("Authorization", "Bearer injected");
        let rendered = request.render();
        assert!(!rendered.contains("attacker"), "{rendered}");
        assert_eq!(rendered.matches("Authorization").count(), 1);
        assert!(rendered.ends_with("\r\n\r\n"));
    }

    #[test]
    fn header_lookup_is_case_insensitive() {
        let request = RequestHead::parse(b"GET / HTTP/1.1\r\nCoNtEnT-lEnGtH: 5\r\n").unwrap();
        assert_eq!(request.header("content-length"), Some("5"));
        assert_eq!(request.header("absent"), None);
    }

    #[test]
    fn content_length_and_chunked_together_are_refused() {
        // The request-smuggling shape: refused rather than resolved by
        // precedence, because "which one wins" is the disagreement attackers use.
        let request = RequestHead::parse(
            b"POST / HTTP/1.1\r\nContent-Length: 5\r\nTransfer-Encoding: chunked\r\n",
        )
        .unwrap();
        assert_eq!(request.framing(), Err(HttpError::AmbiguousFraming));
    }

    #[test]
    fn framing_is_read_from_the_headers() {
        let with_length = RequestHead::parse(b"POST / HTTP/1.1\r\nContent-Length: 12\r\n").unwrap();
        assert_eq!(with_length.framing(), Ok(BodyFraming::Length(12)));

        let chunked =
            RequestHead::parse(b"POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n").unwrap();
        assert_eq!(chunked.framing(), Ok(BodyFraming::Chunked));

        let none = RequestHead::parse(b"GET / HTTP/1.1\r\nHost: x\r\n").unwrap();
        assert_eq!(none.framing(), Ok(BodyFraming::None));

        let bad = RequestHead::parse(b"POST / HTTP/1.1\r\nContent-Length: abc\r\n").unwrap();
        assert_eq!(bad.framing(), Err(HttpError::BadContentLength));

        let unsupported =
            RequestHead::parse(b"POST / HTTP/1.1\r\nTransfer-Encoding: gzip\r\n").unwrap();
        assert!(matches!(
            unsupported.framing(),
            Err(HttpError::UnsupportedTransferEncoding(_))
        ));
    }

    #[test]
    fn responses_frame_by_status_and_headers() {
        let no_content = ResponseHead::parse(b"HTTP/1.1 204 No Content\r\n").unwrap();
        assert_eq!(no_content.framing(), Ok(BodyFraming::None));

        let sized = ResponseHead::parse(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n").unwrap();
        assert_eq!(sized.framing(), Ok(BodyFraming::Length(3)));

        let streamed =
            ResponseHead::parse(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n").unwrap();
        assert_eq!(streamed.framing(), Ok(BodyFraming::UntilClose));

        let chunked =
            ResponseHead::parse(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n").unwrap();
        assert_eq!(chunked.framing(), Ok(BodyFraming::Chunked));
    }

    #[test]
    fn response_round_trips() {
        let response =
            ResponseHead::parse(b"HTTP/1.1 429 Too Many Requests\r\nRetry-After: 5\r\n").unwrap();
        assert_eq!(response.status, 429);
        assert!(
            response
                .render()
                .starts_with("HTTP/1.1 429 Too Many Requests\r\n")
        );
    }

    #[test]
    fn chunk_sizes_parse_in_hex_and_ignore_extensions() {
        assert_eq!(parse_chunk_size("1a"), Ok(26));
        assert_eq!(parse_chunk_size("0"), Ok(0));
        assert_eq!(parse_chunk_size("ff;name=value"), Ok(255));
        assert_eq!(parse_chunk_size(""), Err(HttpError::BadChunkHeader));
        assert_eq!(parse_chunk_size("xyz"), Err(HttpError::BadChunkHeader));
    }

    #[test]
    fn refusal_responses_are_well_formed() {
        let rendered = simple_response(403, "Forbidden", "destination is not allowlisted");
        assert!(rendered.starts_with("HTTP/1.1 403 Forbidden\r\n"));
        assert!(rendered.contains("Content-Length: 30"));
        assert!(rendered.ends_with("destination is not allowlisted"));
    }
}
