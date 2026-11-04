//! Errors surfaced by the tenant, ledger and registry clients.

use thiserror::Error;

/// Anything that can go wrong talking to a DecMan host's tenant API.
///
/// Every variant carries the host it applies to: a wallet drives N hosts and
/// needs to report "P2 rejected the bundle" without losing which host that was.
#[derive(Debug, Error)]
pub enum Error {
    #[error("{host}: could not build an HTTP client: {source}")]
    Client {
        host: String,
        #[source]
        source: reqwest::Error,
    },

    #[error("{host}: {method} {path} did not complete: {source}")]
    Transport {
        host: String,
        method: &'static str,
        path: String,
        #[source]
        source: reqwest::Error,
    },

    #[error("{host}: {method} {path} returned {status}: {message}")]
    Api {
        host: String,
        method: &'static str,
        path: String,
        status: u16,
        message: String,
    },

    #[error("{host}: {method} {path} returned a body this client cannot read: {source}")]
    Decode {
        host: String,
        method: &'static str,
        path: String,
        #[source]
        source: reqwest::Error,
    },

    #[error("{host} returned a {field} that is not valid base64: {source}")]
    Base64 {
        host: String,
        field: &'static str,
        #[source]
        source: base64::DecodeError,
    },

    /// The preparing host derived a different party id than the wallet's own key
    /// does. Signing anyway would bind the wallet to a party it cannot control,
    /// so the flow stops before it signs.
    #[error(
        "{host} prepared party id {returned}, but this key derives {expected} — \
         refusing to sign a topology for a party we do not own"
    )]
    PartyIdMismatch {
        host: String,
        expected: String,
        returned: String,
    },

    /// The preparing host returned a topology this client cannot sign — e.g. a
    /// different number of hashes than transactions, so it is unclear which hash
    /// authorizes which transaction.
    #[error("{host} returned a topology this client cannot sign: {detail}")]
    MalformedPreparation { host: String, detail: String },

    /// Two hosts prepared different topology for the same party. One of them is
    /// wrong or lying, and the wallet cannot tell which, so it signs neither.
    ///
    /// This is the check that makes the sovereignty claim hold against a *host*
    /// rather than only against an outsider: the wallet signs a hash it cannot
    /// itself parse, so its only defence against a host that returns the hash of a
    /// mapping it never showed us is that the other hosts independently produced
    /// exactly the same bytes.
    #[error(
        "{host} prepared different topology than {reference} for the same party ({detail}) — \
         one of them is wrong, so refusing to sign either"
    )]
    HostDisagreement {
        host: String,
        reference: String,
        detail: String,
    },

    #[error("a co-validated party needs at least two hosts, got {0}")]
    NotEnoughHosts(usize),

    /// The key store backing a [`Signer`](crate::Signer) could not sign.
    ///
    /// Its own variant because the rest of this enum describes talking to a
    /// host, and a KMS or HSM failure is not that. Shoehorning it into an
    /// HTTP-shaped variant would put a misleading message in front of whoever
    /// has to debug it.
    #[error("the key store could not sign: {message}")]
    Signing { message: String },

    /// A flow was given no hosts to act on.
    ///
    /// Distinct from [`NotEnoughHosts`](Self::NotEnoughHosts), which is about
    /// onboarding's two-host minimum. Adding hosts needs at least one joiner and
    /// at least one host that already holds the party, and neither is a
    /// "co-validated party" requirement.
    #[error("{operation} needs at least one {role}, and none was given")]
    NoHosts {
        operation: &'static str,
        role: &'static str,
    },

    /// The sender owns no unlocked holding of the instrument, so a send has
    /// nothing to spend. Nothing was asked of the registry or the ledger.
    #[error("{party} holds no unlocked {instrument_id} from {instrument_admin}")]
    NoHoldings {
        party: String,
        instrument_admin: String,
        instrument_id: String,
    },

    /// The prepared transaction failed the check the wallet runs before it
    /// signs. Nothing was signed and nothing was executed.
    #[error("{host} prepared a transaction this wallet will not sign: {reason}")]
    TransactionRefused { host: String, reason: Refusal },
}

/// Why the wallet refused to sign a prepared transaction.
#[derive(Debug, Error)]
pub enum Refusal {
    /// The wallet could not recompute the hash, so it cannot know what the
    /// hash commits to. An unknown hashing scheme lands here.
    #[error("its hash cannot be recomputed: {0}")]
    Unverifiable(String),

    /// The hash that came with the transaction is not the transaction's hash.
    #[error("it came with hash {supplied}, but it hashes to {recomputed}")]
    HashMismatch {
        supplied: String,
        recomputed: String,
    },

    /// The transaction is not one exercise at its root, or the exercise's
    /// argument lacks a field the check reads.
    #[error("{0}")]
    Shape(String),

    /// The root exercise differs from what the wallet asked for.
    #[error("its {field} is {found}, not {expected}")]
    Mismatch {
        field: &'static str,
        expected: String,
        found: String,
    },
}

impl Error {
    /// The host this error came from.
    pub fn host(&self) -> &str {
        match self {
            Self::Client { host, .. }
            | Self::Transport { host, .. }
            | Self::Api { host, .. }
            | Self::Decode { host, .. }
            | Self::Base64 { host, .. }
            | Self::PartyIdMismatch { host, .. }
            | Self::MalformedPreparation { host, .. }
            | Self::HostDisagreement { host, .. }
            | Self::TransactionRefused { host, .. } => host,
            // Not about any one host: a signing failure is local, a missing
            // host set has nobody to name, and an empty wallet is the
            // wallet's own state.
            Self::NotEnoughHosts(_)
            | Self::Signing { .. }
            | Self::NoHosts { .. }
            | Self::NoHoldings { .. } => "",
        }
    }

    /// Whether this is an HTTP error with the given status.
    pub fn is_status(&self, wanted: u16) -> bool {
        matches!(self, Self::Api { status, .. } if *status == wanted)
    }
}

pub type Result<T> = std::result::Result<T, Error>;
