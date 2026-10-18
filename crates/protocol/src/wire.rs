//! P0 framing. This isolated ALPN does not implement the coordinated MVP log.
use anyhow::{Result, ensure};

pub const P0_ALPN: &[u8] = b"chat/p0/mls/1";
pub const MAX_FRAME_BYTES: usize = 64 * 1024;
pub const MAX_TEXT_BYTES: usize = 8 * 1024;
const HEADER_BYTES: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum FrameKind {
    KeyPackage = 1,
    Welcome = 2,
    Application = 3,
}

pub fn encode_frame(kind: FrameKind, payload: &[u8]) -> Result<Vec<u8>> {
    ensure!(
        !payload.is_empty() && payload.len() <= MAX_FRAME_BYTES - HEADER_BYTES,
        "invalid frame size"
    );
    let mut frame = Vec::with_capacity(HEADER_BYTES + payload.len());
    frame.extend_from_slice(b"CP");
    frame.push(1);
    frame.push(kind as u8);
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(payload);
    Ok(frame)
}

pub fn decode_frame(frame: &[u8], expected: FrameKind) -> Result<&[u8]> {
    ensure!(
        (HEADER_BYTES + 1..=MAX_FRAME_BYTES).contains(&frame.len()),
        "invalid frame size"
    );
    ensure!(
        &frame[..2] == b"CP" && frame[2] == 1,
        "unsupported P0 protocol"
    );
    ensure!(frame[3] == expected as u8, "unexpected P0 frame kind");
    let len = u32::from_be_bytes(frame[4..8].try_into()?) as usize;
    ensure!(
        len == frame.len() - HEADER_BYTES,
        "truncated or trailing frame bytes"
    );
    Ok(&frame[HEADER_BYTES..])
}

pub fn validate_text(text: &str) -> Result<()> {
    ensure!(!text.trim().is_empty(), "message is empty");
    ensure!(
        text.len() <= MAX_TEXT_BYTES,
        "message exceeds 8192 UTF-8 bytes"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_wrong_version_kind_truncation_and_limit() {
        let good = encode_frame(FrameKind::Application, b"ciphertext").unwrap();
        assert_eq!(
            decode_frame(&good, FrameKind::Application).unwrap(),
            b"ciphertext"
        );
        assert!(decode_frame(&good, FrameKind::Welcome).is_err());
        assert!(decode_frame(&good[..good.len() - 1], FrameKind::Application).is_err());
        let mut bad = good.clone();
        bad[2] = 2;
        assert!(decode_frame(&bad, FrameKind::Application).is_err());
        assert!(encode_frame(FrameKind::Application, &vec![0; MAX_FRAME_BYTES]).is_err());
        assert!(validate_text(" ").is_err());
        assert!(validate_text(&"中".repeat(3000)).is_err());
    }
}
