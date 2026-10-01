//! Canton namespace-fingerprint derivation for external-party keys, and the
//! signature scheme a party's key implies.
//!
//! The derivation itself lives in [`common::fingerprint`] so the wallet-side
//! client (`decman-wallet`) computes party ids exactly the way this node
//! validates them. Key generation and signing live entirely on the client — a
//! production binary cannot make or hold a party key, so nothing of the sort
//! appears here.

use canton_proto_rs::com::digitalasset::canton::{
    crypto::v30::{Signature, SignatureFormat, SigningAlgorithmSpec, SigningKeySpec},
    protocol::v30::PartyToParticipant,
};

use crate::utils::compute_fingerprint;

pub use common::fingerprint::fingerprint_from_public_key;

/// Length of an Ed25519 signature, the only form Canton takes for that key.
const ED25519_SIGNATURE_LEN: usize = 64;

/// How Canton verifies a signature made with a party's key: the algorithm, and
/// the wire format the bytes must take.
///
/// Canton refuses a signature whose algorithm does not match the key's spec, so
/// an Ed25519 label on a secp256k1 signature fails before authorization is even
/// considered. The spec is read off the party's authorized mapping, never off
/// the request: the request is the untrusted side, and the mapping is what
/// Canton verifies against anyway.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PartySignatureScheme {
    algorithm: SigningAlgorithmSpec,
    /// Length of a fixed-width `r || s` ECDSA signature for this key, which the
    /// wallet may send instead of DER. `None` for Ed25519, whose 64 bytes go to
    /// Canton as they are.
    concat_len: Option<usize>,
}

impl PartySignatureScheme {
    const ED25519: Self = Self {
        algorithm: SigningAlgorithmSpec::Ed25519,
        concat_len: None,
    };

    /// The scheme for a key of `spec`, or `None` when Canton has no signing
    /// algorithm for it.
    #[must_use]
    pub fn for_key_spec(spec: SigningKeySpec) -> Option<Self> {
        match spec {
            SigningKeySpec::EcCurve25519 => Some(Self::ED25519),
            SigningKeySpec::EcP256 | SigningKeySpec::EcSecp256k1 => Some(Self {
                algorithm: SigningAlgorithmSpec::EcDsaSha256,
                concat_len: Some(64),
            }),
            SigningKeySpec::EcP384 => Some(Self {
                algorithm: SigningAlgorithmSpec::EcDsaSha384,
                concat_len: Some(96),
            }),
            SigningKeySpec::Unspecified | SigningKeySpec::MlDsa65 => None,
        }
    }

    /// The scheme for the party key whose fingerprint is `signed_by` on
    /// `mapping`.
    ///
    /// A mapping that records no key for `signed_by` gets the Ed25519 default
    /// this path always used, so a namespace key that is not among the party's
    /// signing keys behaves as before. Canton still has the last word on it.
    #[must_use]
    pub fn for_party(mapping: &PartyToParticipant, signed_by: &str) -> Self {
        let found = mapping
            .party_signing_keys
            .iter()
            .flat_map(|keys| keys.keys.iter())
            .find(|key| compute_fingerprint(key) == signed_by)
            .map(|key| {
                SigningKeySpec::try_from(key.key_spec)
                    .ok()
                    .and_then(Self::for_key_spec)
            });
        match found {
            Some(Some(scheme)) => scheme,
            Some(None) => {
                tracing::warn!(
                    party = %mapping.party,
                    signed_by,
                    "party key has a spec Canton cannot sign with; sending the signature as Ed25519"
                );
                Self::ED25519
            }
            None => {
                tracing::warn!(
                    party = %mapping.party,
                    signed_by,
                    "signed_by matches none of the party's signing keys; sending the signature as Ed25519"
                );
                Self::ED25519
            }
        }
    }

    /// Whether `signature` has a form this scheme can forward to Canton.
    ///
    /// Ed25519 takes exactly 64 bytes. ECDSA takes either the fixed-width
    /// `r || s` pair or a DER `SEQUENCE` of two `INTEGER`s. Checking here turns
    /// a truncated or mis-encoded signature into a 400 instead of a Canton RPC
    /// failure that would surface as a 500.
    #[must_use]
    pub fn accepts(self, signature: &[u8]) -> bool {
        match self.concat_len {
            None => signature.len() == ED25519_SIGNATURE_LEN,
            Some(len) => signature.len() == len || is_der_ecdsa_signature(signature),
        }
    }

    /// The forms [`Self::accepts`] takes, for an error message.
    #[must_use]
    pub fn expected_form(self) -> String {
        match self.concat_len {
            None => format!("an Ed25519 signature is always {ED25519_SIGNATURE_LEN} bytes"),
            Some(len) => format!(
                "an ECDSA signature is the {len}-byte r || s pair or a DER SEQUENCE of two INTEGERs"
            ),
        }
    }

    /// The Canton signature for `signature` as the wallet sent it.
    ///
    /// An ECDSA signature that is already DER passes through. One that arrives
    /// as the fixed-width `r || s` pair is re-encoded as DER, the only ECDSA
    /// format Canton accepts. DER is detected by structure, not by length: a
    /// DER signature whose integers lost leading zero octets can be exactly as
    /// long as the pair. Ed25519 bytes go as they are.
    #[must_use]
    pub fn canton_signature(self, signature: &[u8], signed_by: &str) -> Signature {
        let (format, bytes) = match self.concat_len {
            None => (SignatureFormat::Concat, signature.to_vec()),
            Some(_) if is_der_ecdsa_signature(signature) => {
                (SignatureFormat::Der, signature.to_vec())
            }
            Some(len) if signature.len() == len => {
                (SignatureFormat::Der, ecdsa_concat_to_der(signature))
            }
            Some(_) => (SignatureFormat::Der, signature.to_vec()),
        };
        Signature {
            format: format as i32,
            signature: bytes,
            signed_by: signed_by.to_string(),
            signing_algorithm_spec: self.algorithm as i32,
            signature_delegation: None,
        }
    }
}

/// Whether `bytes` are exactly one DER `SEQUENCE { INTEGER r, INTEGER s }`
/// in short-form lengths, which covers every curve Canton signs with, through P-384.
fn is_der_ecdsa_signature(bytes: &[u8]) -> bool {
    let [0x30, seq_len, body @ ..] = bytes else {
        return false;
    };
    if *seq_len >= 0x80 || usize::from(*seq_len) != body.len() {
        return false;
    }
    let Some(rest) = der_integer_tail(body) else {
        return false;
    };
    der_integer_tail(rest).is_some_and(<[u8]>::is_empty)
}

/// The bytes after one short-form DER `INTEGER` at the start of `bytes`, or
/// `None` when there is no well-formed, non-empty INTEGER there.
fn der_integer_tail(bytes: &[u8]) -> Option<&[u8]> {
    let [0x02, len, rest @ ..] = bytes else {
        return None;
    };
    if *len == 0 || *len >= 0x80 || rest.len() < usize::from(*len) {
        return None;
    }
    Some(&rest[usize::from(*len)..])
}

/// DER `SEQUENCE { INTEGER r, INTEGER s }` for a fixed-width big-endian
/// `r || s` pair (RFC 3279 §2.2.3).
fn ecdsa_concat_to_der(concat: &[u8]) -> Vec<u8> {
    let (r, s) = concat.split_at(concat.len() / 2);
    let body = [der_integer(r), der_integer(s)].concat();
    let mut out = vec![0x30];
    der_length(&mut out, body.len());
    out.extend(body);
    out
}

/// A DER INTEGER for an unsigned big-endian number: leading zeros dropped, a
/// zero byte prepended when the top bit would read as a sign.
fn der_integer(unsigned: &[u8]) -> Vec<u8> {
    const ZERO: [u8; 1] = [0];
    let magnitude = match unsigned.iter().position(|b| *b != 0) {
        Some(first) => &unsigned[first..],
        None => &ZERO[..],
    };
    let mut out = vec![0x02];
    let pad = usize::from(magnitude.first().is_some_and(|b| b & 0x80 != 0));
    der_length(&mut out, magnitude.len() + pad);
    if pad == 1 {
        out.push(0x00);
    }
    out.extend_from_slice(magnitude);
    out
}

fn der_length(out: &mut Vec<u8>, len: usize) {
    if len < 0x80 {
        out.push(len as u8);
    } else {
        let bytes = len.to_be_bytes();
        let first = bytes
            .iter()
            .position(|b| *b != 0)
            .unwrap_or(bytes.len() - 1);
        out.push(0x80 | (bytes.len() - first) as u8);
        out.extend_from_slice(&bytes[first..]);
    }
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::digitalasset::canton::crypto::v30::{
        CryptoKeyFormat, SigningKeySpec, SigningKeyUsage, SigningKeysWithThreshold,
        SigningPublicKey,
    };

    use super::*;

    fn key(spec: SigningKeySpec, tag: u8) -> SigningPublicKey {
        SigningPublicKey {
            format: CryptoKeyFormat::DerX509SubjectPublicKeyInfo as i32,
            public_key: vec![tag; 88],
            key_spec: spec as i32,
            usage: vec![
                SigningKeyUsage::Namespace as i32,
                SigningKeyUsage::Protocol as i32,
            ],
            ..Default::default()
        }
    }

    fn mapping(keys: Vec<SigningPublicKey>) -> PartyToParticipant {
        PartyToParticipant {
            party: "alice::1220aa".to_string(),
            threshold: 1,
            participants: vec![],
            party_signing_keys: Some(SigningKeysWithThreshold { keys, threshold: 1 }),
        }
    }

    /// A well-formed 71-byte DER signature: a 33-byte r (sign-padded) and a
    /// 32-byte s.
    fn der_signature() -> Vec<u8> {
        let mut der = vec![0x30, 0x45, 0x02, 0x21, 0x00];
        der.extend([0x80; 32]);
        der.extend([0x02, 0x20]);
        der.extend([0x7f; 32]);
        der
    }

    #[test]
    fn secp256k1_key_on_the_mapping_selects_ecdsa_sha256_in_der() {
        let k = key(SigningKeySpec::EcSecp256k1, 7);
        let fp = compute_fingerprint(&k);
        let scheme = PartySignatureScheme::for_party(&mapping(vec![k]), &fp);

        let sig = scheme.canton_signature(&[0x80; 64], &fp);

        assert_eq!(
            sig.signing_algorithm_spec,
            SigningAlgorithmSpec::EcDsaSha256 as i32
        );
        assert_eq!(sig.format, SignatureFormat::Der as i32);
        assert_eq!(sig.signed_by, fp);
        assert_eq!(sig.signature[0], 0x30);
        // Two 33-byte INTEGERs (a sign byte each), 2 bytes of tag + length apiece.
        assert_eq!(sig.signature.len(), 2 + 2 * 35);
    }

    #[test]
    fn ed25519_key_keeps_the_concat_format_and_bytes() {
        let k = key(SigningKeySpec::EcCurve25519, 3);
        let fp = compute_fingerprint(&k);
        let scheme = PartySignatureScheme::for_party(&mapping(vec![k]), &fp);

        let sig = scheme.canton_signature(&[0x80; 64], &fp);

        assert_eq!(
            sig.signing_algorithm_spec,
            SigningAlgorithmSpec::Ed25519 as i32
        );
        assert_eq!(sig.format, SignatureFormat::Concat as i32);
        assert_eq!(sig.signature, vec![0x80; 64]);
    }

    #[test]
    fn unknown_signer_falls_back_to_ed25519() {
        let k = key(SigningKeySpec::EcSecp256k1, 7);
        let scheme = PartySignatureScheme::for_party(&mapping(vec![k]), "1220ff");
        assert_eq!(scheme, PartySignatureScheme::ED25519);
    }

    #[test]
    fn der_ecdsa_signature_passes_through() {
        let der = der_signature();
        let sig = PartySignatureScheme::for_key_spec(SigningKeySpec::EcSecp256k1)
            .map(|scheme| scheme.canton_signature(&der, "1220aa"));
        assert_eq!(sig.as_ref().map(|s| s.signature.clone()), Some(der));
        assert_eq!(sig.map(|s| s.format), Some(SignatureFormat::Der as i32));
    }

    #[test]
    fn a_64_byte_der_signature_is_not_re_encoded() {
        // r and s each lost three leading zero octets: 6 + 29 + 29 = 64 bytes.
        let mut der = vec![0x30, 0x3e, 0x02, 0x1d];
        der.extend([0x11; 29]);
        der.extend([0x02, 0x1d]);
        der.extend([0x22; 29]);
        assert_eq!(der.len(), 64);
        let sig = PartySignatureScheme::for_key_spec(SigningKeySpec::EcSecp256k1)
            .map(|scheme| scheme.canton_signature(&der, "1220aa").signature);
        assert_eq!(sig, Some(der));
    }

    #[test]
    fn accepts_the_forms_each_scheme_can_forward() {
        let ed = PartySignatureScheme::ED25519;
        assert!(ed.accepts(&[0; 64]));
        assert!(!ed.accepts(&[0; 63]));
        assert!(!ed.accepts(&der_signature()));

        let ecdsa = PartySignatureScheme::for_key_spec(SigningKeySpec::EcP256);
        assert_eq!(ecdsa.map(|s| s.accepts(&[0; 64])), Some(true));
        assert_eq!(ecdsa.map(|s| s.accepts(&der_signature())), Some(true));
        assert_eq!(ecdsa.map(|s| s.accepts(&[0; 65])), Some(false));
        assert_eq!(ecdsa.map(|s| s.accepts(&[])), Some(false));

        let p384 = PartySignatureScheme::for_key_spec(SigningKeySpec::EcP384);
        assert_eq!(p384.map(|s| s.accepts(&[0; 96])), Some(true));
        assert_eq!(p384.map(|s| s.accepts(&[0; 64])), Some(false));
    }

    #[test]
    fn der_detection_is_structural() {
        assert!(is_der_ecdsa_signature(&der_signature()));
        assert!(!is_der_ecdsa_signature(&[0x30; 64]));
        // Right tags, outer length one byte short.
        let mut short = der_signature();
        short[1] -= 1;
        assert!(!is_der_ecdsa_signature(&short));
        // Trailing byte after the second INTEGER.
        let mut long = der_signature();
        long[1] += 1;
        long.push(0x00);
        assert!(!is_der_ecdsa_signature(&long));
        // An empty INTEGER is not a number.
        assert!(!is_der_ecdsa_signature(&[
            0x30, 0x04, 0x02, 0x00, 0x02, 0x00
        ]));
    }

    #[test]
    fn concat_to_der_pads_a_high_bit_and_strips_leading_zeros() {
        let mut concat = vec![0u8; 64];
        concat[0] = 0x80;
        concat[63] = 0x01;
        let der = ecdsa_concat_to_der(&concat);
        let mut expected = vec![0x30, 0x26, 0x02, 0x21, 0x00, 0x80];
        expected.extend([0u8; 31]);
        expected.extend([0x02, 0x01, 0x01]);
        assert_eq!(der, expected);
    }

    #[test]
    fn concat_to_der_keeps_a_zero_integer_as_one_byte() {
        let der = ecdsa_concat_to_der(&[0u8; 64]);
        assert_eq!(der, vec![0x30, 0x06, 0x02, 0x01, 0x00, 0x02, 0x01, 0x00]);
    }
}
