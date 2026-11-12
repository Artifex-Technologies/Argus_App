//! Framing of the RPC stream. After `start_rpc_session` every message in either direction is one
//! `PB.Main`, written as a protobuf varint length followed by that many bytes (nanopb's
//! `PB_ENCODE_DELIMITED` / `PB_DECODE_DELIMITED` in the C++ app and the firmware).
//!
//! Unlike the C++ app, which keeps appending to its buffer until nanopb manages to decode a
//! message (a corrupt stream makes it wait forever and grow without limit), this decoder tells
//! "not complete yet" apart from "can never decode" and refuses any frame longer than
//! [`MAX_FRAME_LEN`].

use prost::Message;

use crate::proto::pb::Main;

/// The longest frame accepted or sent. The largest messages the firmware's nanopb options allow
/// are a list response (8 files, names up to 255 bytes, MD5 strings) and a screen frame
/// (1024 bytes); both stay far below 4 KiB. 16 KiB leaves room for long property values without
/// letting a broken or hostile device make us buffer more.
pub const MAX_FRAME_LEN: usize = 16 * 1024;

/// A protobuf length prefix for [`MAX_FRAME_LEN`] needs at most 3 bytes; nothing legitimate
/// uses more than the 5 bytes of a 32-bit varint.
const MAX_PREFIX_LEN: usize = 5;

/// What can go wrong while framing. Every variant ends the session: the stream has no
/// resynchronisation markers, so after a bad frame nothing that follows can be trusted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    /// The length prefix is not a valid varint of at most 5 bytes.
    BadPrefix,
    /// The length prefix announces a frame longer than [`MAX_FRAME_LEN`].
    TooLong(u64),
    /// The frame is complete but is not a valid `PB.Main`.
    Decode(String),
    /// More unread data was pushed than one frame can ever need.
    Overflow,
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::BadPrefix => write!(f, "invalid frame length prefix"),
            FrameError::TooLong(n) => write!(f, "frame of {n} bytes is longer than the {MAX_FRAME_LEN} allowed"),
            FrameError::Decode(e) => write!(f, "frame does not decode as PB.Main: {e}"),
            FrameError::Overflow => write!(f, "too much undecoded data buffered"),
        }
    }
}

impl std::error::Error for FrameError {}

/// Encodes one message as a frame. Fails only if the message is longer than [`MAX_FRAME_LEN`].
pub fn encode_frame(msg: &Main) -> Result<Vec<u8>, FrameError> {
    let len = msg.encoded_len();
    if len > MAX_FRAME_LEN {
        return Err(FrameError::TooLong(len as u64));
    }
    Ok(msg.encode_length_delimited_to_vec())
}

/// Reads the varint length prefix at the start of `buf`: `Ok(None)` if more bytes are needed,
/// `Ok(Some((length, prefix_size)))` when complete.
fn read_prefix(buf: &[u8]) -> Result<Option<(u64, usize)>, FrameError> {
    let mut value: u64 = 0;
    for (i, &b) in buf.iter().enumerate().take(MAX_PREFIX_LEN) {
        value |= u64::from(b & 0x7f) << (7 * i);
        if b & 0x80 == 0 {
            return Ok(Some((value, i + 1)));
        }
    }
    if buf.len() >= MAX_PREFIX_LEN {
        Err(FrameError::BadPrefix)
    } else {
        Ok(None)
    }
}

/// Splits a byte stream into messages. Push whatever the transport delivered, then call
/// [`FrameDecoder::next_frame`] until it returns `Ok(None)`.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    buf: Vec<u8>,
}

impl FrameDecoder {
    pub fn new() -> FrameDecoder {
        FrameDecoder::default()
    }

    /// Bytes received but not yet returned as a frame.
    pub fn pending(&self) -> usize {
        self.buf.len()
    }

    /// Appends received bytes. At most one incomplete frame stays buffered between calls when
    /// the caller drains [`FrameDecoder::next_frame`] after each push, so anything beyond two
    /// frames' worth of unread data means the caller is not draining and is refused.
    pub fn push(&mut self, data: &[u8]) -> Result<(), FrameError> {
        if self.buf.len() + data.len() > 2 * (MAX_FRAME_LEN + MAX_PREFIX_LEN) {
            return Err(FrameError::Overflow);
        }
        self.buf.extend_from_slice(data);
        Ok(())
    }

    /// The next complete message, `Ok(None)` if the buffer holds only part of one.
    pub fn next_frame(&mut self) -> Result<Option<Main>, FrameError> {
        let Some((len, prefix)) = read_prefix(&self.buf)? else {
            return Ok(None);
        };
        if len > MAX_FRAME_LEN as u64 {
            return Err(FrameError::TooLong(len));
        }
        let end = prefix + len as usize;
        if self.buf.len() < end {
            return Ok(None);
        }
        let msg = Main::decode(&self.buf[prefix..end]).map_err(|e| FrameError::Decode(e.to_string()))?;
        self.buf.drain(..end);
        Ok(Some(msg))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::pb::main::Content;
    use crate::proto::pb_system::ProtobufVersionRequest;

    fn version_request(id: u32) -> Main {
        Main { command_id: id, content: Some(Content::SystemProtobufVersionRequest(ProtobufVersionRequest {})), ..Default::default() }
    }

    #[test]
    fn round_trip_one_byte_at_a_time() {
        let a = encode_frame(&version_request(1)).unwrap();
        let b = encode_frame(&version_request(300)).unwrap();
        let stream: Vec<u8> = a.iter().chain(b.iter()).copied().collect();
        let mut d = FrameDecoder::new();
        let mut got = Vec::new();
        for byte in stream {
            d.push(&[byte]).unwrap();
            while let Some(m) = d.next_frame().unwrap() {
                got.push(m);
            }
        }
        assert_eq!(got, vec![version_request(1), version_request(300)]);
        assert_eq!(d.pending(), 0);
    }

    #[test]
    fn oversized_prefix_is_refused_before_buffering() {
        let mut d = FrameDecoder::new();
        // 0x80 0x80 0x02 = 32768
        d.push(&[0x80, 0x80, 0x02]).unwrap();
        assert_eq!(d.next_frame(), Err(FrameError::TooLong(32768)));
    }

    #[test]
    fn runaway_varint_is_refused() {
        let mut d = FrameDecoder::new();
        d.push(&[0xff; 4]).unwrap();
        assert_eq!(d.next_frame(), Ok(None));
        d.push(&[0xff]).unwrap();
        assert_eq!(d.next_frame(), Err(FrameError::BadPrefix));
    }

    #[test]
    fn complete_garbage_frame_is_an_error_not_a_wait() {
        let mut d = FrameDecoder::new();
        // length 2, then field 1 with wire type 7 (invalid)
        d.push(&[0x02, 0x0f, 0x00]).unwrap();
        assert!(matches!(d.next_frame(), Err(FrameError::Decode(_))));
    }

    #[test]
    fn empty_frame_is_an_empty_main() {
        let mut d = FrameDecoder::new();
        d.push(&[0x00]).unwrap();
        assert_eq!(d.next_frame(), Ok(Some(Main::default())));
    }

    #[test]
    fn undrained_buffer_is_refused() {
        let mut d = FrameDecoder::new();
        let big = vec![0u8; MAX_FRAME_LEN];
        d.push(&big).unwrap();
        d.push(&big).unwrap();
        assert_eq!(d.push(&big), Err(FrameError::Overflow));
    }
}
