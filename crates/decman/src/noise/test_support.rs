//! Frames the noise tests share.

use super::{Message, MessageType};

/// The body a listener on `version` sends when it cannot decode our frame.
pub(super) fn refusal_on(version: u8) -> Vec<u8> {
    let mut frame = Message::new(
        MessageType::Error,
        b"request rejected: Noise protocol version mismatch".to_vec(),
    )
    .to_bytes();
    frame[0] = version;
    frame
}
