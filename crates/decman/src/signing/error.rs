use thiserror::Error;

/// Errors a signing backend can return.
///
/// The typed boundary keeps backend failures distinguishable at the trait
/// surface. Backend internals may still use `anyhow` for one-off failures;
/// those arrive as [`SigningError::Other`].
#[derive(Debug, Error)]
pub enum SigningError {
    /// A Canton admin-API RPC failed (for example `ExportKeyPair`).
    #[error("vault RPC failed: {0}")]
    VaultRpc(#[from] tonic::Status),
    /// The external KMS rejected or failed a signing call. Common causes:
    /// missing `kms:Sign` permission for decman's role on the party key, or
    /// unreachable KMS endpoint.
    #[error("KMS signing failed: {0}")]
    Kms(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// Any other signing failure.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
