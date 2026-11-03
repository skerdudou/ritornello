//! Whether a downloaded yt-dlp is the one its authors released.
//!
//! The chain: `SHA2-256SUMS.sig` is a detached OpenPGP signature over
//! `SHA2-256SUMS`, made with yt-dlp's release key (embedded below); that list
//! names the SHA-256 of each asset; the asset must hash to the sum the list
//! gives under its exact name. Any link that does not hold refuses the asset.
//!
//! The signature is checked with the `pgp` crate: the device has no `gpgv`
//! (measured), and a missing system tool must not turn into "unchecked".

use pgp::composed::{Deserializable, DetachedSignature, SignedPublicKey};
use pgp::crypto::hash::HashAlgorithm;
use pgp::packet::SignatureType;
use sha2::{Digest, Sha256};

/// yt-dlp's release signing key, as published at the root of its repository
/// (`public.key`, https://github.com/yt-dlp/yt-dlp/blob/master/public.key,
/// added by commit 12647e03 "[build] Sign SHA files and release public key"
/// on 2023-03-03 and unchanged since):
///
/// `Simon Sawicki (yt-dlp signing key) <contact@grub4k.xyz>`, RSA 4096,
/// created 2023-02-27, fingerprint
/// `AC0C BBE6 848D 6A87 3464  AF4E 57CF 6593 3B5A 7581`.
///
/// Cross-checked on 2026-11-02 against keys.openpgp.org, which publishes a
/// user ID only once the owner of the address has confirmed it: the key it
/// serves for `contact@grub4k.xyz` has this same fingerprint. The release
/// signatures of both channels (stable 2026.10.26, nightly
/// 2026.10.29.232701) verify with it (`tests/fixtures/sums/`).
pub const YTDLP_SIGNING_KEY: &str = include_str!("yt-dlp-signing-key.asc");

/// The fingerprint above, without spaces; a test holds the embedded key to it.
pub const YTDLP_KEY_FINGERPRINT: &str = "AC0CBBE6848D6A873464AF4E57CF65933B5A7581";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyError {
    /// The signature does not verify with the key (or is not a signature of
    /// a document, or uses a broken hash).
    BadSignature(String),
    /// The signed list has no line for the asset's exact name.
    NotListed,
    /// The asset does not hash to the sum the signed list gives.
    Mismatch,
}

impl std::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VerifyError::BadSignature(why) => write!(f, "bad signature: {why}"),
            VerifyError::NotListed => write!(f, "the asset is not in the signed list"),
            VerifyError::Mismatch => write!(f, "the asset does not match its signed sum"),
        }
    }
}

/// The embedded yt-dlp key.
pub fn ytdlp_key() -> Result<SignedPublicKey, String> {
    SignedPublicKey::from_string(YTDLP_SIGNING_KEY).map(|(k, _)| k).map_err(|e| e.to_string())
}

pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

/// Hashes a signature may use: none of the broken ones (MD5, SHA-1,
/// RIPEMD-160), whose collisions would let another list carry the same
/// signature.
fn strong(hash: HashAlgorithm) -> bool {
    matches!(
        hash,
        HashAlgorithm::Sha256
            | HashAlgorithm::Sha384
            | HashAlgorithm::Sha512
            | HashAlgorithm::Sha3_256
            | HashAlgorithm::Sha3_512
    )
}

/// `sig` (binary or armored) is a signature of the document `sums` by the
/// primary key of `key`. Only the primary key: yt-dlp's has no subkey, and a
/// key that grew one would be a new key to embed, not something to accept.
pub fn check_signature(sums: &[u8], sig: &[u8], key: &SignedPublicKey) -> Result<(), VerifyError> {
    let bad = |why: String| VerifyError::BadSignature(why);
    let (sig, _) = DetachedSignature::from_reader_single(sig).map_err(|e| bad(format!("unreadable: {e}")))?;
    if !matches!(sig.signature.typ(), Some(SignatureType::Binary | SignatureType::Text)) {
        return Err(bad(format!("not a document signature ({:?})", sig.signature.typ())));
    }
    match sig.signature.hash_alg() {
        Some(h) if strong(h) => {}
        other => return Err(bad(format!("weak or unknown hash {other:?}"))),
    }
    sig.verify(&key.primary_key, sums).map_err(|e| bad(e.to_string()))
}

/// The sum `sums` gives for exactly `name` (GNU format: `<hex>  <name>`, or
/// `<hex> *<name>` in binary mode). The first line naming it wins.
pub fn listed_sum(sums: &[u8], name: &str) -> Result<[u8; 32], VerifyError> {
    let text = String::from_utf8_lossy(sums);
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        let Some((hex, rest)) = line.split_once(' ') else { continue };
        let listed = rest.strip_prefix(' ').or_else(|| rest.strip_prefix('*')).unwrap_or(rest);
        if listed != name {
            continue;
        }
        return parse_hex(hex).ok_or(VerifyError::NotListed);
    }
    Err(VerifyError::NotListed)
}

fn parse_hex(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 || !hex.is_ascii() {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

pub fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The whole chain, in order: signature, listing, sum.
pub fn verify(sums: &[u8], sig: &[u8], key: &SignedPublicKey, asset: &[u8], asset_name: &str) -> Result<(), VerifyError> {
    check_signature(sums, sig, key)?;
    let want = listed_sum(sums, asset_name)?;
    if sha256(asset) != want {
        return Err(VerifyError::Mismatch);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::testkit::*;
    use pgp::types::KeyDetails;
    use std::path::Path;

    fn capture(dir: &str, name: &str) -> Vec<u8> {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sums").join(dir).join(name);
        std::fs::read(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
    }

    #[test]
    fn the_embedded_key_is_yt_dlps_and_its_self_signatures_hold() {
        let key = ytdlp_key().unwrap();
        assert_eq!(format!("{:X}", key.fingerprint()), YTDLP_KEY_FINGERPRINT);
        key.verify_bindings().unwrap();
        assert!(key.public_subkeys.is_empty(), "a subkey appeared: re-read check_signature's doc");
    }

    /// Real captures of both channels, verified with the embedded real key:
    /// the nightly builds are signed with the same key as the stable ones.
    #[test]
    fn the_real_stable_and_nightly_lists_verify_with_the_embedded_key() {
        let key = ytdlp_key().unwrap();
        for dir in ["stable-2026.10.26", "nightly-2026.10.29.232701"] {
            let (sums, sig) = (capture(dir, "SHA2-256SUMS"), capture(dir, "SHA2-256SUMS.sig"));
            check_signature(&sums, &sig, &key).unwrap_or_else(|e| panic!("{dir}: {e}"));
            listed_sum(&sums, "yt-dlp").unwrap();
            let mut tampered = sums.clone();
            tampered[0] ^= 1;
            assert!(matches!(check_signature(&tampered, &sig, &key), Err(VerifyError::BadSignature(_))), "{dir}");
        }
        let sums = capture("stable-2026.10.26", "SHA2-256SUMS");
        assert_eq!(
            to_hex(&listed_sum(&sums, "yt-dlp").unwrap()),
            "e51e69e89489f199530f13d32a4342da3ddb2cf34842f364603a066db05afbf3"
        );
    }

    #[test]
    fn a_valid_chain_verifies() {
        let asset = b"the asset".to_vec();
        let sums = sums_for(&[("yt-dlp.exe", b"other"), ("yt-dlp", &asset)]);
        let key = public(test_key());
        assert_eq!(verify(&sums, &sign(&sums), &key, &asset, "yt-dlp"), Ok(()));
    }

    #[test]
    fn a_signature_by_another_key_or_over_other_bytes_is_refused() {
        let asset = b"the asset".to_vec();
        let sums = sums_for(&[("yt-dlp", &asset)]);
        let key = public(test_key());
        let by_other = sign_with(other_key(), HashAlgorithm::Sha512, &sums);
        assert!(matches!(verify(&sums, &by_other, &key, &asset, "yt-dlp"), Err(VerifyError::BadSignature(_))));
        let over_other = sign(b"another list");
        assert!(matches!(verify(&sums, &over_other, &key, &asset, "yt-dlp"), Err(VerifyError::BadSignature(_))));
        assert!(matches!(verify(&sums, b"garbage", &key, &asset, "yt-dlp"), Err(VerifyError::BadSignature(_))));
    }

    #[test]
    fn a_signature_with_a_broken_hash_is_refused() {
        let asset = b"the asset".to_vec();
        let sums = sums_for(&[("yt-dlp", &asset)]);
        // RSA, like yt-dlp's key: EdDSA refuses SHA-1 by itself, RSA does not.
        let key = public(rsa_key());
        for weak in [HashAlgorithm::Sha1, HashAlgorithm::Md5] {
            let sig = sign_with(rsa_key(), weak, &sums);
            match verify(&sums, &sig, &key, &asset, "yt-dlp") {
                Err(VerifyError::BadSignature(why)) => assert!(why.contains("weak"), "{weak:?}: {why}"),
                other => panic!("{weak:?}: {other:?}"),
            }
        }
        // The same key with a strong hash passes: it is the hash refused.
        let sha256 = sign_with(rsa_key(), HashAlgorithm::Sha256, &sums);
        assert_eq!(verify(&sums, &sha256, &key, &asset, "yt-dlp"), Ok(()));
    }

    /// pgp hashes a single byte of the data for a standalone or timestamp
    /// signature: without the type check, any such signature ever made with
    /// the key would vouch for every list that begins with the same byte.
    #[test]
    fn a_signature_that_is_not_of_a_document_is_refused() {
        use pgp::packet::{Signature, SignatureConfig, Subpacket, SubpacketData};
        use pgp::types::{KeyDetails as _, Password, SigningKey, Timestamp};
        let asset = b"the asset".to_vec();
        let sums = sums_for(&[("yt-dlp", &asset)]);
        let key = test_key();
        for typ in [SignatureType::Standalone, SignatureType::Timestamp] {
            let mut config = SignatureConfig::v4(typ, key.primary_key.algorithm(), HashAlgorithm::Sha512);
            config.hashed_subpackets = vec![
                Subpacket::regular(SubpacketData::IssuerFingerprint(key.fingerprint())).unwrap(),
                Subpacket::regular(SubpacketData::SignatureCreationTime(Timestamp::now())).unwrap(),
            ];
            // Signed over another document, that only shares the first byte.
            let other = [sums[0], b'z', b'z'];
            let mut hasher = HashAlgorithm::Sha512.new_hasher().unwrap();
            config.hash_data_to_sign(&mut hasher, &other[..]).unwrap();
            let len = config.hash_signature_data(&mut hasher).unwrap();
            hasher.update(&config.trailer(len).unwrap());
            let hash = hasher.finalize();
            let raw = key.primary_key.sign(&Password::empty(), HashAlgorithm::Sha512, &hash).unwrap();
            let sig = Signature::from_config(config, [hash[0], hash[1]], raw).unwrap();
            let bytes = pgp::ser::Serialize::to_bytes(&DetachedSignature::new(sig)).unwrap();
            match verify(&sums, &bytes, &public(key), &asset, "yt-dlp") {
                Err(VerifyError::BadSignature(why)) => assert!(why.contains("not a document"), "{typ:?}: {why}"),
                other => panic!("{typ:?}: {other:?}"),
            }
        }
    }

    #[test]
    fn an_asset_not_listed_under_its_exact_name_is_refused() {
        let asset = b"the asset".to_vec();
        let sums = sums_for(&[("yt-dlp.exe", &asset), ("yt-dlp_linux", &asset), ("xyt-dlp", &asset)]);
        assert_eq!(verify(&sums, &sign(&sums), &public(test_key()), &asset, "yt-dlp"), Err(VerifyError::NotListed));
    }

    #[test]
    fn an_asset_that_does_not_match_its_sum_is_refused() {
        let sums = sums_for(&[("yt-dlp", b"what was released")]);
        assert_eq!(
            verify(&sums, &sign(&sums), &public(test_key()), b"something else", "yt-dlp"),
            Err(VerifyError::Mismatch)
        );
    }

    #[test]
    fn the_list_is_read_in_the_gnu_formats() {
        let h = "e51e69e89489f199530f13d32a4342da3ddb2cf34842f364603a066db05afbf3";
        for line in [format!("{h}  yt-dlp\n"), format!("{h} *yt-dlp\r\n"), format!("{h}  yt-dlp")] {
            assert_eq!(to_hex(&listed_sum(line.as_bytes(), "yt-dlp").unwrap()), h, "{line:?}");
        }
        // A malformed sum under the right name is not a sum.
        assert_eq!(listed_sum(b"e51e  yt-dlp\n", "yt-dlp"), Err(VerifyError::NotListed));
        assert_eq!(listed_sum(format!("{}  yt-dlp\n", "zz".repeat(32)).as_bytes(), "yt-dlp"), Err(VerifyError::NotListed));
    }
}
