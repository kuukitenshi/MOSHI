//! AB: App-Facing Broker (Microsservico 1)
//!
//! **Two listeners:**
//!   - HTTPS on port 4001: inter-service (token exchange, discovery, JWKS)
//!   - HTTP  on port 4010: browser-facing (Google OAuth redirect chain)
//!
//! **Visibility:**
//!   - KNOWS  `app_id` (in plaintext)
//!   - NEVER SEES `iss` or `sub`
//!
//! Blinds `app_id` via PRF (HMAC-SHA256) before forwarding to IB.
//!
//! ## HTTPS Endpoints (:4001)
//!
//! - `GET  /.well-known/openid-configuration`: OIDC discovery
//! - `GET  /jwks.json`: FROST group public key as JWK
//! - `GET  /authorize`: OIDC authorize (implicit & code flow + PKCE): proxied/sync
//! - `POST /token`: OIDC token exchange (code flow)
//! - `POST /login`: original direct API (backwards compatible)
//!
//! ## HTTP Endpoints (:4010): browser-facing
//!
//! - `GET /authorize`: starts Google redirect flow (blinds app_id, redirects to IB)
//! - `GET /callback`: receives relay from IB, generates auth code, redirects to demo_app

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::Rng;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use shared::types::{BlindedLoginRequest, LoginRequest, TokenDelivery, TokenResponse};
use std::collections::HashMap;
use std::env;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;

#[derive(Debug, Clone)]
struct AbConfig {
    ib_https_base: String,
    ib_http_base: String,
    /// tTS base URL: only used in longpoll mode (RQ3) for GET /await.
    tts_base: String,
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    jwks_uri: String,
    demo_app_redirect_uri: String,
    web_ui_redirect_uri: String,
    web_ui_root_uri: String,
}

fn env_or_default(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_string())
}

// ---------------------------------------------------------------------------
// Client Registry (static: in production, this would be a database)
// ---------------------------------------------------------------------------

/// Registered client with allowed redirect URIs.
struct RegisteredClient {
    redirect_uris: Vec<String>,
}

/// Static client registry. Any client_id not listed here is rejected.
/// In production, this would be backed by a database or admin API.
fn get_registered_client(client_id: &str) -> Option<RegisteredClient> {
    let demo_app_redirect =
        env_or_default("DEMO_APP_REDIRECT_URI", "http://localhost:3000/callback");
    let web_ui_callback = env_or_default("WEB_UI_CALLBACK_URI", "http://localhost:8080/callback");
    let web_ui_root = env_or_default("WEB_UI_ROOT_URI", "http://localhost:8080/");

    match client_id {
        "demo_app_hello" => Some(RegisteredClient {
            redirect_uris: vec![demo_app_redirect],
        }),
        "web_ui" => Some(RegisteredClient {
            redirect_uris: vec![web_ui_callback.clone(), web_ui_root],
        }),
        "app_example_456" => Some(RegisteredClient {
            redirect_uris: vec![web_ui_callback],
        }),
        // Allow any client_id starting with "test_" for development convenience
        _ if client_id.starts_with("test_") => Some(RegisteredClient {
            redirect_uris: vec![
                env_or_default("DEMO_APP_REDIRECT_URI", "http://localhost:3000/callback"),
                env_or_default("WEB_UI_CALLBACK_URI", "http://localhost:8080/callback"),
            ],
        }),
        _ => None,
    }
}

/// Validate that a client_id is registered and the redirect_uri is allowed.
fn validate_client(client_id: &str, redirect_uri: &str) -> Result<(), String> {
    let client = get_registered_client(client_id)
        .ok_or_else(|| format!("unregistered client_id: {client_id}"))?;

    if !client.redirect_uris.iter().any(|u| u == redirect_uri) {
        return Err(format!(
            "redirect_uri not registered for client {client_id}: {redirect_uri}"
        ));
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Authorization Code storage (ephemeral, TTL ~120s)
// ---------------------------------------------------------------------------

/// Stored data for a pending authorization code.
#[derive(Debug, Clone)]
struct PendingCode {
    client_id: String,
    redirect_uri: String,
    code_challenge: String,
    nonce: Option<String>,
    created_at: Instant,
    /// Pre-computed JWT (from Google redirect flow). If present, /token returns
    /// this directly instead of calling run_flow().
    completed_jwt: Option<String>,
}

/// TTL for authorization codes (2 minutes).
const CODE_TTL_SECS: u64 = 120;

// ---------------------------------------------------------------------------
// Pending Session storage (for browser redirect flow)
// ---------------------------------------------------------------------------

/// Data stored while the user is being redirected through IB → Google → back.
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct PendingSession {
    client_id: String,
    redirect_uri: String,
    code_challenge: String,
    code_challenge_method: String,
    nonce: Option<String>,
    state: Option<String>,
    blind_app_id: String,
    created_at: Instant,
}

const SESSION_TTL_SECS: u64 = 300; // 5 min for Google auth
const DELIVERY_TTL_SECS: u64 = 30; // tTS token pickup window (normally sub-second)

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

struct AppState {
    http_client: reqwest::Client,
    blinding_key: Vec<u8>,
    /// Key used to sign the AB -> IB authorize envelope (step 5).
    authorize_sig_key: Vec<u8>,
    /// Key used to verify the tTS -> AB delivery envelope (step 18).
    delivery_sig_key: Vec<u8>,
    config: AbConfig,
    /// Ephemeral storage for authorization codes (code -> PendingCode).
    /// Cleaned up periodically and on each access.
    pending_codes: RwLock<HashMap<String, PendingCode>>,
    /// Ephemeral storage for browser redirect sessions (session_id -> PendingSession).
    pending_sessions: RwLock<HashMap<String, PendingSession>>,
    /// Tokens delivered by the tTS, keyed by session_id, awaiting pickup
    /// (push mode: populated by POST /deliver).
    deliveries: RwLock<HashMap<String, (TokenResponse, Instant)>>,
    /// Delivery mode. Default (false) = push: the tTS POSTs the token to AB
    /// /deliver. When true (MOSHI_LONGPOLL=1, RQ3 load test) AB instead
    /// collects the token from the tTS via long-poll GET /await/{session_id}.
    longpoll: bool,
}

// ---------------------------------------------------------------------------
// Core: blind + call IB (shared by /login, /authorize sync, /token sync)
// ---------------------------------------------------------------------------

/// Execute the partitioned flow: blind app_id, call IB, return JWT.
async fn run_flow(
    state: &AppState,
    app_id: &str,
    nonce: Option<String>,
) -> Result<TokenResponse, (StatusCode, String)> {
    // ── Blind the app_id ─────────────────────────────────────────────
    let blind_app_id = shared::crypto::blind(&state.blinding_key, app_id.as_bytes());
    tracing::info!(
        blind_app_id = %blind_app_id,
        "[AB] app_id blinded (original will NOT be forwarded)"
    );

    // Fresh session id correlates this flow with the tTS delivery.
    let session_id = hex::encode(rand::thread_rng().gen::<[u8; 16]>());

    // ── Forward to IB (only the blinded app_id + session crosses) ─────
    // IB blinds the origin and calls the tTS; the tTS delivers the signed
    // token straight back to AB (POST /deliver), so neither the DI nor the
    // token ever transit IB.
    // Authenticate the AB->IB envelope so its integrity does not rest on TLS
    // alone (IB rejects any mismatch before contacting an IdP).
    let sig = shared::crypto::mac(
        &state.authorize_sig_key,
        &shared::crypto::authorize_signing_bytes(&blind_app_id, &session_id, nonce.as_deref()),
    );
    let payload = BlindedLoginRequest {
        blind_app_id,
        session_id: session_id.clone(),
        nonce,
        sig,
    };

    let resp = state
        .http_client
        .post(format!("{}/process", state.config.ib_https_base))
        .json(&payload)
        .send()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("IB unreachable: {e}")))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err((
            StatusCode::BAD_GATEWAY,
            format!("IB error {status}: {body}"),
        ));
    }

    // Obtain the minted token for this session (push: already at /deliver;
    // longpoll: collected from the tTS).
    collect_token(state, &session_id).await
}

/// Obtain the minted token for this session. In push mode (default) the tTS
/// already POSTed it to AB's `/deliver`; in longpoll mode (RQ3) AB collects it
/// from the tTS via GET `/await/{session_id}`, so the tTS never opens a
/// reentrant connection back to AB.
async fn collect_token(
    state: &AppState,
    session_id: &str,
) -> Result<TokenResponse, (StatusCode, String)> {
    if state.longpoll {
        let url = format!("{}/await/{}", state.config.tts_base, session_id);
        let resp = state
            .http_client
            .get(&url)
            .send()
            .await
            .map_err(|e| (StatusCode::BAD_GATEWAY, format!("tTS unreachable: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err((
                StatusCode::BAD_GATEWAY,
                format!("tTS await error {status}: {body}"),
            ));
        }
        return resp
            .json()
            .await
            .map_err(|e| (StatusCode::BAD_GATEWAY, format!("Invalid tTS await response: {e}")));
    }
    take_delivery(state, session_id).await
}

/// Pull the token the tTS pushed for this session (push mode). The tTS awaits
/// AB's `/deliver` before responding to IB, so the delivery is already present
/// by the time the IB call returns.
async fn take_delivery(
    state: &AppState,
    session_id: &str,
) -> Result<TokenResponse, (StatusCode, String)> {
    // O(1) remove: GC is handled out-of-band by the background sweeper.
    state
        .deliveries
        .write()
        .await
        .remove(session_id)
        .map(|(token, _)| token)
        .ok_or((
            StatusCode::BAD_GATEWAY,
            "tTS delivery missing for session".into(),
        ))
}

/// POST /deliver: the tTS pushes the minted token here (tTS -> AB, never IB).
async fn deliver(State(state): State<Arc<AppState>>, Json(d): Json<TokenDelivery>) -> StatusCode {
    // Verify the tTS's delivery tag before storing: this binds the JWT to the
    // session_id it was minted for, so a tampered delivery cannot route a valid
    // token to the wrong AB session even if the transport is broken.
    if !shared::crypto::verify_mac(
        &state.delivery_sig_key,
        &shared::crypto::delivery_signing_bytes(&d.session_id, &d.jwt),
        &d.sig,
    ) {
        tracing::warn!(session_id = %d.session_id, "[AB] Rejected /deliver: invalid delivery signature");
        return StatusCode::UNAUTHORIZED;
    }
    tracing::info!(
        session_id = %d.session_id,
        "[AB] Token delivered by tTS"
    );
    // O(1) insert: no per-request GC scan (that serialised the write lock
    // under load and stopped the map from draining). A background task GCs.
    state.deliveries.write().await.insert(
        d.session_id,
        (TokenResponse { jwt: d.jwt }, Instant::now()),
    );
    StatusCode::OK
}

// ---------------------------------------------------------------------------
// POST /login: original direct API (backwards compatible)
// ---------------------------------------------------------------------------

async fn login(
    State(state): State<Arc<AppState>>,
    Json(req): Json<LoginRequest>,
) -> Result<Json<TokenResponse>, (StatusCode, String)> {
    tracing::info!("══════════════════════════════════════════════════");
    tracing::info!(app_id = %req.app_id, "[AB] POST /login: direct API call");

    let token = run_flow(&state, &req.app_id, None).await?;

    tracing::info!("[AB] Delivering JWT to App (AB never saw iss or sub)");
    Ok(Json(token))
}

// ---------------------------------------------------------------------------
// OIDC: GET /.well-known/openid-configuration
// ---------------------------------------------------------------------------

async fn openid_configuration(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    Json(serde_json::json!({
        "issuer": state.config.issuer.clone(),
        "authorization_endpoint": state.config.authorization_endpoint.clone(),
        "token_endpoint": state.config.token_endpoint.clone(),
        "jwks_uri": state.config.jwks_uri.clone(),
        "response_types_supported": ["id_token", "code"],
        "response_modes_supported": ["fragment", "form_post", "query"],
        "subject_types_supported": ["pairwise"],
        "id_token_signing_alg_values_supported": ["EdDSA"],
        "scopes_supported": ["openid", "profile", "email"],
        "claims_supported": ["sub", "iss", "aud", "iat", "exp", "nonce", "jti", "name", "email", "email_verified", "picture"],
        "grant_types_supported": ["implicit", "authorization_code"],
        "token_endpoint_auth_methods_supported": ["none"],
        "code_challenge_methods_supported": ["S256"],
        "_note": "moshi PoC: OIDC Implicit + Code Flow (PKCE)"
    }))
}

// ---------------------------------------------------------------------------
// OIDC: GET /jwks.json: FROST group public key
// ---------------------------------------------------------------------------

async fn jwks() -> impl IntoResponse {
    // Read the FROST group verifying key (hex-encoded Ed25519 public key)
    let vk_hex = match std::fs::read_to_string("certs/tts_verifying.pub") {
        Ok(h) => h.trim().to_string(),
        Err(e) => {
            tracing::error!("[AB] Cannot read FROST verifying key: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "verifying key unavailable"})),
            )
                .into_response();
        }
    };

    let vk_bytes = match hex::decode(&vk_hex) {
        Ok(b) => b,
        Err(e) => {
            tracing::error!("[AB] Invalid hex in verifying key: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "invalid verifying key"})),
            )
                .into_response();
        }
    };

    // Ed25519 public key → JWK format (OKP / Ed25519)
    let x_b64 = URL_SAFE_NO_PAD.encode(&vk_bytes);

    Json(serde_json::json!({
        "keys": [{
            "kty": "OKP",
            "crv": "Ed25519",
            "x": x_b64,
            "use": "sig",
            "alg": "EdDSA",
            "kid": "frost-group-key-1"
        }]
    }))
    .into_response()
}

// ---------------------------------------------------------------------------
// Client registration metadata: GET /client_blind_id?client_id=...
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct ClientBlindParams {
    client_id: String,
}

/// Return the blinded app id for a registered client. An RP learns its own
/// `blind_app_id` here once (at registration) and then validates that the
/// `aud` of the Hellō ID Token equals it: the token keeps a single FROST
/// group signature (no AB re-signing) and the RP never holds `k_AB`.
async fn client_blind_id(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ClientBlindParams>,
) -> impl IntoResponse {
    if get_registered_client(&params.client_id).is_none() {
        tracing::warn!(
            "[AB] /client_blind_id: unregistered client_id: {}",
            params.client_id
        );
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "unregistered client_id"})),
        )
            .into_response();
    }

    // Same key + input as run_flow/browser_authorize, so this equals the aud
    // the tTS stamped into the token: blind(k_AB, app_id).
    let blind_app_id = shared::crypto::blind(&state.blinding_key, params.client_id.as_bytes());
    Json(serde_json::json!({
        "client_id": params.client_id,
        "blind_app_id": blind_app_id,
    }))
    .into_response()
}

// ---------------------------------------------------------------------------
// OIDC: GET /authorize (HTTPS :4001): Implicit Flow + Sync Code Flow
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct AuthorizeParams {
    client_id: String,
    redirect_uri: String,
    #[serde(default = "default_response_type")]
    response_type: String,
    #[serde(default)]
    scope: String,
    #[serde(default)]
    nonce: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default = "default_response_mode")]
    response_mode: String,
    /// PKCE code challenge (required for code flow)
    #[serde(default)]
    code_challenge: Option<String>,
    /// PKCE code challenge method (must be S256)
    #[serde(default)]
    code_challenge_method: Option<String>,
}

fn default_response_type() -> String {
    "id_token".into()
}
fn default_response_mode() -> String {
    "fragment".into()
}

async fn authorize(
    State(state): State<Arc<AppState>>,
    Query(params): Query<AuthorizeParams>,
) -> impl IntoResponse {
    tracing::info!("══════════════════════════════════════════════════");
    tracing::info!(
        client_id = %params.client_id,
        redirect_uri = %params.redirect_uri,
        response_type = %params.response_type,
        response_mode = %params.response_mode,
        "[AB] OIDC /authorize (HTTPS: sync/proxied)"
    );

    // Validate scope contains openid
    if !params.scope.split_whitespace().any(|s| s == "openid") {
        let err = format!(
            "{}#error=invalid_scope&error_description=openid+scope+required",
            params.redirect_uri
        );
        return (StatusCode::FOUND, [("location", err)]).into_response();
    }

    // Validate client registration and redirect_uri
    if let Err(msg) = validate_client(&params.client_id, &params.redirect_uri) {
        tracing::warn!("[AB] Client validation failed: {msg}");
        // Per OIDC spec, if redirect_uri is invalid we must NOT redirect
        return (
            StatusCode::BAD_REQUEST,
            format!("Client validation error: {msg}"),
        )
            .into_response();
    }

    let state_param = params
        .state
        .as_deref()
        .map(|s| format!("&state={s}"))
        .unwrap_or_default();

    match params.response_type.as_str() {
        // ── Authorization Code Flow (PKCE) ───────────────────────────
        "code" => {
            tracing::info!("[AB] Authorization Code Flow (PKCE): sync mode");

            // Validate PKCE parameters
            let code_challenge = match &params.code_challenge {
                Some(cc) if !cc.is_empty() => cc.clone(),
                _ => {
                    let err = format!(
                        "{}?error=invalid_request&error_description=code_challenge+required{}",
                        params.redirect_uri, state_param
                    );
                    return (StatusCode::FOUND, [("location", err)]).into_response();
                }
            };

            match &params.code_challenge_method {
                Some(m) if m == "S256" => {}
                _ => {
                    let err = format!(
                        "{}?error=invalid_request&error_description=code_challenge_method+must+be+S256{}",
                        params.redirect_uri, state_param
                    );
                    return (StatusCode::FOUND, [("location", err)]).into_response();
                }
            }

            // Generate random authorization code (32 bytes hex = 64 chars)
            let code_bytes: [u8; 32] = rand::thread_rng().gen();
            let code = hex::encode(code_bytes);

            // Store the code with metadata
            {
                let mut codes = state.pending_codes.write().await;

                // Garbage-collect expired codes while we're here
                codes.retain(|_, v| v.created_at.elapsed().as_secs() < CODE_TTL_SECS);

                codes.insert(
                    code.clone(),
                    PendingCode {
                        client_id: params.client_id.clone(),
                        redirect_uri: params.redirect_uri.clone(),
                        code_challenge,
                        nonce: params.nonce.clone(),
                        created_at: Instant::now(),
                        completed_jwt: None,
                    },
                );
                tracing::info!(
                    code_prefix = %&code[..16],
                    ttl_secs = CODE_TTL_SECS,
                    "[AB] Authorization code stored (pending_codes count={})",
                    codes.len()
                );
            }

            // Redirect back to the app with the code
            let location = format!("{}?code={}{}", params.redirect_uri, code, state_param);
            tracing::info!("[AB] Redirecting with authorization code");
            (StatusCode::FOUND, [("location", location)]).into_response()
        }

        // ── Implicit Flow (id_token directly) ────────────────────────
        "id_token" => {
            tracing::info!("[AB] Implicit Flow");

            // Require a nonce: OIDC mandates it for the implicit flow, and it
            // binds the issued ID Token to this RP login session.
            if params.nonce.as_deref().map(str::is_empty).unwrap_or(true) {
                let err = format!(
                    "{}#error=invalid_request&error_description=nonce+is+required{}",
                    params.redirect_uri, state_param
                );
                return (StatusCode::FOUND, [("location", err)]).into_response();
            }

            // Run the partitioned flow immediately
            let result = run_flow(&state, &params.client_id, params.nonce.clone()).await;

            match result {
                Ok(token) => {
                    tracing::info!(
                        "[AB] OIDC flow complete: returning id_token via {}",
                        params.response_mode
                    );

                    match params.response_mode.as_str() {
                        "form_post" => {
                            let html = format!(
                                r#"<!DOCTYPE html>
<html><head><title>Submitting...</title></head>
<body onload="document.forms[0].submit()">
<form method="POST" action="{}">
<input type="hidden" name="id_token" value="{}">
<input type="hidden" name="token_type" value="bearer">{}
<noscript><button type="submit">Continue</button></noscript>
</form></body></html>"#,
                                params.redirect_uri,
                                token.jwt,
                                if let Some(st) = &params.state {
                                    format!(r#"<input type="hidden" name="state" value="{st}">"#)
                                } else {
                                    String::new()
                                }
                            );
                            Html(html).into_response()
                        }
                        _ => {
                            let location = format!(
                                "{}#id_token={}&token_type=bearer{}",
                                params.redirect_uri, token.jwt, state_param
                            );
                            (StatusCode::FOUND, [("location", location)]).into_response()
                        }
                    }
                }
                Err((_status, msg)) => {
                    let location = format!(
                        "{}#error=server_error&error_description={}",
                        params.redirect_uri,
                        urlencoding(&msg)
                    );
                    tracing::error!("[AB] OIDC flow failed: {msg}");
                    (StatusCode::FOUND, [("location", location)]).into_response()
                }
            }
        }

        other => {
            let err = format!(
                "{}#error=unsupported_response_type&error_description=unsupported:+{}{}",
                params.redirect_uri,
                urlencoding(other),
                state_param
            );
            (StatusCode::FOUND, [("location", err)]).into_response()
        }
    }
}

// ---------------------------------------------------------------------------
// OIDC: POST /token: Token exchange (Authorization Code + PKCE)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct TokenExchangeRequest {
    grant_type: String,
    code: String,
    redirect_uri: String,
    code_verifier: String,
    client_id: String,
}

async fn token_exchange(
    State(state): State<Arc<AppState>>,
    axum::Form(req): axum::Form<TokenExchangeRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    tracing::info!("══════════════════════════════════════════════════");
    tracing::info!(
        client_id = %req.client_id,
        "[AB] POST /token: Authorization Code exchange"
    );

    // Validate grant_type
    if req.grant_type != "authorization_code" {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "unsupported_grant_type",
                "error_description": "only authorization_code supported"
            })),
        ));
    }

    // Validate client registration
    if get_registered_client(&req.client_id).is_none() {
        tracing::warn!(
            "[AB] Token exchange: unregistered client_id: {}",
            req.client_id
        );
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "invalid_client",
                "error_description": "unregistered client_id"
            })),
        ));
    }

    // Look up and remove the authorization code (one-time use)
    let pending = {
        let mut codes = state.pending_codes.write().await;

        // GC expired codes
        codes.retain(|_, v| v.created_at.elapsed().as_secs() < CODE_TTL_SECS);

        codes.remove(&req.code)
    };

    let pending = match pending {
        Some(p) => p,
        None => {
            tracing::warn!("[AB] Invalid or expired authorization code");
            return Err((
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "invalid_grant",
                    "error_description": "authorization code invalid or expired"
                })),
            ));
        }
    };

    // Validate client_id matches
    if req.client_id != pending.client_id {
        tracing::warn!("[AB] client_id mismatch in token exchange");
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "invalid_grant",
                "error_description": "client_id mismatch"
            })),
        ));
    }

    // Validate redirect_uri matches
    if req.redirect_uri != pending.redirect_uri {
        tracing::warn!("[AB] redirect_uri mismatch in token exchange");
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "invalid_grant",
                "error_description": "redirect_uri mismatch"
            })),
        ));
    }

    // ── PKCE verification: base64url(SHA256(code_verifier)) == code_challenge
    let mut hasher = Sha256::new();
    hasher.update(req.code_verifier.as_bytes());
    let hash = hasher.finalize();
    let computed_challenge = URL_SAFE_NO_PAD.encode(hash);

    if computed_challenge != pending.code_challenge {
        tracing::warn!("[AB] PKCE code_verifier verification failed");
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "invalid_grant",
                "error_description": "PKCE verification failed"
            })),
        ));
    }

    tracing::info!("[AB] PKCE verification passed");

    // ── Check if JWT was pre-computed (Google redirect flow) ─────────
    if let Some(jwt) = pending.completed_jwt {
        tracing::info!("[AB] Returning pre-computed JWT from Google redirect flow");
        return Ok(Json(serde_json::json!({
            "id_token": jwt,
            "token_type": "bearer",
        })));
    }

    // ── Otherwise, execute the synchronous partitioned flow ──────────
    tracing::info!("[AB] Running synchronous partitioned flow (mock IdP path)");
    let token = run_flow(&state, &req.client_id, pending.nonce)
        .await
        .map_err(|(status, msg)| {
        (
            status,
            Json(serde_json::json!({
                "error": "server_error",
                "error_description": msg
            })),
        )
    })?;

    tracing::info!("[AB] Token exchange complete: returning id_token");

    // Return OIDC token response
    Ok(Json(serde_json::json!({
        "id_token": token.jwt,
        "token_type": "bearer",
    })))
}

/// Also accept JSON body for token exchange (some clients send JSON)
async fn token_exchange_json(
    State(state): State<Arc<AppState>>,
    Json(req): Json<TokenExchangeRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    // Reuse the same logic by converting to Form handling
    token_exchange(State(state), axum::Form(req)).await
}

// ---------------------------------------------------------------------------
// HTTP :4010: GET /authorize (browser redirect → IB → Google)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct BrowserAuthorizeParams {
    client_id: String,
    redirect_uri: String,
    #[serde(default)]
    response_type: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    nonce: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    code_challenge: Option<String>,
    #[serde(default)]
    code_challenge_method: Option<String>,
    #[serde(default = "default_provider")]
    provider: String,
}

fn default_provider() -> String {
    "google".to_string()
}

async fn browser_authorize(
    State(state): State<Arc<AppState>>,
    Query(params): Query<BrowserAuthorizeParams>,
) -> impl IntoResponse {
    tracing::info!("══════════════════════════════════════════════════");
    tracing::info!(
        client_id = %params.client_id,
        redirect_uri = %params.redirect_uri,
        provider = %params.provider,
        "[AB] Browser /authorize (HTTP :4010): {} redirect flow",
        params.provider
    );

    // Validate client registration
    if let Err(msg) = validate_client(&params.client_id, &params.redirect_uri) {
        tracing::warn!("[AB] Client validation failed: {msg}");
        return (
            StatusCode::BAD_REQUEST,
            format!("Client validation error: {msg}"),
        )
            .into_response();
    }

    // Validate PKCE parameters
    let code_challenge = match &params.code_challenge {
        Some(cc) if !cc.is_empty() => cc.clone(),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                "code_challenge is required".to_string(),
            )
                .into_response();
        }
    };

    let code_challenge_method = params
        .code_challenge_method
        .clone()
        .unwrap_or_else(|| "S256".into());

    if code_challenge_method != "S256" {
        return (
            StatusCode::BAD_REQUEST,
            "code_challenge_method must be S256".to_string(),
        )
            .into_response();
    }

    // Require a nonce: it binds the issued ID Token to this RP login session
    // (the RP generates it, sends it here, and verifies it on the returned
    // token). Without it the token would not be bound to any session.
    if params.nonce.as_deref().map(str::is_empty).unwrap_or(true) {
        return (
            StatusCode::BAD_REQUEST,
            "nonce is required (binds the ID Token to the RP session)".to_string(),
        )
            .into_response();
    }

    // ── Blind the app_id ─────────────────────────────────────────────
    let blind_app_id = shared::crypto::blind(&state.blinding_key, params.client_id.as_bytes());
    tracing::info!(
        blind_app_id = %blind_app_id,
        "[AB] app_id blinded for redirect flow (original will NOT leave AB)"
    );

    // ── Generate session_id and store pending session ─────────────────
    let session_id = hex::encode(rand::thread_rng().gen::<[u8; 16]>());

    {
        let mut sessions = state.pending_sessions.write().await;
        // GC expired sessions
        sessions.retain(|_, v| v.created_at.elapsed().as_secs() < SESSION_TTL_SECS);

        sessions.insert(
            session_id.clone(),
            PendingSession {
                client_id: params.client_id.clone(),
                redirect_uri: params.redirect_uri.clone(),
                code_challenge,
                code_challenge_method,
                nonce: params.nonce.clone(),
                state: params.state.clone(),
                blind_app_id: blind_app_id.clone(),
                created_at: Instant::now(),
            },
        );
        tracing::info!(
            session_id = %session_id,
            "[AB] Session stored (pending_sessions count={})",
            sessions.len()
        );
    }

    // ── Determine IB path based on provider ──────────────────────────
    let ib_path = match params.provider.as_str() {
        "github" => "/authorize/github",
        "discord" => "/authorize/discord",
        _ => "/authorize", // default to Google
    };

    // ── Redirect to IB HTTP :4020 /authorize[/{provider}] ─────────────
    let nonce_param = params
        .nonce
        .as_deref()
        .map(|n| format!("&nonce={n}"))
        .unwrap_or_default();

    // NOTE: the plaintext client_id is NOT forwarded to IB: only the blinded
    // app id crosses, so the IB cannot learn the destination application.
    // The redirect is front-channel (through the browser, over HTTP), so it
    // carries a tag over (blind_app_id, session_id, nonce) that the IB verifies:
    // the user-agent cannot tamper with the blinded app id or the handle.
    let sig = shared::crypto::mac(
        &state.authorize_sig_key,
        &shared::crypto::authorize_signing_bytes(&blind_app_id, &session_id, params.nonce.as_deref()),
    );
    let redirect_url = format!(
        "{}{}?session_id={}&blind_app_id={}{}&sig={}",
        state.config.ib_http_base, ib_path, session_id, blind_app_id, nonce_param, sig
    );

    tracing::info!("[AB] Redirecting browser to IB {} endpoint", ib_path);
    (StatusCode::FOUND, [("location", redirect_url)]).into_response()
}

// ---------------------------------------------------------------------------
// HTTP :4010: GET /callback (IB redirects back here after Google auth)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct CallbackParams {
    session_id: String,
}

async fn browser_callback(
    State(state): State<Arc<AppState>>,
    Query(params): Query<CallbackParams>,
) -> impl IntoResponse {
    tracing::info!("══════════════════════════════════════════════════");
    tracing::info!(
        session_id = %params.session_id,
        "[AB] Browser /callback: received from IB"
    );

    // ── Look up pending session ──────────────────────────────────────
    let session = {
        let mut sessions = state.pending_sessions.write().await;
        sessions.retain(|_, v| v.created_at.elapsed().as_secs() < SESSION_TTL_SECS);
        sessions.remove(&params.session_id)
    };

    let session = match session {
        Some(s) => s,
        None => {
            tracing::error!("[AB] Invalid or expired session_id");
            return (
                StatusCode::BAD_REQUEST,
                "Invalid or expired session".to_string(),
            )
                .into_response();
        }
    };

    // ── Obtain the token the tTS minted for this session ──────────────
    // Push mode: already delivered to AB /deliver before the redirect.
    // Longpoll mode: collected from the tTS. Neither the DI nor the token
    // ever transited IB in either mode.
    let token = match collect_token(&state, &params.session_id).await {
        Ok(t) => t,
        Err((status, msg)) => {
            tracing::error!("[AB] {msg}");
            return (status, msg).into_response();
        }
    };

    tracing::info!("[AB] Hellō ID Token delivered by tTS: generating authorization code");

    // ── Generate authorization code with pre-computed JWT ─────────────
    let code_bytes: [u8; 32] = rand::thread_rng().gen();
    let code = hex::encode(code_bytes);

    {
        let mut codes = state.pending_codes.write().await;
        codes.retain(|_, v| v.created_at.elapsed().as_secs() < CODE_TTL_SECS);

        codes.insert(
            code.clone(),
            PendingCode {
                client_id: session.client_id.clone(),
                redirect_uri: session.redirect_uri.clone(),
                code_challenge: session.code_challenge,
                nonce: session.nonce,
                created_at: Instant::now(),
                completed_jwt: Some(token.jwt),
            },
        );
        tracing::info!(
            code_prefix = %&code[..16],
            "[AB] Authorization code stored with pre-computed JWT (pending_codes count={})",
            codes.len()
        );
    }

    // ── Redirect back to the demo app with code + state ──────────────
    let state_param = session
        .state
        .as_deref()
        .map(|s| format!("&state={s}"))
        .unwrap_or_default();

    let redirect_url = format!("{}?code={}{}", session.redirect_uri, code, state_param);

    tracing::info!("[AB] Redirecting browser to demo app with authorization code");
    (StatusCode::FOUND, [("location", redirect_url)]).into_response()
}

/// Minimal URL encoding for error descriptions
fn urlencoding(s: &str) -> String {
    s.replace(' ', "+").replace('&', "%26").replace('=', "%3D")
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    dotenvy::dotenv().ok();
    shared::tls::install_default_provider();
    shared::tls::ensure_certs("certs").expect("Failed to generate TLS certificates");

    let config = AbConfig {
        ib_https_base: env_or_default("IB_HTTPS_BASE", "https://localhost:4002"),
        ib_http_base: env_or_default("IB_HTTP_BASE", "http://localhost:4020"),
        tts_base: env_or_default("TTS_BASE", "https://localhost:5001"),
        issuer: env_or_default("AB_ISSUER", "https://localhost:4001"),
        authorization_endpoint: env_or_default(
            "AB_AUTHORIZATION_ENDPOINT",
            "https://localhost:4001/authorize",
        ),
        token_endpoint: env_or_default("AB_TOKEN_ENDPOINT", "https://localhost:4001/token"),
        jwks_uri: env_or_default("AB_JWKS_URI", "https://localhost:4001/jwks.json"),
        demo_app_redirect_uri: env_or_default(
            "DEMO_APP_REDIRECT_URI",
            "http://localhost:3000/callback",
        ),
        web_ui_redirect_uri: env_or_default(
            "WEB_UI_CALLBACK_URI",
            "http://localhost:8080/callback",
        ),
        web_ui_root_uri: env_or_default("WEB_UI_ROOT_URI", "http://localhost:8080/"),
    };

    // Build HTTPS client that trusts our self-signed CA
    let ca_pem = std::fs::read("certs/ca.pem").expect("read CA cert");
    let ca_cert = reqwest::Certificate::from_pem(&ca_pem).expect("parse CA cert");
    let http_client = reqwest::Client::builder()
        .add_root_certificate(ca_cert)
        .build()
        .expect("build HTTP client");

    let longpoll = matches!(env::var("MOSHI_LONGPOLL").as_deref(), Ok("1") | Ok("true"));
    tracing::info!(
        "[AB] Delivery mode: {}",
        if longpoll {
            "long-poll (AB pulls from tTS /await)"
        } else {
            "push (tTS POSTs to AB /deliver)"
        }
    );

    let state = Arc::new(AppState {
        http_client,
        blinding_key: b"secret_key_osa_prototype_2024".to_vec(),
        authorize_sig_key: shared::crypto::AUTHORIZE_MAC_KEY_DEMO.to_vec(),
        delivery_sig_key: shared::crypto::DELIVERY_MAC_KEY_DEMO.to_vec(),
        config: config.clone(),
        pending_codes: RwLock::new(HashMap::new()),
        pending_sessions: RwLock::new(HashMap::new()),
        deliveries: RwLock::new(HashMap::new()),
        longpoll,
    });

    // Background sweeper: expire abandoned token deliveries out-of-band so the
    // hot path (deliver/take_delivery) stays O(1) and the map drains even when
    // a flood of abandoned flows arrives under overload.
    {
        let gc_state = state.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(10));
            loop {
                tick.tick().await;
                gc_state
                    .deliveries
                    .write()
                    .await
                    .retain(|_, (_, at)| at.elapsed().as_secs() < DELIVERY_TTL_SECS);
            }
        });
    }

    // ── HTTPS :4001 (inter-service + sync OIDC) ─────────────────────
    let https_app = Router::new()
        .route("/login", post(login))
        .route("/authorize", get(authorize))
        .route("/token", post(token_exchange).put(token_exchange_json))
        .route("/deliver", post(deliver))
        .route(
            "/.well-known/openid-configuration",
            get(openid_configuration),
        )
        .route("/jwks.json", get(jwks))
        .route("/client_blind_id", get(client_blind_id))
        .with_state(state.clone());

    let https_bind = env_or_default("AB_HTTPS_BIND", "127.0.0.1:4001");
    let https_addr: SocketAddr = https_bind.parse().expect("Invalid AB_HTTPS_BIND");

    let tls_config =
        axum_server::tls_rustls::RustlsConfig::from_pem_file("certs/ab.pem", "certs/ab.key")
            .await
            .expect("Failed to load AB TLS config");

    // ── HTTP :4010 (browser-facing: Google redirect flow) ───────────
    let http_app = Router::new()
        .route("/authorize", get(browser_authorize))
        .route("/callback", get(browser_callback))
        .with_state(state.clone());

    let http_bind = env_or_default("AB_HTTP_BIND", "127.0.0.1:4010");
    let http_addr: SocketAddr = http_bind.parse().expect("Invalid AB_HTTP_BIND");

    tracing::info!("[AB] App-Facing Broker");
    tracing::info!("[AB]   HTTPS inter-service: https://{}", https_addr);
    tracing::info!("[AB]   HTTP  browser-facing: http://{}", http_addr);
    tracing::info!("[AB] Visibility: KNOWS app_id | does NOT know iss, sub");
    tracing::info!("[AB] OIDC: discovery, jwks, authorize (implicit+code+redirect), token (PKCE)");
    tracing::info!("[AB] IB HTTPS base: {}", config.ib_https_base);
    tracing::info!("[AB] IB HTTP base: {}", config.ib_http_base);
    tracing::info!(
        "[AB] Registered redirect (demo): {}",
        config.demo_app_redirect_uri
    );
    tracing::info!(
        "[AB] Registered redirect (web_ui cb): {}",
        config.web_ui_redirect_uri
    );
    tracing::info!(
        "[AB] Registered redirect (web_ui root): {}",
        config.web_ui_root_uri
    );

    // Launch both servers concurrently
    let https_handle = tokio::spawn(async move {
        axum_server::bind_rustls(https_addr, tls_config)
            .serve(https_app.into_make_service())
            .await
            .expect("AB HTTPS server error");
    });

    let http_handle = tokio::spawn(async move {
        let listener = tokio::net::TcpListener::bind(http_addr)
            .await
            .expect("Failed to bind AB HTTP");
        axum::serve(listener, http_app)
            .await
            .expect("AB HTTP server error");
    });

    tokio::select! {
        r = https_handle => { r.expect("HTTPS task panicked"); }
        r = http_handle => { r.expect("HTTP task panicked"); }
    }
}
