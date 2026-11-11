/// Why an add-hosts call could not proceed.
///
/// The tenant endpoints map these onto status codes, and they cannot do that
/// from an opaque `anyhow::Error`: a caller who pinned a serial that has since
/// moved, a caller who submitted bytes this host refuses, and a Canton RPC that
/// fell over are three different answers (409, 400, 500) with three different
/// remedies. Collapsing them into one status sends the wallet to the wrong one.
#[derive(Debug, thiserror::Error)]
pub enum AddHostsError {
    /// This node holds no authorized mapping for the party.
    #[error("{party} has no authorized PartyToParticipant on this node")]
    UnknownParty { party: String },
    /// The pinned base serial disagrees with head state — the party moved under
    /// the request. The caller re-reads and retries.
    #[error(
        "this host reads serial {found} for {party}, not the {pinned} the request pinned; \
         re-read the party and retry"
    )]
    StaleSerial {
        party: String,
        pinned: u32,
        found: u32,
    },
    /// The caller's host set or submitted bytes failed validation.
    #[error("{0:#}")]
    Invalid(#[source] anyhow::Error),
    /// A Canton RPC failed, or this node could not read its own state.
    #[error("{0:#}")]
    Canton(#[source] anyhow::Error),
}
