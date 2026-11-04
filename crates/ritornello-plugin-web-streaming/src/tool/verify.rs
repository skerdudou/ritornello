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

/// The keys Node.js releases are signed with, for the plugin's own node on
/// armv7 ([`super::runtime::install_node`]): the armored blocks of
/// `https://raw.githubusercontent.com/nodejs/release-keys/main/keys/<FPR>.asc`,
/// one per fingerprint of that repository's `keys.list`, concatenated in the
/// list's order. Fetched on 2026-11-04: 29 keys, the 29 fingerprints of
/// [`NODE_KEY_FINGERPRINTS`]. The release list of `latest-v22.x` then
/// (v22.23.4, `tests/fixtures/sums/node-v22.23.4/`) is signed by the primary
/// key `5BE8A3F6C8A5C01D106C0AD820B1A390B168D356`.
///
/// A releaser who joins later is unknown here: their release is refused and
/// the node in place keeps serving until a plugin version embeds their key.
pub const NODE_RELEASE_KEYS: &str = include_str!("node-release-keys.asc");

/// `keys.list` of nodejs/release-keys on 2026-11-04, in its order; a test
/// holds the embedded keys to it, one for one.
pub const NODE_KEY_FINGERPRINTS: [&str; 29] = [
    "4ED778F539E3634C779C87C6D7062848A1AB005C",
    "94AE36675C464D64BAFA68DD7434390BDBE9B9C5",
    "1C050899334244A8AF75E53792EF661D867B9DFA",
    "B9AE9905FFD7803F25714661B63B535A4C206CA9",
    "77984A986EBC2AA786BC0F66B01FBB92821C587A",
    "71DCFD284A79C3B38668286BC97EC7A07EDE3FC1",
    "61FC681DFB92A079F1685E77973F295594EC4689",
    "FD3A5288F042B6850C66B31F09FE44734EB7990E",
    "8FCCA13FEF1D0C2E91008E09770F7A9A5AE15600",
    "C4F0DFFF4E8C1A8236409D08E73BC641CC11F4C8",
    "890C08DB8579162FEE0DF9DB8BEAB4DFCF555EF4",
    "C82FA3AE1CBEDC6BE46B9360C43CEC45C17AB93C",
    "DD8F2338BAE7501E3DD5AC78C273792F7D83545D",
    "A48C2BEE680E841632CD4E44F07496B3EB3C1762",
    "B9E2F5981AA6E0CD28160D9FF13993A75599653C",
    "108F52B48DB57BB0CC439B2997B01419BD92F80A",
    "9554F04D7259F04124DE6B476D5A82AC7E37093B",
    "93C7E9E91B49E432C2F75674B0A78B0A6C481CF6",
    "56730D5401028683275BD23C23EFEFE93C4CFFFE",
    "114F43EE0176B71C7BC219DD50A3051F888C628D",
    "7937DFD2AB06298B2293C3187D33FF9D0246406D",
    "74F12602B6F1C4E913FAA37AD3A89613643B6201",
    "141F07595B7B3FFE74309A937405533BE57C7D57",
    "DD792F5973C6DE52C432CBDAC77ABFA00DDBF2B7",
    "A363A499291CBBC940DD62E41F10027AF002F8B0",
    "CC68F5A3106FF448322E48ED27F5E38D5B0A215F",
    "C0D6248439F1D5604AAFFB4021D900FFDB233756",
    "5BE8A3F6C8A5C01D106C0AD820B1A390B168D356",
    "655F3B5C1FB3FA8D1A0CA6BDE4A7D232B936D2FD",
];

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

/// The armored blocks of `text`, each from its `BEGIN` line.
fn armored_blocks(text: &str) -> Vec<&str> {
    let starts: Vec<usize> = text.match_indices("-----BEGIN PGP PUBLIC KEY BLOCK-----").map(|(i, _)| i).collect();
    starts.iter().enumerate().map(|(n, &s)| &text[s..starts.get(n + 1).copied().unwrap_or(text.len())]).collect()
}

/// The embedded Node.js release keys, each parsed on its own: one the `pgp`
/// crate cannot read fails the whole set, loudly, rather than shrinking it.
pub fn node_keys() -> Result<Vec<SignedPublicKey>, String> {
    armored_blocks(NODE_RELEASE_KEYS)
        .into_iter()
        .enumerate()
        .map(|(i, block)| SignedPublicKey::from_string(block).map(|(k, _)| k).map_err(|e| format!("node release key #{i}: {e}")))
        .collect()
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
    let sig = document_signature(sig)?;
    sig.verify(&key.primary_key, sums).map_err(|e| VerifyError::BadSignature(e.to_string()))
}

/// The signature `sig` (binary or armored) when it may vouch for a document
/// at all: a signature of a document (not a standalone or timestamp one, for
/// which pgp hashes a single byte of the data), with a strong hash.
fn document_signature(sig: &[u8]) -> Result<DetachedSignature, VerifyError> {
    let bad = |why: String| VerifyError::BadSignature(why);
    let (sig, _) = DetachedSignature::from_reader_single(sig).map_err(|e| bad(format!("unreadable: {e}")))?;
    if !matches!(sig.signature.typ(), Some(SignatureType::Binary | SignatureType::Text)) {
        return Err(bad(format!("not a document signature ({:?})", sig.signature.typ())));
    }
    match sig.signature.hash_alg() {
        Some(h) if strong(h) => {}
        other => return Err(bad(format!("weak or unknown hash {other:?}"))),
    }
    Ok(sig)
}

/// `sig` is a signature of the document `sums` by one of `keys`: by its
/// primary key, or by one of its subkeys that may sign, bound to it by a
/// binding that verifies (a signing flag, the primary's binding signature
/// and the subkey's back-signature), and not revoked. Node.js releasers sign
/// either way. Every other key is refused, and so is a subkey merely
/// attached to an embedded key: anyone can append a subkey to a public key,
/// only its owner can bind one.
///
/// yt-dlp's list never goes through here: [`check_signature`] keeps it to
/// its key's primary.
pub fn check_signature_by_any(sums: &[u8], sig: &[u8], keys: &[SignedPublicKey]) -> Result<(), VerifyError> {
    let sig = document_signature(sig)?;
    for key in keys {
        if sig.verify(&key.primary_key, sums).is_ok() {
            return Ok(());
        }
        // The document first: a binding is checked only for the subkey that
        // actually made the signature.
        if key.public_subkeys.iter().any(|sub| sig.verify(&sub.key, sums).is_ok() && bound_for_signing(key, sub)) {
            return Ok(());
        }
    }
    Err(VerifyError::BadSignature("made by none of the embedded release keys".into()))
}

/// The subkey `sub` is bound to `key` for signing: at least one binding
/// signature, every binding verifying (the back-signature of a signing one
/// included), one of them with the signing flag, and no revocation.
fn bound_for_signing(key: &SignedPublicKey, sub: &pgp::composed::SignedPublicSubKey) -> bool {
    let of = |t: SignatureType| sub.signatures.iter().filter(move |s| s.typ() == Some(t));
    of(SignatureType::SubkeyRevocation).next().is_none()
        && of(SignatureType::SubkeyBinding).any(|s| s.key_flags().sign())
        && sub.verify_bindings(&key.primary_key).is_ok()
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

    /// Every embedded Node.js key is read, is one of `keys.list`, and holds
    /// its own self-signatures and subkey bindings.
    #[test]
    fn the_embedded_node_keys_are_the_listed_ones_and_their_bindings_hold() {
        let keys = node_keys().unwrap();
        let mut got: Vec<String> = keys.iter().map(|k| format!("{:X}", k.fingerprint())).collect();
        assert_eq!(got.len(), NODE_KEY_FINGERPRINTS.len(), "one block per listed key");
        got.sort();
        let mut want: Vec<String> = NODE_KEY_FINGERPRINTS.iter().map(|s| s.to_string()).collect();
        want.sort();
        want.dedup();
        assert_eq!(want.len(), 29, "the list on 2026-11-04 named 29 distinct keys");
        assert_eq!(got, want);
        let mut faults = Vec::new();
        for key in &keys {
            let fpr = format!("{:X}", key.fingerprint());
            // Their own certifications only: some blocks also carry other
            // releasers' certifications, which this key cannot verify.
            let mut own = 0;
            for user in &key.details.users {
                for sig in user.signatures.iter().filter(|s| issued_by(s, key)) {
                    own += 1;
                    if let Err(e) = sig.verify_certification(&key.primary_key, pgp::types::Tag::UserId, &user.id) {
                        faults.push(format!("{fpr} user {:?}: {e}", user.id.id()));
                    }
                }
            }
            if own == 0 {
                faults.push(format!("{fpr}: no self-certification"));
            }
            for sub in &key.public_subkeys {
                if let Err(e) = sub.verify_bindings(&key.primary_key) {
                    faults.push(format!("{fpr} subkey {:X}: {e}", sub.key.fingerprint()));
                }
            }
        }
        assert!(faults.is_empty(), "{faults:#?}");
    }

    fn issued_by(sig: &pgp::packet::Signature, key: &SignedPublicKey) -> bool {
        sig.issuer_fingerprint().iter().any(|f| **f == key.fingerprint()) || sig.issuer_key_id().iter().any(|id| **id == key.legacy_key_id())
    }

    /// The real list of `latest-v22.x` (v22.23.4), as nodejs.org served it
    /// on 2026-11-04, with its real detached signature.
    #[test]
    fn the_real_node_list_verifies_with_the_embedded_keys() {
        let keys = node_keys().unwrap();
        let (sums, sig) = (capture("node-v22.23.4", "SHASUMS256.txt"), capture("node-v22.23.4", "SHASUMS256.txt.sig"));
        check_signature_by_any(&sums, &sig, &keys).unwrap();
        // Made by the key the spec measured, and by its primary.
        let signer = keys.iter().find(|k| format!("{:X}", k.fingerprint()) == "5BE8A3F6C8A5C01D106C0AD820B1A390B168D356").unwrap();
        check_signature(&sums, &sig, signer).unwrap();
        let mut tampered = sums.clone();
        tampered[0] ^= 1;
        assert!(matches!(check_signature_by_any(&tampered, &sig, &keys), Err(VerifyError::BadSignature(_))));
        // The other 28 keys do not vouch for it.
        let others: Vec<_> = keys.iter().filter(|k| !std::ptr::eq(*k, signer)).cloned().collect();
        assert!(matches!(check_signature_by_any(&sums, &sig, &others), Err(VerifyError::BadSignature(_))));
    }

    /// yt-dlp's list stays with its own key: signed by a Node.js key, or by
    /// the subkey of the key it is checked with, it is refused.
    #[test]
    fn yt_dlps_check_takes_no_subkey_and_no_node_key() {
        let sums = sums_for(&[("yt-dlp", b"the asset")]);
        let by_sub = sign_by_subkey(subkey_signer(), 0, &sums);
        assert!(matches!(check_signature(&sums, &by_sub, &public(subkey_signer())), Err(VerifyError::BadSignature(_))));
        let node = capture("node-v22.23.4", "SHASUMS256.txt");
        let node_sig = capture("node-v22.23.4", "SHASUMS256.txt.sig");
        assert!(check_signature(&node, &node_sig, &ytdlp_key().unwrap()).is_err());
    }

    #[test]
    fn a_node_list_signed_by_a_bound_signing_subkey_is_taken() {
        let sums = sums_for(&[("node-v22.1.0-linux-armv7l.tar.gz", b"tarball")]);
        let keys = [public(other_key()), public(subkey_signer())];
        assert_eq!(check_signature_by_any(&sums, &sign_by_subkey(subkey_signer(), 0, &sums), &keys), Ok(()));
        // Its primary too.
        let by_primary = sign_with(subkey_signer(), HashAlgorithm::Sha512, &sums);
        assert_eq!(check_signature_by_any(&sums, &by_primary, &keys), Ok(()));
    }

    #[test]
    fn a_node_list_signed_by_an_unknown_key_is_refused() {
        let sums = sums_for(&[("node-v22.1.0-linux-armv7l.tar.gz", b"tarball")]);
        let sig = sign_with(other_key(), HashAlgorithm::Sha512, &sums);
        let keys = [public(test_key()), public(subkey_signer())];
        assert!(matches!(check_signature_by_any(&sums, &sig, &keys), Err(VerifyError::BadSignature(_))));
        assert!(matches!(check_signature_by_any(&sums, &sig, &[]), Err(VerifyError::BadSignature(_))));
    }

    /// Anyone can append a subkey to a public key; only the key's owner can
    /// bind one. A subkey bound to another primary, or bound without the
    /// signing flag, vouches for nothing.
    #[test]
    fn a_subkey_without_a_valid_signing_binding_is_refused() {
        let sums = sums_for(&[("node-v22.1.0-linux-armv7l.tar.gz", b"tarball")]);
        let by_sub = sign_by_subkey(subkey_signer(), 0, &sums);
        let embedded = public(test_key());
        let mut grafted = embedded.clone();
        grafted.public_subkeys = public(subkey_signer()).public_subkeys;
        match check_signature_by_any(&sums, &by_sub, &[grafted]) {
            Err(VerifyError::BadSignature(why)) => assert!(why.contains("none of the embedded"), "{why}"),
            other => panic!("{other:?}"),
        }
        // Bound, but not for signing.
        let not_for_signing = sign_by_subkey(subkey_signer(), 1, &sums);
        let keys = [public(subkey_signer())];
        assert!(matches!(check_signature_by_any(&sums, &not_for_signing, &keys), Err(VerifyError::BadSignature(_))));
    }

    /// A signing subkey its owner revoked vouches for nothing any more.
    #[test]
    fn a_revoked_signing_subkey_is_refused() {
        use pgp::packet::{SignatureConfig, Subpacket, SubpacketData};
        use pgp::types::{KeyDetails as _, Password, Timestamp};
        let sums = sums_for(&[("node-v22.1.0-linux-armv7l.tar.gz", b"tarball")]);
        let by_sub = sign_by_subkey(subkey_signer(), 0, &sums);
        let secret = subkey_signer();
        let mut key = public(secret);
        assert_eq!(check_signature_by_any(&sums, &by_sub, std::slice::from_ref(&key)), Ok(()), "bound before");
        let mut config =
            SignatureConfig::v4(SignatureType::SubkeyRevocation, secret.primary_key.algorithm(), HashAlgorithm::Sha256);
        config.hashed_subpackets = vec![
            Subpacket::regular(SubpacketData::IssuerFingerprint(secret.fingerprint())).unwrap(),
            Subpacket::regular(SubpacketData::SignatureCreationTime(Timestamp::now())).unwrap(),
        ];
        let revocation = config
            .sign_subkey_binding(&secret.primary_key, &key.primary_key, &Password::empty(), &key.public_subkeys[0].key)
            .unwrap();
        key.public_subkeys[0].signatures.push(revocation);
        key.public_subkeys[0].verify_bindings(&key.primary_key).expect("a genuine revocation");
        assert!(matches!(check_signature_by_any(&sums, &by_sub, &[key]), Err(VerifyError::BadSignature(_))));
    }

    /// The refusals of yt-dlp's check hold for Node.js's too.
    #[test]
    fn a_node_list_signature_with_a_broken_hash_is_refused() {
        let sums = sums_for(&[("node-v22.1.0-linux-armv7l.tar.gz", b"tarball")]);
        let sig = sign_with(rsa_key(), HashAlgorithm::Sha1, &sums);
        match check_signature_by_any(&sums, &sig, &[public(rsa_key())]) {
            Err(VerifyError::BadSignature(why)) => assert!(why.contains("weak"), "{why}"),
            other => panic!("{other:?}"),
        }
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
