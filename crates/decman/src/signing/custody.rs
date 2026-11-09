//! Where a new party Daml key may live.
//!
//! Canton 3.7 refuses to export a key from its software vault unless the node
//! sets `enable-key-download`, and Canton's configuration reference says that
//! setting "should not be enabled in production". A key in AWS KMS needs no
//! export, because DecMan signs with the KMS `Sign` API. DecMan therefore
//! puts a new party's keys only into a vault that keeps its keys in KMS.
//!
//! Insecure mode may still use the software vault. It runs only on devnet, in
//! `cargo test`, and in `test-mode` builds, so localnet keeps working.
//!
//! A vault that only wraps its software keys with a KMS key (`wrapper_key_id`)
//! does not count. Those keys still need an export to sign.

use canton_proto_rs::com::digitalasset::canton::crypto::admin::v30::{
    ListKeysFilters, ListMyKeysRequest, PrivateKeyMetadata,
    vault_service_client::VaultServiceClient,
};
use tonic::transport::Channel;

use crate::{canton_id::Fingerprint, config::NodeConfig, error::Result};

/// Whether this node may put a party Daml key into a software vault.
pub fn software_keys_allowed(config: &NodeConfig) -> bool {
    config.insecure || cfg!(any(test, feature = "test-mode"))
}

/// Refuse to create party keys on a participant that keeps no key in KMS.
///
/// Call this before any key is generated, so a refused run leaves nothing
/// behind in the vault or the topology.
///
/// # Errors
///
/// Errors when the vault cannot be listed, or when it holds no KMS key and
/// this node is not in insecure mode.
pub async fn require_kms_participant(
    config: &NodeConfig,
    vault_client: &mut VaultServiceClient<Channel>,
) -> Result {
    let metadata = list_keys(vault_client, None).await?;
    check(
        metadata.iter().any(in_kms),
        software_keys_allowed(config),
        "This participant keeps its keys in Canton's software vault, not in a KMS",
    )
}

/// Refuse a party key that does not live in KMS.
///
/// A key found by name can predate the participant's move to KMS, so the key
/// itself is checked as well as the participant. Only the Daml key needs an
/// export to sign, but the namespace key gets the same rule, so a party's
/// keys live in one place.
///
/// # Errors
///
/// Errors when the vault cannot be listed, does not hold the key, or holds it
/// outside KMS while this node is not in insecure mode.
pub async fn require_kms_key(
    config: &NodeConfig,
    vault_client: &mut VaultServiceClient<Channel>,
    fingerprint: &Fingerprint,
) -> Result {
    let metadata = list_keys(vault_client, Some(fingerprint)).await?;
    anyhow::ensure!(
        !metadata.is_empty(),
        "The vault does not hold the key {fingerprint} it just returned"
    );
    check(
        metadata.iter().all(in_kms),
        software_keys_allowed(config),
        &format!("The party key {fingerprint} is in Canton's software vault, not in a KMS"),
    )
}

fn in_kms(metadata: &PrivateKeyMetadata) -> bool {
    metadata
        .kms_key_id
        .as_deref()
        .is_some_and(|id| !id.is_empty())
}

fn check(in_kms: bool, software_allowed: bool, problem: &str) -> Result {
    if in_kms {
        return Ok(());
    }
    if software_allowed {
        tracing::warn!(
            "{problem}. Insecure mode allows it. Its first deploy on Splice 0.10 needs \
             canton.parameters.enable-key-download"
        );
        return Ok(());
    }
    anyhow::bail!(
        "{problem}. DecMan creates party keys only on a KMS-backed participant, because \
         Canton 3.7 refuses to export a software key. Set the participant's crypto provider \
         to kms (canton.participants.<name>.crypto.provider = kms), or use DECPM_INSECURE \
         on devnet for local tests"
    )
}

async fn list_keys(
    vault_client: &mut VaultServiceClient<Channel>,
    fingerprint: Option<&Fingerprint>,
) -> Result<Vec<PrivateKeyMetadata>> {
    Ok(vault_client
        .list_my_keys(tonic::Request::new(ListMyKeysRequest {
            filters: Some(ListKeysFilters {
                fingerprint: fingerprint.map(ToString::to_string).unwrap_or_default(),
                name: String::new(),
                purpose: vec![],
                usage_v30: vec![],
            }),
            base_request: None,
        }))
        .await?
        .into_inner()
        .private_keys_metadata)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata(kms_key_id: Option<&str>, wrapper_key_id: Option<&str>) -> PrivateKeyMetadata {
        PrivateKeyMetadata {
            public_key_with_name: None,
            wrapper_key_id: wrapper_key_id.map(str::to_string),
            kms_key_id: kms_key_id.map(str::to_string),
        }
    }

    #[test]
    fn a_kms_key_counts_and_a_wrapped_software_key_does_not() {
        assert!(in_kms(&metadata(Some("mrk-0123"), None)));
        assert!(!in_kms(&metadata(None, Some("wrapper"))));
        assert!(!in_kms(&metadata(Some(""), None)));
        assert!(!in_kms(&metadata(None, None)));
    }

    #[test]
    fn a_software_key_is_refused_outside_insecure_mode() {
        let error = check(false, false, "Software vault")
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(error.contains("Software vault"), "{error}");
        assert!(error.contains("crypto.provider = kms"), "{error}");
    }

    #[test]
    fn insecure_mode_accepts_a_software_key() -> Result {
        check(false, true, "Software vault")
    }

    #[test]
    fn a_kms_key_is_accepted_in_every_mode() -> Result {
        check(true, false, "unused")?;
        check(true, true, "unused")
    }
}
