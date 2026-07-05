//! Cryptographic primitives for the Partitioned Identity Broker.
//!
//! - PRF Blinding via HMAC-SHA256 (used by AB and IB)
//! - Deterministic Identifier (DI) generation via a keyed PRF (HMAC-SHA256,
//!   used by tTS). The key models the distributed PRF key of the quorum.

use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

// ---------------------------------------------------------------------------
// PRF Blinding
// ---------------------------------------------------------------------------

/// Applies PRF blinding: `HMAC-SHA256(key, data)` → hex-encoded string.
///
/// Each orchestrator uses its own secret key so that the blinded value
/// is unlinkable to the original plaintext by any other party.
///
/// # Examples
/// ```
/// let blinded = shared::crypto::blind(b"secret_key_osa", b"app_123");
/// assert_eq!(blinded.len(), 64); // 256 bits = 64 hex chars
/// ```
pub fn blind(key: &[u8], data: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC-SHA256 accepts keys of any length");
    mac.update(data);
    hex::encode(mac.finalize().into_bytes())
}

// ---------------------------------------------------------------------------
// Deterministic Identifier (DI): computed by tTS via a KEYED PRF
// ---------------------------------------------------------------------------

/// Computes the Deterministic Identifier:
///
/// ```text
/// DI = HMAC-SHA256(k_tTS, blind_app_id || blind_iss || blind_sub)
/// ```
///
/// The DI is a **keyed** PRF: the secret `key` belongs to the tTS quorum
/// (in a full deployment it would be a distributed PRF key held in shares).
/// This is what makes the DI uncomputable by any party that merely observes
/// the blinded tuple: notably the IB, which holds `blind_iss`/`blind_sub`
/// and could otherwise derive the DI and link it to the real identity.
///
/// The tTS only ever sees blinded values, so it cannot learn the
/// original `app_id`, `iss`, or `sub`.
pub fn compute_di(key: &[u8], blind_app: &str, blind_iss: &str, blind_sub: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC-SHA256 accepts keys of any length");
    mac.update(blind_app.as_bytes());
    mac.update(blind_iss.as_bytes());
    mac.update(blind_sub.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

// ---------------------------------------------------------------------------
// Issuance-envelope authentication (IB -> tTS, step 12 / "POST /issue")
// ---------------------------------------------------------------------------

/// Shared secret used to authenticate the IB -> tTS issuance envelope.
///
/// This is a PROTOTYPE stand-in: a symmetric MAC key held by both the IB and
/// the tTS. In a full deployment the envelope would instead carry an Ed25519
/// **signature** under the IB's private key, verified by the tTS against the
/// IB's public key (true origin authentication, no shared secret). HMAC is used
/// here to stay consistent with the rest of the prototype, which already
/// approximates the distributed PRF with a single keyed HMAC.
pub const ISSUE_MAC_KEY_DEMO: &[u8] = b"shared_issue_mac_key_ib_tts_prototype_2024";

fn push_field(buf: &mut Vec<u8>, field: &[u8]) {
    // Length-prefix every field so the concatenation is unambiguous (no field
    // can be shifted into an adjacent one), independent of the field contents.
    buf.extend_from_slice(&(field.len() as u64).to_be_bytes());
    buf.extend_from_slice(field);
}

/// Canonical, unambiguous byte encoding of the IB -> tTS issuance envelope.
///
/// Both the IB (signer) and the tTS (verifier) build these exact bytes, so the
/// MAC binds the whole blinded tuple plus the routing/anti-replay fields. The
/// domain-separation prefix prevents the tag from being reused in another
/// context, and the explicit nonce-presence byte distinguishes "no nonce" from
/// "empty nonce".
pub fn issue_signing_bytes(
    blind_app: &str,
    blind_iss: &str,
    blind_sub: &str,
    session_id: &str,
    nonce: Option<&str>,
) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(b"moshi-issue-v1");
    push_field(&mut buf, blind_app.as_bytes());
    push_field(&mut buf, blind_iss.as_bytes());
    push_field(&mut buf, blind_sub.as_bytes());
    push_field(&mut buf, session_id.as_bytes());
    match nonce {
        Some(n) => {
            buf.push(1);
            push_field(&mut buf, n.as_bytes());
        }
        None => buf.push(0),
    }
    buf
}

/// Shared secret authenticating the AB -> IB authorize redirect (step 5). This
/// hop is front-channel (it travels as query parameters through the browser over
/// plain HTTP), so the tag is what stops the user-agent from tampering with
/// `blind_app_id`/`session_id`/`nonce`. Same PROTOTYPE caveat as
/// [`ISSUE_MAC_KEY_DEMO`]: a deployment would use an \texttt{Ed25519} signature
/// under the AB's key.
pub const AUTHORIZE_MAC_KEY_DEMO: &[u8] = b"shared_authorize_mac_key_ab_ib_prototype_2024";

/// Shared secret authenticating the tTS -> AB token delivery (step 18). The JWT
/// is already FROST-signed, but the `session_id`->`jwt` *binding* is not; this
/// tag stops a tampered delivery from routing a valid token to the wrong AB
/// session if the transport is broken.
pub const DELIVERY_MAC_KEY_DEMO: &[u8] = b"shared_delivery_mac_key_tts_ab_prototype_2024";

/// Canonical bytes for the AB -> IB authorize-redirect envelope (step 5):
/// binds the blinded app id, the routing handle and the OIDC nonce.
pub fn authorize_signing_bytes(
    blind_app: &str,
    session_id: &str,
    nonce: Option<&str>,
) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(b"moshi-authorize-v1");
    push_field(&mut buf, blind_app.as_bytes());
    push_field(&mut buf, session_id.as_bytes());
    match nonce {
        Some(n) => {
            buf.push(1);
            push_field(&mut buf, n.as_bytes());
        }
        None => buf.push(0),
    }
    buf
}

/// Canonical bytes for the tTS -> AB delivery envelope (step 18): binds the
/// minted JWT to the `session_id` it must be delivered to.
pub fn delivery_signing_bytes(session_id: &str, jwt: &str) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(b"moshi-delivery-v1");
    push_field(&mut buf, session_id.as_bytes());
    push_field(&mut buf, jwt.as_bytes());
    buf
}

/// Computes `HMAC-SHA256(key, msg)` as a hex string: the authentication tag
/// carried alongside the envelope so its integrity survives TLS termination.
pub fn mac(key: &[u8], msg: &[u8]) -> String {
    let mut m = HmacSha256::new_from_slice(key).expect("HMAC-SHA256 accepts keys of any length");
    m.update(msg);
    hex::encode(m.finalize().into_bytes())
}

/// Constant-time verification of a hex tag produced by [`mac`]. Returns `false`
/// on a malformed hex tag or any mismatch (never short-circuits on content).
pub fn verify_mac(key: &[u8], msg: &[u8], tag_hex: &str) -> bool {
    let tag = match hex::decode(tag_hex) {
        Ok(t) => t,
        Err(_) => return false,
    };
    let mut m = HmacSha256::new_from_slice(key).expect("HMAC-SHA256 accepts keys of any length");
    m.update(msg);
    m.verify_slice(&tag).is_ok()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blind_is_deterministic() {
        let key = b"key_osa";
        let data = b"app_xyz";
        assert_eq!(blind(key, data), blind(key, data));
    }

    #[test]
    fn blind_differs_with_different_keys() {
        let data = b"app_xyz";
        assert_ne!(blind(b"key_1", data), blind(b"key_2", data));
    }

    #[test]
    fn blind_differs_with_different_data() {
        let key = b"same_key";
        assert_ne!(blind(key, b"app_a"), blind(key, b"app_b"));
    }

    #[test]
    fn blind_output_is_64_hex_chars() {
        let out = blind(b"k", b"d");
        assert_eq!(out.len(), 64);
        assert!(out.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn di_is_deterministic() {
        let key = b"tts_di_key";
        let di1 = compute_di(key, "ba", "bi", "bs");
        let di2 = compute_di(key, "ba", "bi", "bs");
        assert_eq!(di1, di2);
    }

    #[test]
    fn di_changes_if_any_input_changes() {
        let key = b"tts_di_key";
        let base = compute_di(key, "ba", "bi", "bs");
        assert_ne!(base, compute_di(key, "XX", "bi", "bs"));
        assert_ne!(base, compute_di(key, "ba", "XX", "bs"));
        assert_ne!(base, compute_di(key, "ba", "bi", "XX"));
    }

    #[test]
    fn di_differs_with_different_keys() {
        // Without the tTS key, the same blinded tuple yields a different DI,
        // so a party holding only the blinded tuple cannot derive the DI.
        assert_ne!(
            compute_di(b"key_1", "ba", "bi", "bs"),
            compute_di(b"key_2", "ba", "bi", "bs")
        );
    }

    #[test]
    fn mac_roundtrips() {
        let key = b"k_sig";
        let msg = issue_signing_bytes("ba", "bi", "bs", "sid", Some("nonce"));
        let tag = mac(key, &msg);
        assert!(verify_mac(key, &msg, &tag));
    }

    #[test]
    fn mac_rejects_tampered_field() {
        // Flipping any blinded field invalidates the tag: this is exactly the
        // protection that survives a broken/terminated TLS leg.
        let key = b"k_sig";
        let tag = mac(key, &issue_signing_bytes("ba", "bi", "bs", "sid", Some("n")));
        assert!(!verify_mac(key, &issue_signing_bytes("XX", "bi", "bs", "sid", Some("n")), &tag));
        assert!(!verify_mac(key, &issue_signing_bytes("ba", "XX", "bs", "sid", Some("n")), &tag));
        assert!(!verify_mac(key, &issue_signing_bytes("ba", "bi", "XX", "sid", Some("n")), &tag));
        assert!(!verify_mac(key, &issue_signing_bytes("ba", "bi", "bs", "XXX", Some("n")), &tag));
        assert!(!verify_mac(key, &issue_signing_bytes("ba", "bi", "bs", "sid", Some("XX")), &tag));
    }

    #[test]
    fn mac_rejects_wrong_key() {
        let msg = issue_signing_bytes("ba", "bi", "bs", "sid", None);
        let tag = mac(b"k_sig", &msg);
        assert!(!verify_mac(b"other_key", &msg, &tag));
    }

    #[test]
    fn mac_distinguishes_absent_and_empty_nonce() {
        // "no nonce" and "empty nonce" must not collide under the MAC.
        let key = b"k_sig";
        let tag_none = mac(key, &issue_signing_bytes("ba", "bi", "bs", "sid", None));
        assert!(!verify_mac(key, &issue_signing_bytes("ba", "bi", "bs", "sid", Some("")), &tag_none));
    }

    #[test]
    fn mac_rejects_malformed_tag() {
        let msg = issue_signing_bytes("ba", "bi", "bs", "sid", None);
        assert!(!verify_mac(b"k_sig", &msg, "not-hex"));
    }

    #[test]
    fn authorize_mac_roundtrips_and_detects_tamper() {
        let k = b"k_auth";
        let tag = mac(k, &authorize_signing_bytes("ba", "sid", Some("n")));
        assert!(verify_mac(k, &authorize_signing_bytes("ba", "sid", Some("n")), &tag));
        assert!(!verify_mac(k, &authorize_signing_bytes("XX", "sid", Some("n")), &tag));
        assert!(!verify_mac(k, &authorize_signing_bytes("ba", "XXX", Some("n")), &tag));
    }

    #[test]
    fn delivery_mac_binds_session_to_jwt() {
        let k = b"k_del";
        let tag = mac(k, &delivery_signing_bytes("sid", "header.payload.sig"));
        assert!(verify_mac(k, &delivery_signing_bytes("sid", "header.payload.sig"), &tag));
        // swapping the session_id (the attack this binding prevents) must fail
        assert!(!verify_mac(k, &delivery_signing_bytes("other", "header.payload.sig"), &tag));
        // tampering the jwt must fail
        assert!(!verify_mac(k, &delivery_signing_bytes("sid", "header.payload.XXX"), &tag));
    }
}
