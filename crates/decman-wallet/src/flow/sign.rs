//! The steps every flow shares between preparing and submitting: checking that
//! the hosts agree, and signing what they prepared.

use super::WalletHost;
use crate::{
    error::{Error, Result},
    signer::Signer,
};

/// Require every other host's preparation to match the reference one.
///
/// `compare(other, reference)` returns why `other` differs, or `None` when it
/// matches. The first host that differs stops the check.
///
/// # Errors
/// [`Error::HostDisagreement`] naming the first host that differs, against
/// `preparer`.
pub(super) fn require_agreement<T>(
    preparer: &WalletHost,
    prepared: &T,
    others: &[(&WalletHost, T)],
    compare: impl Fn(&T, &T) -> Option<String>,
) -> Result<()> {
    for (host, other) in others {
        if let Some(detail) = compare(other, prepared) {
            return Err(Error::HostDisagreement {
                host: host.client.base_url().to_string(),
                reference: preparer.client.base_url().to_string(),
                detail,
            });
        }
    }
    Ok(())
}

/// Sign each of `hashes` with `key`, after checking that there is exactly one
/// hash per transaction.
///
/// # Errors
/// [`Error::MalformedPreparation`] when the counts differ, before anything is
/// signed. Otherwise a hash that is not base64, or a signing failure.
pub(super) async fn sign_prepared(
    preparer: &WalletHost,
    hashes: &[String],
    transactions: &[String],
    key: &dyn Signer,
) -> Result<Vec<String>> {
    if hashes.len() != transactions.len() {
        return Err(Error::MalformedPreparation {
            host: preparer.client.base_url().to_string(),
            detail: format!(
                "{hashes} hash(es) for {txs} transaction(s)",
                hashes = hashes.len(),
                txs = transactions.len()
            ),
        });
    }
    let mut signatures = Vec::with_capacity(hashes.len());
    for encoded in hashes {
        let hash = preparer.client.decode_b64("transaction_hash", encoded)?;
        signatures.push(key.sign_b64_async(&hash).await?);
    }
    Ok(signatures)
}
