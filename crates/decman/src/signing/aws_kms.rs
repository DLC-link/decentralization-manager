//! Signing backend for party keys held in AWS KMS.
//!
//! A KMS-backed participant generates the party's Daml key inside AWS KMS,
//! where the private half is non-exportable. The node cannot sign ledger
//! transactions with it (Canton exposes no vault Sign RPC for Daml hashes),
//! so decman signs by calling the AWS KMS `Sign` API directly with the key id
//! that `VaultService.ListMyKeys` reports for the key.
//!
//! Requirements on the runtime environment:
//! - AWS credentials resolvable through the default provider chain (on EKS:
//!   IRSA on decman's service account).
//! - `kms:Sign` on the party's KMS keys. The keys are created under the
//!   participant's KMS role, so the operator must grant decman's role access
//!   in the key policy. See `docs/KMS_SIGNING.md`.
//!
//! Signature semantics, verified against Canton source
//! (`InteractiveSubmission.verifySignatures` / `JcePureCrypto`):
//! - The prepared-transaction hash is a raw 32-byte digest, signed verbatim
//!   as a *message*: Canton verifies with `SHA256withECDSA`, which hashes the
//!   input again internally. AWS KMS must therefore receive the hash bytes
//!   with `MessageType::Raw` so it applies that same single SHA-256 pass.
//!   Passing them as `Digest` would skip it and fail verification.
//! - Canton accepts ECDSA signatures only in ASN.1/DER
//!   (`SignatureFormat::Der`), which is what AWS KMS returns. No re-encoding.

use async_trait::async_trait;
use aws_sdk_kms::{
    primitives::Blob,
    types::{MessageType, SigningAlgorithmSpec as KmsSigningAlgorithm},
};
use canton_proto_rs::com::digitalasset::canton::crypto::v30::{
    Signature, SignatureFormat, SigningAlgorithmSpec, SigningKeySpec,
};
use p256::{
    ecdsa::{DerSignature, VerifyingKey, signature::Verifier},
    pkcs8::DecodePublicKey,
};

use crate::signing::{PreparedTransactionHash, SigningError, SigningKeyContext, TransactionSigner};

/// Signs with a party key held in AWS KMS via the KMS `Sign` API.
pub struct AwsKmsSigner {
    client: aws_sdk_kms::Client,
}

impl AwsKmsSigner {
    /// Build the signer from the default AWS credential/region chain
    /// (environment, profile, or IRSA when running on EKS).
    pub async fn new() -> Self {
        let config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
        Self {
            client: aws_sdk_kms::Client::new(&config),
        }
    }
}

#[async_trait]
impl TransactionSigner for AwsKmsSigner {
    async fn sign(
        &self,
        hashes: &[PreparedTransactionHash],
        key: &SigningKeyContext,
    ) -> Result<Vec<Signature>, SigningError> {
        let kms_key_id = key.kms_key_id.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "AwsKmsSigner selected for key {fingerprint} but the key carries no kms_key_id",
                fingerprint = key.fingerprint
            )
        })?;
        validate_key_id(kms_key_id)?;

        // Only EC-P256 is supported: it is the spec KMS-backed nodes generate
        // (the KMS provider default), and the only one whose Canton signature
        // rules and local verification this backend implements. Other specs
        // are rejected here rather than signed on unverified assumptions.
        let (kms_algorithm, canton_algorithm) = algorithms_for(key.public_key.key_spec())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "AwsKmsSigner does not support key spec {spec:?} \
                     (key {fingerprint}); only EC-P256 is supported",
                    spec = key.public_key.key_spec(),
                    fingerprint = key.fingerprint
                )
            })?;

        // Local verification key, parsed from the X.509 SPKI public key Canton
        // returned at generation time. Verifying each signature locally before
        // submission catches a wrong key or wrong signing semantics here,
        // instead of as an opaque rejection at ExecuteSubmission.
        let verifying_key =
            VerifyingKey::from_public_key_der(&key.public_key.public_key).map_err(|e| {
                anyhow::anyhow!(
                    "Failed to parse P-256 public key for {fingerprint}: {e}",
                    fingerprint = key.fingerprint
                )
            })?;

        tracing::info!(
            "Signing {count} transaction hashes via AWS KMS key {kms_key_id}...",
            count = hashes.len()
        );

        let mut signatures: Vec<Signature> = Vec::with_capacity(hashes.len());

        for (idx, hash) in hashes.iter().enumerate() {
            // MessageType::Raw: KMS applies the algorithm's digest (SHA-256)
            // to the hash bytes, matching Canton's message-verify semantics.
            let output = self
                .client
                .sign()
                .key_id(kms_key_id)
                .message(Blob::new(hash.as_bytes()))
                .message_type(MessageType::Raw)
                .signing_algorithm(kms_algorithm.clone())
                .send()
                .await
                .map_err(|e| SigningError::Kms(Box::new(e)))?;

            let signature_der = output
                .signature()
                .ok_or_else(|| {
                    anyhow::anyhow!("AWS KMS Sign returned no signature for key {kms_key_id}")
                })?
                .as_ref()
                .to_vec();

            // Verify locally before submission. A signature that fails here is
            // guaranteed to be rejected at ExecuteSubmission, so fail loudly
            // with context instead of producing an opaque ledger error.
            check_kms_signature(&verifying_key, hash.as_bytes(), &signature_der).map_err(
                |failure| match failure {
                    LocalCheck::NotDer(e) => {
                        anyhow::anyhow!("AWS KMS returned a signature that is not valid DER: {e}")
                    }
                    LocalCheck::Mismatch(e) => anyhow::anyhow!(
                        "Signature {index} from KMS key {kms_key_id} failed local \
                         verification against the registered public key \
                         {fingerprint}: {e}",
                        index = idx + 1,
                        fingerprint = key.fingerprint
                    ),
                },
            )?;
            tracing::info!("Signature {index} verified locally", index = idx + 1);

            signatures.push(Signature {
                format: SignatureFormat::Der as i32,
                signature: signature_der,
                signed_by: key.fingerprint.to_string(),
                signing_algorithm_spec: canton_algorithm as i32,
                signature_delegation: None,
            });
        }

        tracing::debug!("Generated {count} signatures", count = signatures.len());
        Ok(signatures)
    }
}

/// Why a KMS signature failed the local check.
#[derive(Debug)]
enum LocalCheck {
    /// The bytes are not a DER-encoded ECDSA signature.
    NotDer(String),
    /// The signature does not verify against the registered key.
    Mismatch(String),
}

/// Checks a DER signature over `message` with the party's P-256 key. Canton
/// checks the same signature at ExecuteSubmission, with SHA-256 over the raw
/// message, which is also what KMS signs with `MessageType::Raw`.
fn check_kms_signature(
    verifying_key: &VerifyingKey,
    message: &[u8],
    signature_der: &[u8],
) -> Result<(), LocalCheck> {
    let signature =
        DerSignature::from_bytes(signature_der).map_err(|e| LocalCheck::NotDer(e.to_string()))?;
    verifying_key
        .verify(message, &signature)
        .map_err(|e| LocalCheck::Mismatch(e.to_string()))
}

/// Map a Canton signing key spec to the AWS KMS signing algorithm and the
/// Canton algorithm spec recorded on the produced signature. Returns `None`
/// for every spec except EC-P256 — the spec KMS-backed nodes generate, and the
/// only one this backend's Canton format rules and local verification cover.
fn algorithms_for(key_spec: SigningKeySpec) -> Option<(KmsSigningAlgorithm, SigningAlgorithmSpec)> {
    match key_spec {
        SigningKeySpec::EcP256 => Some((
            KmsSigningAlgorithm::EcdsaSha256,
            SigningAlgorithmSpec::EcDsaSha256,
        )),
        _ => None,
    }
}

/// Accept immutable, account-local key IDs only. ARNs can select another
/// account, and aliases can be retargeted independently of the vault metadata.
/// AWS documents UUID key IDs and `mrk-` followed by 32 hexadecimal digits:
/// https://docs.aws.amazon.com/kms/latest/developerguide/concepts.html#key-id
fn validate_key_id(key_id: &str) -> anyhow::Result<()> {
    let valid = if let Some(id) = key_id.strip_prefix("mrk-") {
        id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit())
    } else {
        key_id.len() == 36
            && key_id.bytes().enumerate().all(|(index, byte)| {
                if matches!(index, 8 | 13 | 18 | 23) {
                    byte == b'-'
                } else {
                    byte.is_ascii_hexdigit()
                }
            })
    };
    anyhow::ensure!(
        valid,
        "Invalid kms_key_id: expected a bare AWS KMS key ID (UUID or mrk- followed by \
         32 hex digits); ARNs and aliases are not accepted. Use credentials and a region \
         for the account that owns the participant's key"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use common::fingerprint::fingerprint_from_public_key;
    use p256::{
        ecdsa::{Signature as P256Signature, SigningKey, signature::Signer},
        pkcs8::EncodePublicKey,
    };

    use super::*;

    /// A P-256 key that verifies as the registered key would: parsed back from
    /// its X.509 SPKI encoding, the form Canton stores.
    fn key_pair(seed: u8) -> anyhow::Result<(SigningKey, VerifyingKey)> {
        let signing_key = SigningKey::from_slice(&[seed; 32])?;
        let spki = signing_key.verifying_key().to_public_key_der()?;
        Ok((
            signing_key,
            VerifyingKey::from_public_key_der(spki.as_bytes())?,
        ))
    }

    /// Guards the p256 upgrade: a DER signature over a raw hash, as KMS returns
    /// it, must verify, and anything else must be refused.
    #[test]
    fn a_der_signature_over_the_raw_hash_verifies_and_nothing_else_does() -> anyhow::Result<()> {
        let (signing_key, verifying_key) = key_pair(7)?;
        let hash = [42u8; 32];
        let signature: P256Signature = signing_key.sign(&hash);
        let der = signature.to_der();

        assert!(check_kms_signature(&verifying_key, &hash, der.as_bytes()).is_ok());
        assert!(matches!(
            check_kms_signature(&verifying_key, &[43u8; 32], der.as_bytes()),
            Err(LocalCheck::Mismatch(_))
        ));
        let (_, other_key) = key_pair(9)?;
        assert!(matches!(
            check_kms_signature(&other_key, &hash, der.as_bytes()),
            Err(LocalCheck::Mismatch(_))
        ));
        assert!(matches!(
            check_kms_signature(&verifying_key, &hash, b"not der"),
            Err(LocalCheck::NotDer(_))
        ));
        Ok(())
    }

    #[test]
    fn accepts_account_local_key_ids() -> anyhow::Result<()> {
        for id in [
            "1234abcd-12ab-34cd-56ef-1234567890ab",
            "1234ABCD-12AB-34CD-56EF-1234567890AB",
            "mrk-1234abcd12ab34cd56ef1234567890ab",
        ] {
            validate_key_id(id)?;
        }
        Ok(())
    }

    #[test]
    fn rejects_arns_aliases_and_malformed_key_ids() {
        for id in [
            "",
            "arn:aws:kms:us-east-1:111122223333:key/1234abcd-12ab-34cd-56ef-1234567890ab",
            "arn:aws-us-gov:kms:us-gov-west-1:111122223333:key/mrk-1234abcd12ab34cd56ef1234567890ab",
            "arn:aws:kms:us-east-1:111122223333:alias/party",
            "alias/party",
            "1234abcd12ab34cd56ef1234567890ab",
            "1234abcd_12ab-34cd-56ef-1234567890ab",
            "1234abcd-12ab-34cd-56ef-1234567890ag",
            " 1234abcd-12ab-34cd-56ef-1234567890ab",
            "1234abcd-12ab-34cd-56ef-1234567890ab\n",
            "mrk-1234abcd12ab34cd56ef1234567890a",
            "mrk-1234abcd12ab34cd56ef1234567890abc",
            "mrk-1234abcd12ab34cd56ef1234567890ag",
            "mrk-1234abcd-12ab-34cd-56ef-1234567890ab",
            "mrk-あいうえおかきくけこab",
        ] {
            assert!(validate_key_id(id).is_err(), "accepted {id:?}");
        }
    }

    #[tokio::test]
    async fn rejects_invalid_metadata_before_signing() -> anyhow::Result<()> {
        // No credentials or endpoint: reaching the SDK would fail differently.
        let config = aws_sdk_kms::Config::builder()
            .behavior_version_latest()
            .region(aws_sdk_kms::config::Region::new("us-east-1"))
            .build();
        let signer = AwsKmsSigner {
            client: aws_sdk_kms::Client::from_conf(config),
        };
        let key = SigningKeyContext {
            fingerprint: fingerprint_from_public_key(&[0; 32]),
            public_key: Default::default(),
            kms_key_id: Some("arn:aws:kms:us-east-1:111122223333:key/foreign".into()),
        };
        let result = signer
            .sign(&[PreparedTransactionHash::new(vec![0; 32])], &key)
            .await;
        let error = result
            .err()
            .ok_or_else(|| anyhow::anyhow!("invalid metadata was accepted"))?;
        assert!(error.to_string().contains("Invalid kms_key_id"), "{error}");
        Ok(())
    }

    #[test]
    fn p256_maps_to_ecdsa_sha256() {
        let (kms, canton) = algorithms_for(SigningKeySpec::EcP256).unwrap();
        assert_eq!(kms, KmsSigningAlgorithm::EcdsaSha256);
        assert_eq!(canton, SigningAlgorithmSpec::EcDsaSha256);
    }

    #[test]
    fn other_specs_are_unsupported() {
        assert!(algorithms_for(SigningKeySpec::EcCurve25519).is_none());
        assert!(algorithms_for(SigningKeySpec::EcP384).is_none());
        assert!(algorithms_for(SigningKeySpec::EcSecp256k1).is_none());
        assert!(algorithms_for(SigningKeySpec::Unspecified).is_none());
    }
}
