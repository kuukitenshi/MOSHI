//! IB: IdP-Facing Broker (Microsserviço 2)
//!
//! **Two listeners:**
//!   - HTTPS on port 4002: inter-service (AB calls, relay retrieval)
//!   - HTTP  on port 4020: browser-facing (Google OAuth redirects)
//!
//! **Visibility:**
//!   - KNOWS  `iss` and `sub` (from Google's ID Token)
//!   - NEVER SEES `app_id` (only receives `blind_app_id`)
//!
//! Blinds `iss` and `sub` via PRF before forwarding to the tTS.

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::Rng;
use serde::Deserialize;
use shared::types::{BlindedLoginRequest, IdpResponse, TokenRequest};
use std::collections::HashMap;
use std::env;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;

#[derive(Debug, Clone)]
struct IbConfig {
    mock_idp_authorize_url: String,
    tts_issue_url: String,
    ab_callback_base: String,
    ib_public_base: String,
    /// Google OAuth `prompt` value. Empty (default) = OIDC default: Google shows
    /// UI only when needed, so the FIRST login is interactive but a returning
    /// user with a live session + prior consent is re-authed silently (fast),
    /// matching a wallet-style warm session. "consent" forces the consent screen
    /// every login (slow); "none" forbids all UI (breaks the first login).
    google_prompt: String,
}

fn env_or_default(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_string())
}

const PENDING_TTL_SECS: u64 = 300; // 5 min for Google auth

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

struct AppState {
    http_client: reqwest::Client,
    blinding_key: Vec<u8>,
    /// Shared key used to authenticate the IB -> tTS issuance envelope.
    issue_sig_key: Vec<u8>,
    /// Shared key used to VERIFY the AB -> IB authorize envelope (step 5).
    authorize_sig_key: Vec<u8>,
    config: IbConfig,
    google_client_id: String,
    google_client_secret: String,
    github_client_id: String,
    github_client_secret: String,
    discord_client_id: String,
    discord_client_secret: String,
    /// Pending authentications (state -> PendingAuth)
    pending_google: RwLock<HashMap<String, PendingAuth>>,
    pending_github: RwLock<HashMap<String, PendingAuth>>,
    pending_discord: RwLock<HashMap<String, PendingAuth>>,
}

/// Data stored while the user is authenticating with any IdP (Google, GitHub, Discord)
#[derive(Debug, Clone)]
struct PendingAuth {
    session_id: String, // AB's session ID
    blind_app_id: String,
    nonce: Option<String>,
    created_at: Instant,
}

// ---------------------------------------------------------------------------
// Forward the blinded tuple to the tTS
// ---------------------------------------------------------------------------

/// Build the blinded `TokenRequest` and call the tTS. The tTS mints the token
/// and delivers it straight to AB (keyed by `session_id`), so IB never receives
/// the JWT or the DI: and, because the DI is a keyed PRF, cannot compute it.
async fn call_tts(
    state: &AppState,
    blind_app_id: String,
    blind_iss: String,
    blind_sub: String,
    session_id: String,
    nonce: Option<String>,
) -> Result<(), (StatusCode, String)> {
    // Authenticate the envelope at the application layer (independent of TLS):
    // the tTS rejects any /issue whose tag does not match, so a tampered or
    // forged blinded tuple cannot mint a token even if the transport is broken.
    let sig = shared::crypto::mac(
        &state.issue_sig_key,
        &shared::crypto::issue_signing_bytes(
            &blind_app_id,
            &blind_iss,
            &blind_sub,
            &session_id,
            nonce.as_deref(),
        ),
    );

    // Profile claims (name/email/picture) are intentionally NOT forwarded: the
    // IB sees them (origin side) but the tTS/AB must stay blind to real identity.
    let token_req = TokenRequest {
        blind_app_id,
        blind_iss,
        blind_sub,
        session_id,
        nonce,
        sig,
    };

    tracing::info!("[IB] Forwarding blinded tuple to tTS, signed envelope (token delivered straight to AB)");

    let resp = state
        .http_client
        .post(&state.config.tts_issue_url)
        .json(&token_req)
        .send()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("tTS unreachable: {e}")))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err((
            StatusCode::BAD_GATEWAY,
            format!("tTS error {status}: {body}"),
        ));
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// HTTPS :4002: POST /process (direct flow, backwards compat with mock_idp)
// ---------------------------------------------------------------------------

async fn process(
    State(state): State<Arc<AppState>>,
    Json(req): Json<BlindedLoginRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    tracing::info!("──────────────────────────────────────────────────");
    tracing::info!(
        blind_app_id = %req.blind_app_id,
        "[IB] Received blinded request from AB (IB CANNOT see original app_id)"
    );

    if !authorize_sig_ok(&state, &req.blind_app_id, &req.session_id, req.nonce.as_deref(), &req.sig) {
        tracing::warn!("[IB] Rejected /process: invalid AB signature");
        return Err((StatusCode::UNAUTHORIZED, "invalid authorize signature".to_string()));
    }

    // ── Step 4: Contact Mock IdP ─────────────────────────────────────
    tracing::info!(
        "[IB] Step 4: Contacting Mock IdP via HTTPS ({})",
        state.config.mock_idp_authorize_url
    );

    let idp_resp: IdpResponse = state
        .http_client
        .get(&state.config.mock_idp_authorize_url)
        .send()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("IdP unreachable: {e}")))?
        .json()
        .await
        .map_err(|e| {
            (
                StatusCode::BAD_GATEWAY,
                format!("Invalid IdP response: {e}"),
            )
        })?;

    tracing::info!(
        iss = %idp_resp.iss,
        sub = %idp_resp.sub,
        "[IB] Step 4: Received ID Token from IdP"
    );

    if let Some(ref name) = idp_resp.name {
        tracing::info!(name = %name, "[IB] Step 4: Profile claim: name");
    }
    if let Some(ref email) = idp_resp.email {
        tracing::info!(email = %email, "[IB] Step 4: Profile claim: email");
    }

    // ── Step 5: Blind iss and sub ────────────────────────────────────
    let blind_iss = shared::crypto::blind(&state.blinding_key, idp_resp.iss.as_bytes());
    let blind_sub = shared::crypto::blind(&state.blinding_key, idp_resp.sub.as_bytes());

    tracing::info!(
        blind_iss = %blind_iss,
        blind_sub = %blind_sub,
        "[IB] Step 5: iss and sub blinded (originals will NOT be forwarded to tTS)"
    );

    // ── Step 6: Forward to tTS (tTS delivers the token straight to AB) ─
    call_tts(
        &state,
        req.blind_app_id,
        blind_iss,
        blind_sub,
        req.session_id,
        req.nonce,
    )
    .await?;

    tracing::info!("[IB] Step 6: tTS minted and delivered token to AB (IB never saw DI/token)");
    Ok(StatusCode::OK)
}

// ---------------------------------------------------------------------------
// HTTP :4020: GET /authorize (browser redirect → Google)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct IbAuthorizeParams {
    session_id: String,
    blind_app_id: String,
    #[serde(default)]
    nonce: Option<String>,
    /// AB's authentication tag over (blind_app_id, session_id, nonce).
    sig: String,
}

/// Verify AB's tag on a redirect/forward carrying the blinded app id. Rejecting
/// here means the IB never contacts an IdP for a tampered or unauthenticated
/// request, regardless of the TLS state on the AB->IB hop.
fn authorize_sig_ok(
    state: &AppState,
    blind_app_id: &str,
    session_id: &str,
    nonce: Option<&str>,
    sig: &str,
) -> bool {
    shared::crypto::verify_mac(
        &state.authorize_sig_key,
        &shared::crypto::authorize_signing_bytes(blind_app_id, session_id, nonce),
        sig,
    )
}

async fn browser_authorize(
    State(state): State<Arc<AppState>>,
    Query(params): Query<IbAuthorizeParams>,
) -> impl IntoResponse {
    tracing::info!("══════════════════════════════════════════════════");
    tracing::info!(
        session_id = %params.session_id,
        "[IB] Browser /authorize: received from AB (IB CANNOT see original app_id)"
    );
    tracing::info!(
        blind_app_id = %params.blind_app_id,
        "[IB] Only blinded app_id received"
    );

    if !authorize_sig_ok(&state, &params.blind_app_id, &params.session_id, params.nonce.as_deref(), &params.sig) {
        tracing::warn!("[IB] Rejected /authorize: invalid AB signature");
        return (StatusCode::BAD_REQUEST, "invalid authorize signature").into_response();
    }

    // Generate a random state for the Google request
    let google_state = hex::encode(rand::thread_rng().gen::<[u8; 16]>());

    // Store pending auth
    {
        let mut pending = state.pending_google.write().await;
        // GC expired
        pending.retain(|_, v| v.created_at.elapsed().as_secs() < PENDING_TTL_SECS);

        pending.insert(
            google_state.clone(),
            PendingAuth {
                session_id: params.session_id,
                blind_app_id: params.blind_app_id,
                nonce: params.nonce.clone(),
                created_at: Instant::now(),
            },
        );
    }

    // Build Google OIDC authorize URL
    let nonce_param = params
        .nonce
        .as_deref()
        .map(|n| format!("&nonce={n}"))
        .unwrap_or_default();

    // Omit &prompt entirely when unset → OIDC default (silent for returning
    // users, interactive only when Google needs it).
    let prompt_param = if state.config.google_prompt.is_empty() {
        String::new()
    } else {
        format!("&prompt={}", state.config.google_prompt)
    };

    let google_url = format!(
        "https://accounts.google.com/o/oauth2/v2/auth?\
         client_id={}\
         &redirect_uri={}/callback/google\
         &response_type=code\
         &scope=openid%20email%20profile\
         &state={}\
         &access_type=offline\
         {}\
         {}",
        state.google_client_id,
        state.config.ib_public_base,
        google_state,
        prompt_param,
        nonce_param
    );

    tracing::info!("[IB] Redirecting browser to Google for authentication");
    (StatusCode::FOUND, [("location", google_url)]).into_response()
}

// ---------------------------------------------------------------------------
// HTTP :4020: GET /callback/google (Google redirects back here)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct GoogleCallbackParams {
    code: String,
    state: String,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct GoogleTokenResponse {
    id_token: Option<String>,
    access_token: Option<String>,
}

async fn google_callback(
    State(state): State<Arc<AppState>>,
    Query(params): Query<GoogleCallbackParams>,
) -> impl IntoResponse {
    tracing::info!("══════════════════════════════════════════════════");
    tracing::info!("[IB] Google callback received");

    // ── Look up pending auth by google_state ─────────────────────────
    let pending = {
        let mut map = state.pending_google.write().await;
        map.retain(|_, v| v.created_at.elapsed().as_secs() < PENDING_TTL_SECS);
        map.remove(&params.state)
    };

    let pending = match pending {
        Some(p) => p,
        None => {
            tracing::error!("[IB] Invalid or expired google state");
            return (
                StatusCode::BAD_REQUEST,
                "Invalid or expired state parameter",
            )
                .into_response();
        }
    };

    // ── Exchange Google auth code for tokens (server-to-server) ──────
    tracing::info!("[IB] Exchanging Google authorization code for tokens");

    let token_resp = state
        .http_client
        .post("https://oauth2.googleapis.com/token")
        .form(&[
            ("code", params.code.as_str()),
            ("client_id", state.google_client_id.as_str()),
            ("client_secret", state.google_client_secret.as_str()),
            (
                "redirect_uri",
                &format!("{}/callback/google", state.config.ib_public_base),
            ),
            ("grant_type", "authorization_code"),
        ])
        .send()
        .await;

    let token_resp = match token_resp {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("[IB] Google token exchange failed: {e}");
            return (
                StatusCode::BAD_GATEWAY,
                format!("Google token exchange failed: {e}"),
            )
                .into_response();
        }
    };

    if !token_resp.status().is_success() {
        let status = token_resp.status();
        let body = token_resp.text().await.unwrap_or_default();
        tracing::error!("[IB] Google token endpoint error {status}: {body}");
        return (
            StatusCode::BAD_GATEWAY,
            format!("Google token error {status}: {body}"),
        )
            .into_response();
    }

    let google_tokens: GoogleTokenResponse = match token_resp.json().await {
        Ok(t) => t,
        Err(e) => {
            tracing::error!("[IB] Invalid Google token response: {e}");
            return (
                StatusCode::BAD_GATEWAY,
                format!("Invalid Google token response: {e}"),
            )
                .into_response();
        }
    };

    let id_token = match &google_tokens.id_token {
        Some(t) => t.clone(),
        None => {
            tracing::error!("[IB] No id_token in Google response");
            return (
                StatusCode::BAD_GATEWAY,
                "No id_token from Google".to_string(),
            )
                .into_response();
        }
    };

    // ── Decode Google's id_token to extract claims ───────────────────
    let claims = match decode_google_id_token(&id_token) {
        Some(c) => c,
        None => {
            tracing::error!("[IB] Failed to decode Google id_token");
            return (
                StatusCode::BAD_GATEWAY,
                "Failed to decode Google id_token".to_string(),
            )
                .into_response();
        }
    };

    let iss = claims["iss"].as_str().unwrap_or("unknown").to_string();
    let sub = claims["sub"].as_str().unwrap_or("unknown").to_string();
    let name = claims["name"].as_str().map(String::from);
    let email = claims["email"].as_str().map(String::from);
    // Profile claims are no longer forwarded to the tTS (see call_tts); kept
    // here only for the IB's own logging. Prefixed `_` to mark unused downstream.
    let _email_verified = claims["email_verified"].as_bool();
    let _picture = claims["picture"].as_str().map(String::from);

    tracing::info!(
        iss = %iss,
        sub = %sub,
        "[IB] Extracted identity from Google ID Token"
    );
    if let Some(ref n) = name {
        tracing::info!(name = %n, "[IB] Profile claim: name");
    }
    if let Some(ref e) = email {
        tracing::info!(email = %e, "[IB] Profile claim: email");
    }

    // ── Blind iss and sub (IB NEVER forwards plaintext to tTS) ─────
    let blind_iss = shared::crypto::blind(&state.blinding_key, iss.as_bytes());
    let blind_sub = shared::crypto::blind(&state.blinding_key, sub.as_bytes());

    tracing::info!(
        blind_iss = %blind_iss,
        blind_sub = %blind_sub,
        "[IB] iss and sub blinded (originals will NOT be forwarded to tTS)"
    );

    // ── Forward to tTS, which delivers the token straight to AB ───────
    if let Err((status, msg)) = call_tts(
        &state,
        pending.blind_app_id,
        blind_iss,
        blind_sub,
        pending.session_id.clone(),
        pending.nonce,
    )
    .await
    {
        tracing::error!("[IB] tTS call failed: {msg}");
        return (status, msg).into_response();
    }

    // ── Redirect back to AB /callback (HTTP) ──────────────────────
    let redirect_url = format!(
        "{}/callback?session_id={}",
        state.config.ab_callback_base, pending.session_id
    );

    tracing::info!("[IB] Redirecting browser back to AB (IB never saw DI/token)");
    (StatusCode::FOUND, [("location", redirect_url)]).into_response()
}

// ---------------------------------------------------------------------------
// GitHub OAuth Flow
// ---------------------------------------------------------------------------

async fn browser_authorize_github(
    State(state): State<Arc<AppState>>,
    Query(params): Query<IbAuthorizeParams>,
) -> impl IntoResponse {
    tracing::info!("══════════════════════════════════════════════════");
    tracing::info!(
        session_id = %params.session_id,
        "[IB] Browser /authorize/github: received from AB"
    );

    if !authorize_sig_ok(&state, &params.blind_app_id, &params.session_id, params.nonce.as_deref(), &params.sig) {
        tracing::warn!("[IB] Rejected /authorize/github: invalid AB signature");
        return (StatusCode::BAD_REQUEST, "invalid authorize signature").into_response();
    }

    // Generate a random state for the GitHub request
    let github_state = hex::encode(rand::thread_rng().gen::<[u8; 16]>());

    // Store pending auth
    {
        let mut pending = state.pending_github.write().await;
        pending.retain(|_, v| v.created_at.elapsed().as_secs() < PENDING_TTL_SECS);

        pending.insert(
            github_state.clone(),
            PendingAuth {
                session_id: params.session_id,
                blind_app_id: params.blind_app_id,
                nonce: params.nonce.clone(),
                created_at: Instant::now(),
            },
        );
    }

    // Build GitHub OAuth authorize URL
    let github_url = format!(
        "https://github.com/login/oauth/authorize?\
         client_id={}\
         &redirect_uri={}/callback/github\
         &scope=read:user%20user:email\
         &state={}\
         &allow_signup=true",
        state.github_client_id, state.config.ib_public_base, github_state
    );

    tracing::info!("[IB] Redirecting browser to GitHub for authentication");
    (StatusCode::FOUND, [("location", github_url)]).into_response()
}

#[derive(Debug, Deserialize)]
struct GitHubCallbackParams {
    code: String,
    state: String,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct GitHubTokenResponse {
    access_token: String,
    token_type: String,
    scope: String,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct GitHubUserResponse {
    id: u64,
    login: String,
    name: Option<String>,
    email: Option<String>,
    avatar_url: Option<String>,
}

async fn github_callback(
    State(state): State<Arc<AppState>>,
    Query(params): Query<GitHubCallbackParams>,
) -> impl IntoResponse {
    tracing::info!("══════════════════════════════════════════════════");
    tracing::info!("[IB] GitHub callback received");

    // Look up pending auth by github_state
    let pending = {
        let mut map = state.pending_github.write().await;
        map.retain(|_, v| v.created_at.elapsed().as_secs() < PENDING_TTL_SECS);
        map.remove(&params.state)
    };

    let pending = match pending {
        Some(p) => p,
        None => {
            tracing::error!("[IB] Invalid or expired github state");
            return (
                StatusCode::BAD_REQUEST,
                "Invalid or expired state parameter",
            )
                .into_response();
        }
    };

    // Exchange GitHub auth code for tokens (server-to-server)
    tracing::info!("[IB] Exchanging GitHub authorization code for tokens");

    let token_resp = state
        .http_client
        .post("https://github.com/login/oauth/access_token")
        .header("Accept", "application/json")
        .form(&[
            ("code", params.code.as_str()),
            ("client_id", state.github_client_id.as_str()),
            ("client_secret", state.github_client_secret.as_str()),
            (
                "redirect_uri",
                &format!("{}/callback/github", state.config.ib_public_base),
            ),
        ])
        .send()
        .await;

    let token_resp = match token_resp {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("[IB] GitHub token exchange failed: {e}");
            return (
                StatusCode::BAD_GATEWAY,
                format!("GitHub token exchange failed: {e}"),
            )
                .into_response();
        }
    };

    if !token_resp.status().is_success() {
        let status = token_resp.status();
        let body = token_resp.text().await.unwrap_or_default();
        tracing::error!("[IB] GitHub token endpoint error {status}: {body}");
        return (
            StatusCode::BAD_GATEWAY,
            format!("GitHub token error {status}: {body}"),
        )
            .into_response();
    }

    let github_tokens: GitHubTokenResponse = match token_resp.json().await {
        Ok(t) => t,
        Err(e) => {
            tracing::error!("[IB] Invalid GitHub token response: {e}");
            return (
                StatusCode::BAD_GATEWAY,
                format!("Invalid GitHub token response: {e}"),
            )
                .into_response();
        }
    };

    // Fetch user profile from GitHub
    tracing::info!("[IB] Fetching GitHub user profile");

    let user_resp = state
        .http_client
        .get("https://api.github.com/user")
        .header(
            "Authorization",
            format!("Bearer {}", github_tokens.access_token),
        )
        .header("User-Agent", "partitioned-idp")
        .send()
        .await;

    let user_resp = match user_resp {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("[IB] GitHub user fetch failed: {e}");
            return (
                StatusCode::BAD_GATEWAY,
                format!("GitHub user fetch failed: {e}"),
            )
                .into_response();
        }
    };

    if !user_resp.status().is_success() {
        let status = user_resp.status();
        let body = user_resp.text().await.unwrap_or_default();
        tracing::error!("[IB] GitHub user endpoint error {status}: {body}");
        return (
            StatusCode::BAD_GATEWAY,
            format!("GitHub user error {status}: {body}"),
        )
            .into_response();
    }

    let github_user: GitHubUserResponse = match user_resp.json().await {
        Ok(u) => u,
        Err(e) => {
            tracing::error!("[IB] Invalid GitHub user response: {e}");
            return (
                StatusCode::BAD_GATEWAY,
                format!("Invalid GitHub user response: {e}"),
            )
                .into_response();
        }
    };

    // Extract claims from GitHub profile
    let iss = "https://github.com".to_string();
    let sub = github_user.id.to_string();
    let name = github_user.name;
    let email = github_user.email;
    let _picture = github_user.avatar_url;

    tracing::info!(
        iss = %iss,
        sub = %sub,
        "[IB] Extracted identity from GitHub profile"
    );
    if let Some(ref n) = name {
        tracing::info!(name = %n, "[IB] Profile claim: name");
    }
    if let Some(ref e) = email {
        tracing::info!(email = %e, "[IB] Profile claim: email");
    }

    // Blind iss and sub
    let blind_iss = shared::crypto::blind(&state.blinding_key, iss.as_bytes());
    let blind_sub = shared::crypto::blind(&state.blinding_key, sub.as_bytes());

    tracing::info!(
        blind_iss = %blind_iss,
        blind_sub = %blind_sub,
        "[IB] iss and sub blinded"
    );

    // Forward to tTS, which delivers the token straight to AB.
    if let Err((status, msg)) = call_tts(
        &state,
        pending.blind_app_id,
        blind_iss,
        blind_sub,
        pending.session_id.clone(),
        pending.nonce,
    )
    .await
    {
        tracing::error!("[IB] tTS call failed: {msg}");
        return (status, msg).into_response();
    }

    // Redirect back to AB /callback
    let redirect_url = format!(
        "{}/callback?session_id={}",
        state.config.ab_callback_base, pending.session_id
    );

    tracing::info!("[IB] Redirecting browser back to AB (GitHub flow complete)");
    (StatusCode::FOUND, [("location", redirect_url)]).into_response()
}

// ---------------------------------------------------------------------------
// Discord OAuth Flow
// ---------------------------------------------------------------------------

async fn browser_authorize_discord(
    State(state): State<Arc<AppState>>,
    Query(params): Query<IbAuthorizeParams>,
) -> impl IntoResponse {
    tracing::info!("══════════════════════════════════════════════════");
    tracing::info!(
        session_id = %params.session_id,
        "[IB] Browser /authorize/discord: received from AB"
    );

    if !authorize_sig_ok(&state, &params.blind_app_id, &params.session_id, params.nonce.as_deref(), &params.sig) {
        tracing::warn!("[IB] Rejected /authorize/discord: invalid AB signature");
        return (StatusCode::BAD_REQUEST, "invalid authorize signature").into_response();
    }

    // Generate a random state for the Discord request
    let discord_state = hex::encode(rand::thread_rng().gen::<[u8; 16]>());

    // Store pending auth
    {
        let mut pending = state.pending_discord.write().await;
        pending.retain(|_, v| v.created_at.elapsed().as_secs() < PENDING_TTL_SECS);

        pending.insert(
            discord_state.clone(),
            PendingAuth {
                session_id: params.session_id,
                blind_app_id: params.blind_app_id,
                nonce: params.nonce.clone(),
                created_at: Instant::now(),
            },
        );
    }

    // Build Discord OAuth authorize URL
    let discord_url = format!(
        "https://discord.com/api/oauth2/authorize?\
         client_id={}\
         &redirect_uri={}/callback/discord\
         &response_type=code\
         &scope=identify%20email\
         &state={}",
        state.discord_client_id, state.config.ib_public_base, discord_state
    );

    tracing::info!("[IB] Redirecting browser to Discord for authentication");
    (StatusCode::FOUND, [("location", discord_url)]).into_response()
}

#[derive(Debug, Deserialize)]
struct DiscordCallbackParams {
    code: String,
    state: String,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct DiscordTokenResponse {
    access_token: String,
    token_type: String,
    expires_in: u64,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct DiscordUserResponse {
    id: String,
    username: String,
    discriminator: String,
    email: Option<String>,
    avatar: Option<String>,
}

async fn discord_callback(
    State(state): State<Arc<AppState>>,
    Query(params): Query<DiscordCallbackParams>,
) -> impl IntoResponse {
    tracing::info!("══════════════════════════════════════════════════");
    tracing::info!("[IB] Discord callback received");

    // Look up pending auth by discord_state
    let pending = {
        let mut map = state.pending_discord.write().await;
        map.retain(|_, v| v.created_at.elapsed().as_secs() < PENDING_TTL_SECS);
        map.remove(&params.state)
    };

    let pending = match pending {
        Some(p) => p,
        None => {
            tracing::error!("[IB] Invalid or expired discord state");
            return (
                StatusCode::BAD_REQUEST,
                "Invalid or expired state parameter",
            )
                .into_response();
        }
    };

    // Exchange Discord auth code for tokens (server-to-server)
    tracing::info!("[IB] Exchanging Discord authorization code for tokens");

    let token_resp = state
        .http_client
        .post("https://discord.com/api/oauth2/token")
        .form(&[
            ("code", params.code.as_str()),
            ("client_id", state.discord_client_id.as_str()),
            ("client_secret", state.discord_client_secret.as_str()),
            (
                "redirect_uri",
                &format!("{}/callback/discord", state.config.ib_public_base),
            ),
            ("grant_type", "authorization_code"),
        ])
        .send()
        .await;

    let token_resp = match token_resp {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("[IB] Discord token exchange failed: {e}");
            return (
                StatusCode::BAD_GATEWAY,
                format!("Discord token exchange failed: {e}"),
            )
                .into_response();
        }
    };

    if !token_resp.status().is_success() {
        let status = token_resp.status();
        let body = token_resp.text().await.unwrap_or_default();
        tracing::error!("[IB] Discord token endpoint error {status}: {body}");
        return (
            StatusCode::BAD_GATEWAY,
            format!("Discord token error {status}: {body}"),
        )
            .into_response();
    }

    let discord_tokens: DiscordTokenResponse = match token_resp.json().await {
        Ok(t) => t,
        Err(e) => {
            tracing::error!("[IB] Invalid Discord token response: {e}");
            return (
                StatusCode::BAD_GATEWAY,
                format!("Invalid Discord token response: {e}"),
            )
                .into_response();
        }
    };

    // Fetch user profile from Discord
    tracing::info!("[IB] Fetching Discord user profile");

    let user_resp = state
        .http_client
        .get("https://discord.com/api/users/@me")
        .header(
            "Authorization",
            format!("Bearer {}", discord_tokens.access_token),
        )
        .send()
        .await;

    let user_resp = match user_resp {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("[IB] Discord user fetch failed: {e}");
            return (
                StatusCode::BAD_GATEWAY,
                format!("Discord user fetch failed: {e}"),
            )
                .into_response();
        }
    };

    if !user_resp.status().is_success() {
        let status = user_resp.status();
        let body = user_resp.text().await.unwrap_or_default();
        tracing::error!("[IB] Discord user endpoint error {status}: {body}");
        return (
            StatusCode::BAD_GATEWAY,
            format!("Discord user error {status}: {body}"),
        )
            .into_response();
    }

    let discord_user: DiscordUserResponse = match user_resp.json().await {
        Ok(u) => u,
        Err(e) => {
            tracing::error!("[IB] Invalid Discord user response: {e}");
            return (
                StatusCode::BAD_GATEWAY,
                format!("Invalid Discord user response: {e}"),
            )
                .into_response();
        }
    };

    // Extract claims from Discord profile
    let iss = "https://discord.com".to_string();
    let sub = discord_user.id;
    let name = Some(format!(
        "{}#{}",
        discord_user.username, discord_user.discriminator
    ));
    let email = discord_user.email;
    let _picture = discord_user
        .avatar
        .map(|avatar| format!("https://cdn.discordapp.com/avatars/{}/{}.png", sub, avatar));

    tracing::info!(
        iss = %iss,
        sub = %sub,
        "[IB] Extracted identity from Discord profile"
    );
    if let Some(ref n) = name {
        tracing::info!(name = %n, "[IB] Profile claim: name");
    }
    if let Some(ref e) = email {
        tracing::info!(email = %e, "[IB] Profile claim: email");
    }

    // Blind iss and sub
    let blind_iss = shared::crypto::blind(&state.blinding_key, iss.as_bytes());
    let blind_sub = shared::crypto::blind(&state.blinding_key, sub.as_bytes());

    tracing::info!(
        blind_iss = %blind_iss,
        blind_sub = %blind_sub,
        "[IB] iss and sub blinded"
    );

    // Forward to tTS, which delivers the token straight to AB.
    if let Err((status, msg)) = call_tts(
        &state,
        pending.blind_app_id,
        blind_iss,
        blind_sub,
        pending.session_id.clone(),
        pending.nonce,
    )
    .await
    {
        tracing::error!("[IB] tTS call failed: {msg}");
        return (status, msg).into_response();
    }

    // Redirect back to AB /callback
    let redirect_url = format!(
        "{}/callback?session_id={}",
        state.config.ab_callback_base, pending.session_id
    );

    tracing::info!("[IB] Redirecting browser back to AB (Discord flow complete)");
    (StatusCode::FOUND, [("location", redirect_url)]).into_response()
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Decode a Google ID Token (JWT) payload without signature verification.
/// The token was received directly from Google's token endpoint over HTTPS,
/// so it is already trusted.
fn decode_google_id_token(jwt: &str) -> Option<serde_json::Value> {
    let parts: Vec<&str> = jwt.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let bytes = URL_SAFE_NO_PAD.decode(parts[1]).ok()?;
    let text = String::from_utf8(bytes).ok()?;
    serde_json::from_str(&text).ok()
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    shared::tls::install_default_provider();
    shared::tls::ensure_certs("certs").expect("Failed to generate TLS certificates");

    // Load .env
    dotenvy::dotenv().ok();

    let config = IbConfig {
        mock_idp_authorize_url: env_or_default(
            "MOCK_IDP_AUTHORIZE_URL",
            "https://localhost:3001/authorize",
        ),
        tts_issue_url: env_or_default("TTS_ISSUE_URL", "https://localhost:5001/issue"),
        ab_callback_base: env_or_default("AB_HTTP_BASE", "http://localhost:4010"),
        ib_public_base: env_or_default("IB_HTTP_BASE", "http://localhost:4020"),
        google_prompt: env_or_default("IB_GOOGLE_PROMPT", ""),
    };
    let google_client_id =
        std::env::var("GOOGLE_CLIENT_ID").expect("GOOGLE_CLIENT_ID must be set in .env");
    let google_client_secret =
        std::env::var("GOOGLE_CLIENT_SECRET").expect("GOOGLE_CLIENT_SECRET must be set in .env");
    let github_client_id =
        std::env::var("GITHUB_CLIENT_ID").expect("GITHUB_CLIENT_ID must be set in .env");
    let github_client_secret =
        std::env::var("GITHUB_CLIENT_SECRET").expect("GITHUB_CLIENT_SECRET must be set in .env");
    let discord_client_id =
        std::env::var("DISCORD_CLIENT_ID").expect("DISCORD_CLIENT_ID must be set in .env");
    let discord_client_secret =
        std::env::var("DISCORD_CLIENT_SECRET").expect("DISCORD_CLIENT_SECRET must be set in .env");

    tracing::info!(
        "[IB] Google Client ID loaded: {}...",
        &google_client_id[..20.min(google_client_id.len())]
    );
    tracing::info!(
        "[IB] GitHub Client ID loaded: {}...",
        &github_client_id[..20.min(github_client_id.len())]
    );
    tracing::info!(
        "[IB] Discord Client ID loaded: {}...",
        &discord_client_id[..20.min(discord_client_id.len())]
    );

    // Build HTTPS client that trusts our self-signed CA + system roots (for Google)
    let ca_pem = std::fs::read("certs/ca.pem").expect("read CA cert");
    let ca_cert = reqwest::Certificate::from_pem(&ca_pem).expect("parse CA cert");
    let http_client = reqwest::Client::builder()
        .add_root_certificate(ca_cert)
        .build()
        .expect("build HTTP client");

    let state = Arc::new(AppState {
        http_client,
        blinding_key: b"secret_key_osb_prototype_2024".to_vec(),
        issue_sig_key: shared::crypto::ISSUE_MAC_KEY_DEMO.to_vec(),
        authorize_sig_key: shared::crypto::AUTHORIZE_MAC_KEY_DEMO.to_vec(),
        config: config.clone(),
        google_client_id,
        google_client_secret,
        github_client_id,
        github_client_secret,
        discord_client_id,
        discord_client_secret,
        pending_google: RwLock::new(HashMap::new()),
        pending_github: RwLock::new(HashMap::new()),
        pending_discord: RwLock::new(HashMap::new()),
    });

    // ── HTTPS :4002 (inter-service) ──────────────────────────────────
    let https_app = Router::new()
        .route("/process", post(process))
        .with_state(state.clone());

    let https_bind = env_or_default("IB_HTTPS_BIND", "127.0.0.1:4002");
    let https_addr: SocketAddr = https_bind.parse().expect("Invalid IB_HTTPS_BIND");

    let tls_config =
        axum_server::tls_rustls::RustlsConfig::from_pem_file("certs/ib.pem", "certs/ib.key")
            .await
            .expect("Failed to load IB TLS config");

    // ── HTTP :4020 (browser-facing) ──────────────────────────────────
    let http_app = Router::new()
        .route("/authorize", get(browser_authorize))
        .route("/authorize/github", get(browser_authorize_github))
        .route("/authorize/discord", get(browser_authorize_discord))
        .route("/callback/google", get(google_callback))
        .route("/callback/github", get(github_callback))
        .route("/callback/discord", get(discord_callback))
        .with_state(state.clone());

    let http_bind = env_or_default("IB_HTTP_BIND", "127.0.0.1:4020");
    let http_addr: SocketAddr = http_bind.parse().expect("Invalid IB_HTTP_BIND");

    tracing::info!("[IB] IdP-Facing Broker");
    tracing::info!("[IB]   HTTPS inter-service: https://{}", https_addr);
    tracing::info!("[IB]   HTTP  browser-facing: http://{}", http_addr);
    tracing::info!("[IB] Visibility: KNOWS iss, sub | does NOT know app_id");
    tracing::info!("[IB] Supported IdPs: Google, GitHub, Discord");
    tracing::info!("[IB] Mock IdP URL: {}", config.mock_idp_authorize_url);
    tracing::info!("[IB] tTS URL: {}", config.tts_issue_url);
    tracing::info!("[IB] AB callback base: {}", config.ab_callback_base);
    tracing::info!("[IB] IB public base: {}", config.ib_public_base);

    // Launch both servers concurrently
    let https_handle = tokio::spawn(async move {
        axum_server::bind_rustls(https_addr, tls_config)
            .serve(https_app.into_make_service())
            .await
            .expect("IB HTTPS server error");
    });

    let http_handle = tokio::spawn(async move {
        let listener = tokio::net::TcpListener::bind(http_addr)
            .await
            .expect("Failed to bind IB HTTP");
        axum::serve(listener, http_app)
            .await
            .expect("IB HTTP server error");
    });

    tokio::select! {
        r = https_handle => { r.expect("HTTPS task panicked"); }
        r = http_handle => { r.expect("HTTP task panicked"); }
    }
}
