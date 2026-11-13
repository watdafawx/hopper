//! The tunnel's frames. One WebSocket binary message carries exactly one
//! frame, so the WebSocket layer does the length framing and this layer
//! only has a fixed 5-byte header: the type, then the stream id (u32,
//! big-endian; 0 is the connection itself).

use std::fmt;

/// Tunnel protocol version. A node and a main talk when their versions are
/// equal or one apart (see `design/multi-panel.md`, "Version skew").
pub const TUNNEL_VERSION: u16 = 1;

/// Biggest DATA payload. Small enough that one busy stream can't hold the
/// socket for long (64 KiB is ~5 ms at 100 Mbit/s), big enough that the
/// header and WebSocket overhead stay under 0.1%.
pub const MAX_DATA: usize = 64 * 1024;
/// Biggest OPEN head (the relayed request line, headers and who is asking)
/// and HELLO. Bodies go as DATA, never in the head.
pub const MAX_HEAD: usize = 16 * 1024;
/// Biggest RESET / GOAWAY reason text.
pub const MAX_REASON: usize = 256;
/// Biggest frame on the wire; the WebSocket layer refuses anything larger
/// before it is buffered.
pub const MAX_FRAME: usize = HEADER + MAX_DATA;
pub const HEADER: usize = 5;

/// RESET and GOAWAY codes.
pub mod code {
    /// The other side no longer wants the stream (dropped, cancelled).
    pub const CANCEL: u16 = 1;
    /// Too many open streams, or the opener's id was not acceptable.
    pub const REFUSED: u16 = 2;
    /// The stream's handler failed.
    pub const INTERNAL: u16 = 3;
    /// The peer broke the protocol; the connection is closed after this.
    pub const PROTOCOL: u16 = 4;
    /// Orderly shutdown of the whole connection (GOAWAY only).
    pub const BYE: u16 = 5;
    /// Tunnel versions too far apart (GOAWAY only).
    pub const VERSION: u16 = 6;
}

#[derive(Clone, PartialEq, Eq)]
pub enum Frame {
    /// First frame each way, stream 0: JSON `Hello`.
    Hello(Vec<u8>),
    /// A new stream; the payload is its head (JSON, at most `MAX_HEAD`).
    Open { id: u32, head: Vec<u8> },
    /// Stream bytes, at most `MAX_DATA`, counted against the window.
    Data { id: u32, data: Vec<u8> },
    /// The receiver may take `n` more bytes on this stream.
    Credit { id: u32, n: u32 },
    /// The sender has finished sending on this stream (half close).
    Close { id: u32 },
    /// The stream is dead both ways.
    Reset { id: u32, code: u16, reason: String },
    Ping(u64),
    Pong(u64),
    /// The connection is ending; stream 0.
    GoAway { code: u16, reason: String },
}

impl fmt::Debug for Frame {
    // Payloads can be megabytes of file data: print lengths only.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Frame::Hello(b) => write!(f, "Hello({} bytes)", b.len()),
            Frame::Open { id, head } => write!(f, "Open({id}, {} bytes)", head.len()),
            Frame::Data { id, data } => write!(f, "Data({id}, {} bytes)", data.len()),
            Frame::Credit { id, n } => write!(f, "Credit({id}, {n})"),
            Frame::Close { id } => write!(f, "Close({id})"),
            Frame::Reset { id, code, reason } => write!(f, "Reset({id}, {code}, {reason:?})"),
            Frame::Ping(n) => write!(f, "Ping({n})"),
            Frame::Pong(n) => write!(f, "Pong({n})"),
            Frame::GoAway { code, reason } => write!(f, "GoAway({code}, {reason:?})"),
        }
    }
}

const T_HELLO: u8 = 1;
const T_OPEN: u8 = 2;
const T_DATA: u8 = 3;
const T_CREDIT: u8 = 4;
const T_CLOSE: u8 = 5;
const T_RESET: u8 = 6;
const T_PING: u8 = 7;
const T_PONG: u8 = 8;
const T_GOAWAY: u8 = 9;

#[derive(Debug, PartialEq, Eq)]
pub enum DecodeError {
    Short,
    UnknownType(u8),
    TooBig,
    /// A connection frame on a stream id, or a stream frame on 0.
    BadStream,
    /// Bytes after a fixed-size payload, or a reason that isn't UTF-8.
    Malformed,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl Frame {
    pub fn encode(&self) -> Vec<u8> {
        let (t, id, payload_len) = match self {
            Frame::Hello(b) => (T_HELLO, 0, b.len()),
            Frame::Open { id, head } => (T_OPEN, *id, head.len()),
            Frame::Data { id, data } => (T_DATA, *id, data.len()),
            Frame::Credit { id, .. } => (T_CREDIT, *id, 4),
            Frame::Close { id } => (T_CLOSE, *id, 0),
            Frame::Reset { id, reason, .. } => (T_RESET, *id, 2 + reason.len().min(MAX_REASON)),
            Frame::Ping(_) => (T_PING, 0, 8),
            Frame::Pong(_) => (T_PONG, 0, 8),
            Frame::GoAway { reason, .. } => (T_GOAWAY, 0, 2 + reason.len().min(MAX_REASON)),
        };
        let mut out = Vec::with_capacity(HEADER + payload_len);
        out.push(t);
        out.extend_from_slice(&id.to_be_bytes());
        match self {
            Frame::Hello(b) | Frame::Open { head: b, .. } | Frame::Data { data: b, .. } => out.extend_from_slice(b),
            Frame::Credit { n, .. } => out.extend_from_slice(&n.to_be_bytes()),
            Frame::Close { .. } => {}
            Frame::Reset { code, reason, .. } | Frame::GoAway { code, reason } => {
                out.extend_from_slice(&code.to_be_bytes());
                out.extend_from_slice(truncate_utf8(reason, MAX_REASON).as_bytes());
            }
            Frame::Ping(n) | Frame::Pong(n) => out.extend_from_slice(&n.to_be_bytes()),
        }
        out
    }

    pub fn decode(buf: &[u8]) -> Result<Frame, DecodeError> {
        if buf.len() < HEADER {
            return Err(DecodeError::Short);
        }
        if buf.len() > MAX_FRAME {
            return Err(DecodeError::TooBig);
        }
        let t = buf[0];
        let id = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]);
        let p = &buf[HEADER..];
        let conn_frame = matches!(t, T_HELLO | T_PING | T_PONG | T_GOAWAY);
        let stream_frame = matches!(t, T_OPEN | T_DATA | T_CREDIT | T_CLOSE | T_RESET);
        if !conn_frame && !stream_frame {
            return Err(DecodeError::UnknownType(t));
        }
        if conn_frame != (id == 0) {
            return Err(DecodeError::BadStream);
        }
        let fixed = |n: usize| if p.len() == n { Ok(()) } else { Err(DecodeError::Malformed) };
        let coded = |p: &[u8]| -> Result<(u16, String), DecodeError> {
            if p.len() < 2 {
                return Err(DecodeError::Short);
            }
            if p.len() > 2 + MAX_REASON {
                return Err(DecodeError::TooBig);
            }
            let reason = std::str::from_utf8(&p[2..]).map_err(|_| DecodeError::Malformed)?;
            Ok((u16::from_be_bytes([p[0], p[1]]), reason.to_string()))
        };
        Ok(match t {
            T_HELLO | T_OPEN if p.len() > MAX_HEAD => return Err(DecodeError::TooBig),
            T_HELLO => Frame::Hello(p.to_vec()),
            T_OPEN => Frame::Open { id, head: p.to_vec() },
            T_DATA => Frame::Data { id, data: p.to_vec() },
            T_CREDIT => {
                fixed(4)?;
                Frame::Credit { id, n: u32::from_be_bytes([p[0], p[1], p[2], p[3]]) }
            }
            T_CLOSE => {
                fixed(0)?;
                Frame::Close { id }
            }
            T_RESET => {
                let (code, reason) = coded(p)?;
                Frame::Reset { id, code, reason }
            }
            T_PING | T_PONG => {
                fixed(8)?;
                let n = u64::from_be_bytes(p.try_into().expect("8 bytes"));
                if t == T_PING { Frame::Ping(n) } else { Frame::Pong(n) }
            }
            T_GOAWAY => {
                let (code, reason) = coded(p)?;
                Frame::GoAway { code, reason }
            }
            _ => unreachable!(),
        })
    }
}

fn truncate_utf8(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// What each side says first.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct Hello {
    pub tunnel: u16,
    /// The hopperd version, for "Main is older" / "Update needed".
    pub version: String,
    /// The API's capability list; the main lights up the intersection.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Bytes this side lets the other send per stream before a CREDIT.
    pub window: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round(f: Frame) {
        let bytes = f.encode();
        assert_eq!(Frame::decode(&bytes).unwrap(), f, "{f:?}");
    }

    #[test]
    fn every_frame_round_trips() {
        round(Frame::Hello(br#"{"tunnel":1}"#.to_vec()));
        round(Frame::Open { id: 2, head: br#"{"kind":"echo"}"#.to_vec() });
        round(Frame::Data { id: 3, data: vec![7; MAX_DATA] });
        round(Frame::Data { id: 3, data: vec![] });
        round(Frame::Credit { id: u32::MAX, n: 262_144 });
        round(Frame::Close { id: 9 });
        round(Frame::Reset { id: 9, code: code::CANCEL, reason: "gone".into() });
        round(Frame::Ping(u64::MAX));
        round(Frame::Pong(1));
        round(Frame::GoAway { code: code::BYE, reason: String::new() });
    }

    #[test]
    fn header_layout_is_type_then_big_endian_id() {
        let b = Frame::Credit { id: 0x0102_0304, n: 0x0a0b_0c0d }.encode();
        assert_eq!(b, [T_CREDIT, 1, 2, 3, 4, 0x0a, 0x0b, 0x0c, 0x0d]);
    }

    #[test]
    fn bad_frames_are_refused() {
        assert_eq!(Frame::decode(&[T_DATA, 0, 0]), Err(DecodeError::Short));
        assert_eq!(Frame::decode(&[0xee, 0, 0, 0, 1]), Err(DecodeError::UnknownType(0xee)));
        // A data frame on the connection's id, a ping on a stream's.
        assert_eq!(Frame::decode(&[T_DATA, 0, 0, 0, 0, 1]), Err(DecodeError::BadStream));
        assert_eq!(Frame::decode(&[T_PING, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0]), Err(DecodeError::BadStream));
        // Fixed-size payloads must be exact.
        assert_eq!(Frame::decode(&[T_CREDIT, 0, 0, 0, 1, 0, 0, 1]), Err(DecodeError::Malformed));
        assert_eq!(Frame::decode(&[T_CLOSE, 0, 0, 0, 1, 0]), Err(DecodeError::Malformed));
        // Oversized data and heads.
        let mut big = Frame::Data { id: 1, data: vec![0; MAX_DATA] }.encode();
        big.push(0);
        assert_eq!(Frame::decode(&big), Err(DecodeError::TooBig));
        let head = Frame::Open { id: 1, head: vec![b'x'; MAX_HEAD + 1] }.encode();
        assert_eq!(Frame::decode(&head), Err(DecodeError::TooBig));
        // A reason that isn't text.
        assert_eq!(Frame::decode(&[T_RESET, 0, 0, 0, 1, 0, 1, 0xff]), Err(DecodeError::Malformed));
    }

    #[test]
    fn long_reasons_are_cut_on_a_character_boundary() {
        let reason = "é".repeat(MAX_REASON); // 2 bytes each
        let f = Frame::decode(&Frame::Reset { id: 1, code: 3, reason }.encode()).unwrap();
        let Frame::Reset { reason, .. } = f else { panic!() };
        assert_eq!(reason.len(), MAX_REASON);
    }
}
