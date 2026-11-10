//! The one error type the hash module returns, and the macros that build it.

use thiserror::Error;

/// Why a prepared transaction could not be hashed, or did not verify.
///
/// The message says what was refused. A caller that signs only after a
/// successful check needs no finer detail than that.
#[derive(Debug, Error)]
#[error("{0}")]
pub struct HashError(String);

impl HashError {
    pub(crate) fn new(message: String) -> Self {
        Self(message)
    }
}

pub type Result<T = (), E = HashError> = std::result::Result<T, E>;

/// Build a [`HashError`] from a format string.
macro_rules! hash_error {
    ($($arg:tt)*) => {
        $crate::canton_hash::HashError::new(format!($($arg)*))
    };
}

/// Return early with a [`HashError`] built from a format string.
macro_rules! bail {
    ($($arg:tt)*) => {
        return Err(hash_error!($($arg)*))
    };
}

/// Return early with a [`HashError`] unless the condition holds.
macro_rules! ensure {
    ($condition:expr, $($arg:tt)*) => {
        if !$condition {
            bail!($($arg)*);
        }
    };
}
