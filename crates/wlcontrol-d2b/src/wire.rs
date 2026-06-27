//! Wire framing primitives for the d2bd public socket.
//!
//! The public protocol frames every message as a 4-byte little-endian unsigned
//! length prefix followed by one UTF-8 JSON document, with a 1 MiB cap
//! (`docs/reference/daemon-api.md`). These helpers are protocol-stable and are
//! provided by Wave 0 so the Wave 1 protocol-client and test-harness agents
//! share one framing implementation.

use wlcontrol_core::error::{WlError, WlResult};

/// Maximum accepted frame size (1 MiB), matching the daemon.
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

/// Encode a JSON payload into a length-prefixed frame.
pub fn encode_frame(json: &[u8]) -> WlResult<Vec<u8>> {
    if json.len() > MAX_FRAME_BYTES {
        return Err(WlError::Protocol(format!(
            "frame too large: {} > {MAX_FRAME_BYTES}",
            json.len()
        )));
    }
    let mut frame = Vec::with_capacity(4 + json.len());
    frame.extend_from_slice(&(json.len() as u32).to_le_bytes());
    frame.extend_from_slice(json);
    Ok(frame)
}

/// Decode a length-prefixed frame, returning the JSON payload bytes.
///
/// `frame` must contain the 4-byte prefix followed by exactly the declared
/// number of payload bytes (as delivered by a single `SOCK_SEQPACKET` message).
pub fn decode_frame(frame: &[u8]) -> WlResult<&[u8]> {
    if frame.len() < 4 {
        return Err(WlError::Protocol("frame shorter than length prefix".into()));
    }
    let len = u32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]]) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(WlError::Protocol(format!(
            "declared frame length {len} exceeds cap {MAX_FRAME_BYTES}"
        )));
    }
    let payload = &frame[4..];
    if payload.len() != len {
        return Err(WlError::Protocol(format!(
            "frame payload length {} does not match declared {len}",
            payload.len()
        )));
    }
    Ok(payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_payload() {
        let payload = br#"{"kind":"list"}"#;
        let frame = encode_frame(payload).expect("encode");
        assert_eq!(&frame[0..4], &(payload.len() as u32).to_le_bytes());
        let decoded = decode_frame(&frame).expect("decode");
        assert_eq!(decoded, payload);
    }

    #[test]
    fn rejects_oversized_encode() {
        let big = vec![0u8; MAX_FRAME_BYTES + 1];
        assert!(encode_frame(&big).is_err());
    }

    #[test]
    fn rejects_truncated_frame() {
        assert!(decode_frame(&[0, 0]).is_err());
    }

    #[test]
    fn rejects_length_mismatch() {
        // Declares 10 bytes but carries 2.
        let frame = [10u8, 0, 0, 0, b'h', b'i'];
        assert!(decode_frame(&frame).is_err());
    }
}
