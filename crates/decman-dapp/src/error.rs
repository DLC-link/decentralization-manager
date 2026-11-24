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
    #[error("A party can have at most {max} owners. This proposal has {got}.")]
    TooManyOwners { max: usize, got: usize },
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
    #[error("A signature comes from key {public_key}, which is not an owner.")]
    SignerNotOwner { public_key: String },
    #[error("Owner {namespace} signed more than once.")]
    DuplicateSignature { namespace: String },
    #[error("The signature of owner {namespace} does not match these transactions.")]
    OwnerSignatureMismatch { namespace: String },
    #[error("Owner {namespace} has not signed yet. Every owner must sign to create the party.")]
    MissingOwnerSignature { namespace: String },
    #[error("The synchronizer id is empty.")]
    MissingSynchronizer,
    #[error("The participant uses topology format {0}. The dApp supports formats 30 and 31.")]
    UnsupportedTopologyFormat(String),
    #[error("The participant's answer has no topology transactions to read the format from.")]
    NoTopologyToProbe,
    #[error("The participant's topology transactions use more than one format.")]
    MixedTopologyFormats,
    #[error("The participant's answer is not valid: {0}")]
    InvalidParticipantAnswer(String),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
