use std::borrow::Cow;

use canton_proto_rs::com::digitalasset::canton::crypto::v30::{
    CryptoKeyFormat, SigningKeySpec, SigningPublicKey,
};
use common::fingerprint::fingerprint_from_public_key;
use x509_parser::prelude::{FromDer, SubjectPublicKeyInfo};

pub use common::fingerprint::MULTIHASH_SHA256_PREFIX;

use crate::canton_id::Fingerprint;

/// Compute the fingerprint (hash) of a Canton `SigningPublicKey`.
///
/// Canton uses multihash format: the `"1220"` prefix marks a SHA-256 hash
/// (`0x12`) of 32 bytes (`0x20`), followed by the hex digest of
/// `SHA-256( be32(HashPurpose.PublicKeyFingerprint=12) || data )`.
///
/// `data` depends on `(format, key_spec)`, mirroring Canton's
/// `SigningPublicKey.getDataForFingerprint` + `PublicKey.id`:
///
/// - **Ed25519 in X.509 SPKI form** hashes only the *extracted raw* public
///   key. This is a backward-compat carve-out: Ed25519 keys were once stored
///   raw, and the fingerprint had to stay stable when Canton moved them into
///   SPKI. It applies to Ed25519 **and nothing else**.
/// - **Every other key** — notably EC-P256/P-384, as generated on KMS-backed
///   nodes — hashes the `public_key` bytes verbatim.
///
/// Extracting the SPKI bit-string for a P-256 key yields a fingerprint Canton
/// never computes, which broke namespace delegation on KMS nodes with
/// `TOPOLOGY_NO_APPROPRIATE_SIGNING_KEY_IN_STORE`. See #264.
///
/// The fingerprint serves as the namespace identifier and unique key
/// identifier in Canton.
pub fn compute_fingerprint(key: &SigningPublicKey) -> Fingerprint {
    let is_ed25519_spki = key.format == CryptoKeyFormat::DerX509SubjectPublicKeyInfo as i32
        && key.key_spec == SigningKeySpec::EcCurve25519 as i32;

    let data: Cow<[u8]> = if is_ed25519_spki {
        // Ed25519 only: hash the raw key extracted from the X.509 SPKI.
        match SubjectPublicKeyInfo::from_der(&key.public_key) {
            Ok((_, spki)) => Cow::Owned(spki.subject_public_key.data.to_vec()),
            Err(e) => {
                tracing::warn!("Failed to parse Ed25519 X.509 SPKI: {e}; hashing full key bytes");
                Cow::Borrowed(key.public_key.as_slice())
            }
        }
    } else {
        // P-256/P-384 and the general case: hash the public key bytes as-is.
        Cow::Borrowed(key.public_key.as_slice())
    };

    let fingerprint = fingerprint_from_public_key(&data);
    tracing::debug!("Computed fingerprint: {fingerprint}");

    fingerprint
}

#[cfg(test)]
mod tests {
    use base64::Engine;
    use canton_proto_rs::com::digitalasset::canton::crypto::v30::{
        CryptoKeyFormat, SigningKeySpec,
    };

    use super::*;

    /// Build a `SigningPublicKey` from a base64 DER-SPKI public key, as Canton's
    /// VaultService returns it.
    fn spki_key(der_b64: &str, key_spec: SigningKeySpec) -> SigningPublicKey {
        SigningPublicKey {
            format: CryptoKeyFormat::DerX509SubjectPublicKeyInfo as i32,
            public_key: base64::engine::general_purpose::STANDARD
                .decode(der_b64)
                .unwrap(),
            key_spec: key_spec as i32,
            usage: vec![],
            // deprecated scheme field
            ..Default::default()
        }
    }

    // Real key + fingerprint pairs captured from live Canton participant nodes.
    // Each key is a node's *root* namespace key, so its Canton fingerprint is
    // the node's own namespace (the part after "::" in the participant id) —
    // ground truth we cannot get wrong.

    #[test]
    fn fingerprint_ed25519_matches_canton() {
        // JCE devnet node (iBTC-validator-1). Ed25519 in X.509 SPKI form:
        // Canton hashes the *extracted raw* key (backward-compat carve-out).
        let key = spki_key(
            "MCowBQYDK2VwAyEAdPyxaQ5QEYjsrWOMlkojsDVB7Hop7HkrP0DrJ3P2I38=",
            SigningKeySpec::EcCurve25519,
        );
        assert_eq!(
            compute_fingerprint(&key),
            "1220fa8543db6c66fe3a55b1f180c8dfc7f876265c76684fbc1d35d89e02c8aafe8e"
        );
    }

    #[test]
    fn fingerprint_ec_p256_matches_canton() {
        // AWS-KMS devnet node (bitsafe-validator-3). EC-P256 in X.509 SPKI form:
        // Canton hashes the *full* SPKI bytes, not the extracted EC point.
        // Regression for #264 — extracting the point here produced a fingerprint
        // Canton never computes, breaking the namespace delegation with
        // TOPOLOGY_NO_APPROPRIATE_SIGNING_KEY_IN_STORE.
        let key = spki_key(
            "MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEqMC8MjPyOvneI165hTcATE6Log\
             A1dsdqiY+qXlMFcGWQ2EZp7MMq/c2iSMJCb/IOlaGOw7guEzDK78qxAsB5KQ==",
            SigningKeySpec::EcP256,
        );
        assert_eq!(
            compute_fingerprint(&key),
            "1220d326ba9000791574dcc0047fafa2147d077c4a6072cf640ba7b8ceb301a31129"
        );
    }
}
