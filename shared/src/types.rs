//! Shared data types for inter-service communication.
//!
//! Every struct here is (de)serializable via JSON for the HTTPS payloads.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Step 1 – App -> AB
// ---------------------------------------------------------------------------

/// Login request sent by the Relying Party (Mock App) to AB.
#[derive(Debug, Serialize, Deserialize)]
pub struct LoginRequest {
    pub app_id: String,
}

// ---------------------------------------------------------------------------
// Step 3 – AB -> IB  (only the blinded app_id crosses this boundary)
// ---------------------------------------------------------------------------

/// Payload forwarded from AB to IB.
/// Contains **only** the blinded `app_id`: the plaintext `app_id`/`client_id`
/// is NEVER sent, so the IB cannot learn the destination application. (The JWT
/// `aud` is set downstream by the tTS to the blinded app id, not the client_id.)
#[derive(Debug, Serialize, Deserialize)]
pub struct BlindedLoginRequest {
    pub blind_app_id: String,
    /// AB session id used by the tTS to deliver the minted token back to AB.
    pub session_id: String,
    /// OIDC nonce (anti-replay, random value: not privacy-sensitive)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nonce: Option<String>,
    /// Application-layer authentication tag over (blind_app_id, session_id,
    /// nonce), set by AB and verified by IB (see `crypto::authorize_signing_bytes`
    /// / `crypto::AUTHORIZE_MAC_KEY_DEMO`). Makes the integrity of the blinded
    /// app id independent of TLS on the AB->IB hop.
    pub sig: String,
}

// ---------------------------------------------------------------------------
// Step 4 – Mock IdP -> IB
// ---------------------------------------------------------------------------

/// Simulated ID Token returned by the Mock IdP.
#[derive(Debug, Serialize, Deserialize)]
pub struct IdpResponse {
    pub iss: String,
    pub sub: String,
    /// User's display name (profile claim).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// User's email address (profile claim).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// Whether the email has been verified by the IdP.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email_verified: Option<bool>,
    /// URL to the user's profile picture (profile claim).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub picture: Option<String>,
}

// ---------------------------------------------------------------------------
// Step 6 – IB -> tTS  (only blinded values cross this boundary)
// ---------------------------------------------------------------------------

/// Payload sent from IB to the tTS Leader.
/// The identity fields are **blinded**: the tTS never sees plaintext `iss`,
/// `sub` or `app_id`, and no plaintext `client_id`/`aud` is sent (the JWT `aud`
/// is set to the blinded app id by the tTS).
///
/// CAVEAT: the profile claims (name, email, picture) currently travel in
/// plaintext so the tTS can embed them in the JWT. `email`/`name` are
/// effectively the real-world identity (`idp_user`), so in the strict
/// unlinkability model they should NOT be visible to the tTS (nor AB).
/// Therefore the profile claims are NOT carried here: the IB extracts them
/// from the IdP but does not forward them, so the tTS sees only blinded values.
/// Releasing verified attributes (email, name, ...) to the RP without exposing
/// them to the broker would be an orthogonal end-to-end-encrypted exchange
/// (future work).
#[derive(Debug, Serialize, Deserialize)]
pub struct TokenRequest {
    pub blind_app_id: String,
    pub blind_iss: String,
    pub blind_sub: String,
    /// AB session id: the tTS uses it to deliver the token straight to AB.
    pub session_id: String,
    /// OIDC nonce (anti-replay)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nonce: Option<String>,
    /// Application-layer authentication tag over the whole envelope
    /// (`crypto::issue_signing_bytes` of the fields above), set by the IB and
    /// verified by the tTS. This makes integrity/authenticity of the blinded
    /// tuple independent of TLS: even if the transport is terminated or a CA is
    /// compromised, a network attacker cannot tamper with `blind_*`,
    /// `session_id` or `nonce`, nor forge an `/issue` call, without the shared
    /// signing key. In a full deployment this is an Ed25519 signature under the
    /// IB's key; here it is an HMAC (see `crypto::ISSUE_MAC_KEY_DEMO`).
    pub sig: String,
}

// ---------------------------------------------------------------------------
// tTS -> AB  (the signed token is delivered straight to AB, never via IB)
// ---------------------------------------------------------------------------

/// Token delivery pushed by the tTS to AB's `/deliver` endpoint, correlated to
/// the AB session. The token never transits IB. The DI is not sent separately:
/// it is the `sub` claim inside the JWT.
#[derive(Debug, Serialize, Deserialize)]
pub struct TokenDelivery {
    pub session_id: String,
    pub jwt: String,
    /// Authentication tag over (session_id, jwt), set by the tTS and verified by
    /// AB (see `crypto::delivery_signing_bytes` / `crypto::DELIVERY_MAC_KEY_DEMO`).
    /// The JWT is already FROST-signed; this tag additionally binds it to the
    /// session_id it must be delivered to, so a broken transport cannot route a
    /// valid token to the wrong AB session.
    pub sig: String,
}

/// Final response carrying the signed JWT (AB -> App). The DI is the `sub`
/// claim inside the JWT, not a separate field.
#[derive(Debug, Serialize, Deserialize)]
pub struct TokenResponse {
    pub jwt: String,
}
