use thiserror::Error as ThisError;

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
    #[error("{0}")]
    Decode(String),
}
