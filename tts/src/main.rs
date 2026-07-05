//! tTS: Threshold Token Service (Microsserviço 3 / Quórum)
//!
//! HTTPS server on port 5001.
//!
//! **Visibility:**
//!   - ONLY sees blinded values (`blind_app_id`, `blind_iss`, `blind_sub`)
//!   - NEVER sees `app_id`, `iss`, or `sub` in plaintext
//!
//! Uses **real FROST (Flexible Round-Optimized Schnorr Threshold) signatures**
//! via `frost-ed25519` (RFC 9591).
//!
//! - **Key generation**: Trusted dealer DKG (`generate_with_dealer`)
//! - **Signing**: Full 2-round FROST protocol (commit -> sign -> aggregate)
//! - **Parameters**: N=3 signers, threshold t=2
//!
//! The resulting signature is a standard Schnorr/Ed25519 group signature
//! verifiable with the single group public key.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use frost_ed25519 as frost;
use rand_core::{OsRng, RngCore};
use shared::types::{TokenDelivery, TokenRequest, TokenResponse};
use std::collections::{BTreeMap, HashMap};
use std::env;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{Notify, RwLock};

const MAX_SIGNERS: u16 = 3;
const MIN_SIGNERS: u16 = 2;
const PENDING_TTL_SECS: u64 = 30; // minted-token pickup window (AB long-poll)
const AWAIT_TIMEOUT_SECS: u64 = 5; // how long /await holds the connection open

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

struct TtsState {
    /// FROST key packages for each signer (in a real deployment, each
    /// signer would hold only their own KeyPackage).
    key_packages: BTreeMap<frost::Identifier, frost::keys::KeyPackage>,
    /// The public key package containing the group verifying key.
    pubkey_package: frost::keys::PublicKeyPackage,
    /// Secret key for the DI keyed-PRF (HMAC-SHA256). Held only by the tTS so
    /// that no party observing the blinded tuple (e.g. IB) can derive the DI.
    /// In a full deployment this would be a distributed PRF key in shares.
    di_key: Vec<u8>,
    /// Shared key used to verify the IB -> tTS issuance envelope tag. Issuance
    /// is rejected unless the tag matches, so the blinded tuple's integrity does
    /// not depend on TLS alone (see `shared::crypto::ISSUE_MAC_KEY_DEMO`).
    issue_sig_key: Vec<u8>,
    /// Shared key used to SIGN the tTS -> AB delivery envelope (step 18), binding
    /// the minted JWT to its target session_id (see `crypto::DELIVERY_MAC_KEY_DEMO`).
    delivery_sig_key: Vec<u8>,
    /// HTTPS client used to deliver the minted token straight to AB (push mode).
    http_client: reqwest::Client,
    /// AB endpoint that receives the minted token (push mode; tTS -> AB).
    ab_deliver_url: String,
    /// Delivery mode. Default (false) = push to AB /deliver (the SSD flow).
    /// When true (MOSHI_LONGPOLL=1, used by the RQ3 load test) the token is
    /// stored here and AB collects it via long-poll GET /await/{session_id},
    /// avoiding the per-request reentrant push to AB.
    longpoll: bool,
    /// Minted tokens awaiting AB long-poll pickup (longpoll mode only).
    pending: RwLock<HashMap<String, (TokenResponse, Instant)>>,
    /// Per-session wakers for long-poll `/await` (longpoll mode). A waiting AB
    /// parks on its session's `Notify` instead of busy-polling, and `/issue`
    /// fires it once the token is stored. This removes the 20 ms poll loop that
    /// hammered the `pending` write lock and serialised the `/issue` inserts
    /// under load (the source of the RQ3 throughput collapse).
    waiters: RwLock<HashMap<String, Arc<Notify>>>,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// POST /issue: receives blinded tuple, computes DI, signs JWT via FROST.
async fn issue(
    State(state): State<Arc<TtsState>>,
    Json(req): Json<TokenRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    tracing::info!("──────────────────────────────────────────────────");
    tracing::info!(
        blind_app_id = %req.blind_app_id,
        blind_iss    = %req.blind_iss,
        blind_sub    = %req.blind_sub,
        "[tTS] Step 7: Received blinded tuple (tTS CANNOT see plaintext)"
    );

    // ── Step 7.0: Verify the IB's envelope tag (app-layer integrity) ──
    // Reject before doing any work if the tag is missing/invalid: this stops a
    // tampered blinded tuple or a forged /issue (e.g. from a party that knows a
    // session_id but not the signing key) from ever minting a token, even if
    // the TLS leg is terminated or a CA is compromised.
    let signed = shared::crypto::issue_signing_bytes(
        &req.blind_app_id,
        &req.blind_iss,
        &req.blind_sub,
        &req.session_id,
        req.nonce.as_deref(),
    );
    if !shared::crypto::verify_mac(&state.issue_sig_key, &signed, &req.sig) {
        tracing::warn!("[tTS] Rejected /issue: invalid envelope signature");
        return Err((
            StatusCode::UNAUTHORIZED,
            "invalid issuance envelope signature".to_string(),
        ));
    }
    tracing::info!("[tTS] Envelope signature verified (integrity independent of TLS)");

    // ── Step 7a: Compute Deterministic Identifier (keyed PRF) ─────────
    let di = shared::crypto::compute_di(
        &state.di_key,
        &req.blind_app_id,
        &req.blind_iss,
        &req.blind_sub,
    );
    tracing::info!(di = %di, "[tTS] Step 7a: DI computed: HMAC-SHA256(k_tTS, blind_app||blind_iss||blind_sub)");

    // ── Step 7b: Build JWT payload ───────────────────────────────────
    let header = r#"{"alg":"EdDSA","typ":"JWT"}"#;
    let header_b64 = URL_SAFE_NO_PAD.encode(header.as_bytes());

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock")
        .as_secs();

    // aud = the BLINDED app id (opaque). The plaintext client_id never reaches
    // the tTS, so the JWT carries no app-identifying value in the clear;
    // app-binding is enforced by AB via the registered redirect_uri on delivery.
    let aud_value = req.blind_app_id.clone();

    // Build payload with optional OIDC fields
    let mut payload = serde_json::json!({
        "sub": di,
        "iss": "tts_prototype",
        "aud": aud_value,
        "iat": now,
        "exp": now + 3600
    });

    // Add OIDC nonce if present (anti-replay)
    if let Some(nonce) = &req.nonce {
        payload["nonce"] = serde_json::Value::String(nonce.clone());
        tracing::info!("[tTS] Step 7b: OIDC nonce included in JWT payload");
    }

    // Add jti (unique token identifier): 16 random bytes from the OS CSPRNG,
    // so it is unpredictable and collision-free even within the same second.
    let mut jti_bytes = [0u8; 16];
    OsRng.fill_bytes(&mut jti_bytes);
    let jti = format!("jti_{}", hex::encode(jti_bytes));
    payload["jti"] = serde_json::Value::String(jti);

    // NOTE: profile claims (name/email/picture) are deliberately NOT included.
    // email/name are effectively the real-world identity (idp_user); embedding
    // them here would expose them to the tTS and AB, breaking unlinkability.
    // The token carries only the pseudonymous DI (sub). Releasing verified
    // attributes to the RP is an orthogonal E2E-encrypted exchange (future work).

    let payload_b64 = URL_SAFE_NO_PAD.encode(payload.to_string().as_bytes());
    let signing_input = format!("{}.{}", header_b64, payload_b64);
    let message = signing_input.as_bytes();

    // ── Step 7c: FROST threshold signing (REAL protocol) ─────────────
    tracing::info!(
        "[tTS] ┌─── FROST Threshold Signing Ceremony (t={}, n={}) ───┐",
        MIN_SIGNERS,
        MAX_SIGNERS
    );

    let mut rng = OsRng;

    // Select t signers (first MIN_SIGNERS participants) to form the quorum
    let signer_ids: Vec<frost::Identifier> = state
        .key_packages
        .keys()
        .take(MIN_SIGNERS as usize)
        .cloned()
        .collect();

    tracing::info!(
        "[tTS] │ Quorum formed: signers {:?}",
        signer_ids
            .iter()
            .map(|id| format!("{id:?}"))
            .collect::<Vec<_>>()
    );

    // ── FROST Round 1: Each signer generates nonces and commitments ──
    tracing::info!("[tTS] │ Round 1: Generating nonces and commitments ...");

    let mut nonces_map = BTreeMap::new();
    let mut commitments_map = BTreeMap::new();

    for id in &signer_ids {
        let key_package = &state.key_packages[id];
        let (nonces, commitments) = frost::round1::commit(key_package.signing_share(), &mut rng);
        tracing::info!("[tTS] │   Signer {id:?}: commitment generated");
        nonces_map.insert(*id, nonces);
        commitments_map.insert(*id, commitments);
    }

    // ── Coordinator builds SigningPackage ─────────────────────────────
    let signing_package = frost::SigningPackage::new(commitments_map, message);
    tracing::info!(
        "[tTS] │ Coordinator: SigningPackage built with {} commitments",
        MIN_SIGNERS
    );

    // ── FROST Round 2: Each signer produces a signature share ────────
    tracing::info!("[tTS] │ Round 2: Generating signature shares ...");

    let mut signature_shares = BTreeMap::new();

    for id in &signer_ids {
        let key_package = &state.key_packages[id];
        let nonces = &nonces_map[id];
        let share = frost::round2::sign(&signing_package, nonces, key_package)
            .expect("FROST round2::sign failed");
        tracing::info!("[tTS] │   Signer {id:?}: signature share produced");
        signature_shares.insert(*id, share);
    }

    // ── Aggregation: combine shares into group signature ─────────────
    tracing::info!(
        "[tTS] │ Aggregation: Combining {} signature shares ...",
        signature_shares.len()
    );

    let group_signature =
        frost::aggregate(&signing_package, &signature_shares, &state.pubkey_package)
            .expect("FROST aggregate failed");

    // ── Verify the group signature locally ───────────────────────────
    let verify_result = state
        .pubkey_package
        .verifying_key()
        .verify(message, &group_signature);

    match &verify_result {
        Ok(()) => tracing::info!("[tTS] │ Verification: Group signature VALID"),
        Err(e) => tracing::error!("[tTS] │ Verification: Group signature INVALID: {e}"),
    }
    verify_result.expect("FROST group signature verification failed");

    tracing::info!("[tTS] └─── FROST Ceremony Complete ────────────────────────┘");

    // ── Build the JWT ────────────────────────────────────────────────
    let sig_bytes = group_signature
        .serialize()
        .expect("serialize FROST signature");
    let sig_b64 = URL_SAFE_NO_PAD.encode(&sig_bytes);
    let jwt = format!("{}.{}", signing_input, sig_b64);

    // ── Step 8: Deliver the token to AB (never via IB) ──────────────
    if state.longpoll {
        // RQ3/perf mode: store for AB to collect via long-poll. No reentrant
        // push, so the AB never has to serve an extra inbound request per login.
        tracing::info!(
            di_prefix = %&di[..16],
            session_id = %req.session_id,
            "[tTS] Step 8: JWT signed. Stored for AB long-poll pickup."
        );
        state
            .pending
            .write()
            .await
            .insert(req.session_id.clone(), (TokenResponse { jwt }, Instant::now()));
        // Wake an AB already long-polling for this session. `notify_one` stores a
        // permit, so there is no lost-wakeup race if `/await` parks just after.
        if let Some(n) = state.waiters.read().await.get(&req.session_id) {
            n.notify_one();
        }
        return Ok(Json(serde_json::json!({ "stored": true })));
    }

    // Default (SSD flow): push the token straight to AB.
    tracing::info!(
        di_prefix = %&di[..16],
        session_id = %req.session_id,
        "[tTS] Step 8: JWT signed with FROST group signature. Delivering directly to AB."
    );

    // Sign the delivery so AB can confirm this exact JWT was minted for this
    // session_id (the JWT signature alone does not bind it to a session).
    let delivery_sig = shared::crypto::mac(
        &state.delivery_sig_key,
        &shared::crypto::delivery_signing_bytes(&req.session_id, &jwt),
    );
    let delivery = TokenDelivery {
        session_id: req.session_id.clone(),
        jwt,
        sig: delivery_sig,
    };

    let resp = state
        .http_client
        .post(&state.ab_deliver_url)
        .json(&delivery)
        .send()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("AB unreachable: {e}")))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err((
            StatusCode::BAD_GATEWAY,
            format!("AB deliver error {status}: {body}"),
        ));
    }

    Ok(Json(serde_json::json!({ "delivered": true })))
}

/// GET /await/:session_id: long-poll used by AB in longpoll mode. Holds the
/// connection open until the minted token for this session is ready (it almost
/// always already is, since the IB->tTS /issue completed before AB calls
/// here), then returns it once. Times out after AWAIT_TIMEOUT_SECS.
async fn await_token(
    State(state): State<Arc<TtsState>>,
    Path(session_id): Path<String>,
) -> Result<Json<TokenResponse>, (StatusCode, String)> {
    // Register (or reuse) this session's waker *before* the first check, so a
    // token stored concurrently by `/issue` cannot be missed.
    let notify = {
        let mut w = state.waiters.write().await;
        w.entry(session_id.clone())
            .or_insert_with(|| Arc::new(Notify::new()))
            .clone()
    };
    let deadline = Instant::now() + Duration::from_secs(AWAIT_TIMEOUT_SECS);
    let result = loop {
        if let Some((token, _)) = state.pending.write().await.remove(&session_id) {
            break Ok(Json(token));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break Err((
                StatusCode::NOT_FOUND,
                "token not ready (await timeout)".into(),
            ));
        }
        // Park until `/issue` stores the token (wakes us) or the timeout elapses
        //: no polling, no lock contention with the insert path.
        let _ = tokio::time::timeout(remaining, notify.notified()).await;
    };
    // Drop our waker so the map cannot grow unbounded.
    state.waiters.write().await.remove(&session_id);
    result
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    shared::tls::install_default_provider();
    shared::tls::ensure_certs("certs").expect("Failed to generate TLS certificates");

    let mut rng = OsRng;

    // ── FROST Distributed Key Generation (Trusted Dealer) ────────────
    tracing::info!("┌─── FROST Key Generation (Trusted Dealer) ───────────┐");
    tracing::info!("│ Parameters: max_signers={MAX_SIGNERS}, min_signers(threshold)={MIN_SIGNERS}");

    let (shares, pubkey_package) = frost::keys::generate_with_dealer(
        MAX_SIGNERS,
        MIN_SIGNERS,
        frost::keys::IdentifierList::Default,
        &mut rng,
    )
    .expect("FROST key generation failed");

    // Convert secret shares into KeyPackages (one per signer)
    let mut key_packages = BTreeMap::new();
    for (id, secret_share) in &shares {
        let key_package =
            frost::keys::KeyPackage::try_from(secret_share.clone()).expect("invalid secret share");
        tracing::info!("│ Signer {id:?}: KeyPackage created");
        key_packages.insert(*id, key_package);
    }

    // Export the group verifying key for external verification (e.g. Mock App)
    let vk = pubkey_package.verifying_key();
    let vk_bytes = vk.serialize().expect("serialize verifying key");
    let vk_hex = hex::encode(&vk_bytes);
    std::fs::write("certs/tts_verifying.pub", &vk_hex).expect("write verifying key");

    tracing::info!("│ Group verifying key: {vk_hex}");
    tracing::info!("│ Exported to certs/tts_verifying.pub");
    tracing::info!("└──────────────────────────────────────────────────────┘");

    // Generate the secret DI keyed-PRF key (held only by the tTS).
    let mut di_key = vec![0u8; 32];
    OsRng.fill_bytes(&mut di_key);
    tracing::info!("│ DI keyed-PRF key generated (32 bytes, tTS-only)");

    // HTTPS client to deliver the minted token straight to AB.
    let ca_pem = std::fs::read("certs/ca.pem").expect("read CA cert");
    let ca_cert = reqwest::Certificate::from_pem(&ca_pem).expect("parse CA cert");
    let http_client = reqwest::Client::builder()
        .add_root_certificate(ca_cert)
        .build()
        .expect("build HTTP client");
    let ab_deliver_url =
        env::var("AB_DELIVER_URL").unwrap_or_else(|_| "https://localhost:4001/deliver".to_string());
    let longpoll = matches!(env::var("MOSHI_LONGPOLL").as_deref(), Ok("1") | Ok("true"));
    tracing::info!(
        "│ Delivery mode: {} (AB push endpoint: {})",
        if longpoll { "long-poll (/await)" } else { "push (/deliver)" },
        ab_deliver_url
    );

    let state = Arc::new(TtsState {
        key_packages,
        pubkey_package,
        di_key,
        issue_sig_key: shared::crypto::ISSUE_MAC_KEY_DEMO.to_vec(),
        delivery_sig_key: shared::crypto::DELIVERY_MAC_KEY_DEMO.to_vec(),
        http_client,
        ab_deliver_url,
        longpoll,
        pending: RwLock::new(HashMap::new()),
        waiters: RwLock::new(HashMap::new()),
    });

    // Background sweeper for expired pending tokens (longpoll mode).
    {
        let gc = state.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(10));
            loop {
                tick.tick().await;
                gc.pending
                    .write()
                    .await
                    .retain(|_, (_, at)| at.elapsed().as_secs() < PENDING_TTL_SECS);
            }
        });
    }

    let app = Router::new()
        .route("/issue", post(issue))
        .route("/await/:session_id", get(await_token))
        .with_state(state);
    let addr = SocketAddr::from(([127, 0, 0, 1], 5001));

    let tls_config =
        axum_server::tls_rustls::RustlsConfig::from_pem_file("certs/tts.pem", "certs/tts.key")
            .await
            .expect("Failed to load tTS TLS config");

    tracing::info!(
        "[tTS] Threshold Token Service listening on https://{}",
        addr
    );
    tracing::info!("[tTS] Visibility: ONLY blinded values | NEVER sees plaintext");
    tracing::info!(
        "[tTS] FROST: real threshold signing (t={}, n={})",
        MIN_SIGNERS,
        MAX_SIGNERS
    );

    axum_server::bind_rustls(addr, tls_config)
        .serve(app.into_make_service())
        .await
        .expect("tTS server error");
}
