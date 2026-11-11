pub mod client;
mod error;
mod keypair;
mod message;
pub mod server;
#[cfg(test)]
mod test_support;
mod transport;

pub use error::NoiseError;
pub(crate) use error::is_transient;
pub use keypair::{NoiseKeypair, NoiseSecretKey, load_or_generate_keypair, parse_public_key};
pub use message::{
    ACS_BLOCK_SIZE, CHUNK_SIZE, MAX_ACS_BLOCK_SIZE, MAX_CHUNK_COUNT, MAX_CHUNKED_TOTAL_SIZE,
    MAX_PAYLOAD_SIZE, Message, MessageType, WIRE_VERSION, acs_block_size,
};
pub use transport::{
    NOISE_ACS_BLOCK_TIMEOUT, NOISE_CHUNK_TIMEOUT, NOISE_HANDLER_TIMEOUT, NOISE_REQUEST_TIMEOUT,
    parse_flexible_uri, send_noise_message, send_noise_message_with_chunked_response,
    send_noise_message_with_retry,
};
