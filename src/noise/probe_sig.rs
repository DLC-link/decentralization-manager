use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use secp256k1::{Message, Secp256k1, ecdsa::Signature};
use sha2::{Digest, Sha256};

use crate::noise::NoiseKeypair;

/// Build the canonical bytes the peer signs and the coordinator verifies.
/// Format (no separators, exact concatenation):
///   peer_pubkey_hex_lowercase   (66 hex chars — secp256k1 compressed pubkey)
///   coordinator_pubkey_hex_lc   (66 hex chars)
///   kind_pascalcase             ("Onboarding" | "Kick" | "Contracts" | "Dars")
///   ts_decimal                  (unix seconds, ASCII, no leading zeros)
fn canonical_bytes(
    peer_pub: &secp256k1::PublicKey,
    coord_pub: &secp256k1::PublicKey,
    kind: &str,
    ts: u64,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(160);
    out.extend_from_slice(hex::encode(peer_pub.serialize()).as_bytes());
    out.extend_from_slice(hex::encode(coord_pub.serialize()).as_bytes());
    out.extend_from_slice(kind.as_bytes());
    out.extend_from_slice(ts.to_string().as_bytes());
    out
}

fn sha256_digest(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().into()
}

/// Sign a probe request. `kind` is the PascalCase workflow kind name —
/// matches `src/server/types.rs` serde repr.
pub fn sign_probe(
    keypair: &NoiseKeypair,
    coord_pub: &secp256k1::PublicKey,
    kind: &str,
    ts: u64,
) -> Vec<u8> {
    let secp = Secp256k1::signing_only();
    let bytes = canonical_bytes(&keypair.public_key, coord_pub, kind, ts);
    let digest = sha256_digest(&bytes);
    let msg = Message::from_digest(digest);
    let sig = secp.sign_ecdsa(&msg, &keypair.secret_key_ref().to_secp_secret_key());
    sig.serialize_compact().to_vec()
}

/// Verify a probe request. Returns Err on any mismatch (bad signature,
/// stale timestamp, malformed input).
#[allow(clippy::too_many_arguments)]
pub fn verify_probe(
    peer_pub: &secp256k1::PublicKey,
    coord_pub: &secp256k1::PublicKey,
    kind: &str,
    ts: u64,
    sig_bytes: &[u8],
    now: u64,
    tolerance: Duration,
) -> Result<()> {
    let age = now.saturating_sub(ts);
    if age > tolerance.as_secs() {
        return Err(anyhow!(
            "probe timestamp too old: age={age}s, tolerance={}s",
            tolerance.as_secs()
        ));
    }
    let secp = Secp256k1::verification_only();
    let bytes = canonical_bytes(peer_pub, coord_pub, kind, ts);
    let digest = sha256_digest(&bytes);
    let msg = Message::from_digest(digest);
    let sig = Signature::from_compact(sig_bytes).context("malformed probe signature")?;
    secp.verify_ecdsa(&msg, &sig, peer_pub)
        .context("probe signature verification failed")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts_now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    #[test]
    fn sign_then_verify_roundtrip() {
        let peer = NoiseKeypair::generate();
        let coord = NoiseKeypair::generate();
        let ts = ts_now();
        let sig = sign_probe(&peer, &coord.public_key, "Onboarding", ts);
        verify_probe(
            &peer.public_key,
            &coord.public_key,
            "Onboarding",
            ts,
            &sig,
            ts,
            Duration::from_secs(30),
        )
        .expect("fresh signature must verify");
    }

    #[test]
    fn stale_timestamp_rejected() {
        let peer = NoiseKeypair::generate();
        let coord = NoiseKeypair::generate();
        let ts = ts_now();
        let sig = sign_probe(&peer, &coord.public_key, "Onboarding", ts);
        let result = verify_probe(
            &peer.public_key,
            &coord.public_key,
            "Onboarding",
            ts,
            &sig,
            ts + 60,
            Duration::from_secs(30),
        );
        assert!(result.is_err(), "60s-old signature must be rejected");
    }

    #[test]
    fn wrong_peer_pubkey_rejected() {
        let peer = NoiseKeypair::generate();
        let imposter = NoiseKeypair::generate();
        let coord = NoiseKeypair::generate();
        let ts = ts_now();
        let sig = sign_probe(&peer, &coord.public_key, "Onboarding", ts);
        let result = verify_probe(
            &imposter.public_key,
            &coord.public_key,
            "Onboarding",
            ts,
            &sig,
            ts,
            Duration::from_secs(30),
        );
        assert!(result.is_err());
    }

    #[test]
    fn wrong_coordinator_pubkey_in_signed_bytes_rejected() {
        let peer = NoiseKeypair::generate();
        let coord_a = NoiseKeypair::generate();
        let coord_b = NoiseKeypair::generate();
        let ts = ts_now();
        let sig = sign_probe(&peer, &coord_a.public_key, "Onboarding", ts);
        let result = verify_probe(
            &peer.public_key,
            &coord_b.public_key,
            "Onboarding",
            ts,
            &sig,
            ts,
            Duration::from_secs(30),
        );
        assert!(result.is_err());
    }

    #[test]
    fn tampered_kind_rejected() {
        let peer = NoiseKeypair::generate();
        let coord = NoiseKeypair::generate();
        let ts = ts_now();
        let sig = sign_probe(&peer, &coord.public_key, "Onboarding", ts);
        let result = verify_probe(
            &peer.public_key,
            &coord.public_key,
            "Kick",
            ts,
            &sig,
            ts,
            Duration::from_secs(30),
        );
        assert!(result.is_err());
    }
}
