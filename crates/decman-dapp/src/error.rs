/// Why a proposal or a signature failed a check. The messages appear in the
/// dApp as they are, so they address the person who fills in the form.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("The party name must not be empty.")]
    EmptyHint,
    #[error("The party name must not be longer than {max} characters.")]
    HintTooLong { max: usize },
    #[error("The party name may only use letters, digits, spaces, '-', '_' and ':'.")]
    InvalidHintCharacter,
    #[error("The party name must not contain '::'.")]
    HintContainsDelimiter,
    #[error("A party needs at least {min} owners. This proposal has {got}.")]
    TooFewOwners { min: usize, got: usize },
    #[error("Owner {number} has an invalid Ed25519 public key.")]
    InvalidOwnerKey { number: usize },
    #[error("Owner {number} has no participant.")]
    MissingParticipant { number: usize },
    #[error("Two owners use the same key.")]
    DuplicateOwner,
    #[error("The {kind} threshold must be between 1 and {max}. It is {got}.")]
    ThresholdOutOfRange {
        kind: &'static str,
        max: usize,
        got: u32,
    },
    #[error("The public key is not a valid Ed25519 key.")]
    InvalidPublicKey,
    #[error("The digest is not valid hex.")]
    InvalidDigest,
    #[error("The signature is not 64 bytes of hex.")]
    InvalidSignature,
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
