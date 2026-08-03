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
    /// Any other signing failure.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
