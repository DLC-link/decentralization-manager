/// Why an unhost was refused. Each variant maps to the status code that tells
/// the caller what to do next.
#[derive(Debug, thiserror::Error)]
pub enum UnhostError {
    /// This node holds no authorized mapping that names it (404).
    #[error("This host does not host external party {party}")]
    NotHosted { party: String },
    /// The pinned serial disagrees with head state (409).
    #[error(
        "this host reads serial {found} for {party}, not the {pinned} the request pinned; \
         re-read the party and retry"
    )]
    StaleSerial {
        party: String,
        pinned: u32,
        found: u32,
    },
    /// The departure would leave the party unable to transact (400).
    #[error("{0}")]
    Refused(String),
    /// Something already running on this node blocks the departure (409).
    #[error("{0}")]
    Busy(String),
    /// The node is not set up for the Splice cleanup the request asked for (422).
    #[error("{0}")]
    NotConfigured(String),
    /// A read failed before anything was written (500).
    #[error("{0:#}")]
    Internal(#[source] anyhow::Error),
}

impl From<anyhow::Error> for UnhostError {
    /// Keeps a [`UnhostError`] that travelled inside an `anyhow::Error` as
    /// itself, so a 404 does not turn into a 500 on the way out.
    fn from(error: anyhow::Error) -> Self {
        match error.downcast::<UnhostError>() {
            Ok(unhost) => unhost,
            Err(other) => Self::Internal(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::external_party::unhost::test_support::*;

    /// A 404 that travelled through `anyhow` must still be a 404.
    #[test]
    fn a_not_hosted_error_survives_anyhow() {
        let wrapped = anyhow::Error::new(UnhostError::NotHosted {
            party: PARTY.to_string(),
        });
        assert!(matches!(
            UnhostError::from(wrapped),
            UnhostError::NotHosted { .. }
        ));
        assert!(matches!(
            UnhostError::from(anyhow::anyhow!("rpc failed")),
            UnhostError::Internal(_)
        ));
    }
}
