use std::error::Error as StdError;

use thiserror::Error as ThisError;

/// The cause a [`Error::Decode`] carries. The causes come from several
/// crates, so the type is boxed: `anyhow::Error` from a [`CantonId`] parse,
/// `DamlDecimalError` from the decimal parser, and [`Error`] itself when one
/// decode wraps another.
///
/// [`CantonId`]: common::canton_id::CantonId
pub type DecodeSource = Box<dyn StdError + Send + Sync + 'static>;

/// Errors the pure codec layer produces. No transport errors live here —
/// the lib does no I/O.
#[derive(Debug, ThisError)]
pub enum Error {
    /// A payload failed a protocol-constraint check. The message is the
    /// exact text decman returns in HTTP 400 bodies — keep it byte-stable.
    #[error("{0}")]
    Validation(String),
    /// A required package ref is absent from the `PackageConfig`. The
    /// message matches decman's previous `.context(...)` strings.
    #[error("{0} package not configured")]
    PackageNotConfigured(&'static str),
    /// A payload cannot encode to a Daml proto `Value`.
    #[error("{0}")]
    Encode(String),
    /// A Daml proto `Value` does not decode into the expected shape.
    ///
    /// `source` holds the error that caused the failure, when one exists, so
    /// [`StdError::source`] walks the whole chain. A shape mismatch has no
    /// deeper cause and leaves it `None`. Build this variant with
    /// [`Error::decode`] or [`Error::decode_from`] rather than by hand.
    #[error("{message}")]
    Decode {
        message: String,
        #[source]
        source: Option<DecodeSource>,
    },
}

impl Error {
    /// A decode failure with no deeper cause, such as a shape mismatch.
    pub fn decode(message: impl Into<String>) -> Self {
        Self::Decode {
            message: message.into(),
            source: None,
        }
    }

    /// A decode failure that keeps the error which caused it. The message
    /// says which field failed; the source says why.
    pub fn decode_from(message: impl Into<String>, source: impl Into<DecodeSource>) -> Self {
        Self::Decode {
            message: message.into(),
            source: Some(source.into()),
        }
    }
}
