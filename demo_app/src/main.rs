//! Demo App: Relying Party example with "Sign in with Google" button
//!
//! This demonstrates how any app would integrate with the Partitioned
//! Identity Broker using standard OIDC Authorization Code Flow + PKCE.
//!
//! HTTP server on port 3000 (no TLS: this is the app, not the broker).
//!
//! ## Endpoints
//!
//! - `GET  /`: Landing page with sign-in button
//! - `GET  /auth/start`: Initiates OIDC Code Flow + PKCE via browser redirect (Google)
//! - `GET  /callback`: Receives authorization code, exchanges for JWT, renders result
//! - `POST /api/sign-in`: Legacy: initiates OIDC Code Flow + PKCE (proxied/sync)
//! - `GET  /api/health`: Health check
//!
//! ## How the Google redirect flow works
//!
//! 1. User clicks "Sign in with Google" button → GET /auth/start
//! 2. Backend generates PKCE code_verifier + code_challenge + state + nonce
//! 3. Backend stores session keyed by state, redirects browser to AB HTTP :4010/authorize
//! 4. AB blinds app_id, redirects to IB HTTP :4020/authorize
//! 5. IB redirects to Google for real authentication
//! 6. Google redirects back to IB /callback/google
//! 7. IB exchanges code with Google, extracts claims, blinds iss/sub, calls tTS
//! 8. IB stores JWT in relay, redirects to AB /callback
//! 9. AB fetches JWT from relay, generates auth code, redirects to demo_app /callback
//! 10. Demo app exchanges code for JWT at AB /token, displays result

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::env;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;

// ---------------------------------------------------------------------------
// Embedded HTML
// ---------------------------------------------------------------------------

const INDEX_HTML: &str = include_str!("index.html");
const ERROR_HTML: &str = include_str!("error.html");
const SUCCESS_HTML: &str = include_str!("success.html");

// ---------------------------------------------------------------------------
// PKCE Session storage (for browser redirect flow)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct PkceSession {
    code_verifier: String,
    nonce: String,
    created_at: Instant,
}

const SESSION_TTL_SECS: u64 = 300; // 5 min

// ---------------------------------------------------------------------------
// App State
// ---------------------------------------------------------------------------

struct AppState {
    /// PKCE sessions keyed by state parameter
    pkce_sessions: RwLock<HashMap<String, PkceSession>>,
    ab_https_base: String,
    ab_http_base: String,
    demo_public_base: String,
}

fn env_or_default(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_string())
}

// ---------------------------------------------------------------------------
// Types (for legacy /api/sign-in)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct SignInRequest {
    client_id: String,
}

#[derive(Debug, Serialize)]
struct SignInResponse {
    success: bool,
    authorize_url: Option<String>,
    token_url: Option<String>,
    id_token: Option<String>,
    payload: Option<serde_json::Value>,
    error: Option<String>,
    steps: Vec<FlowStep>,
}

#[derive(Debug, Serialize, Clone)]
struct FlowStep {
    number: u8,
    actor: String,
    action: String,
    detail: String,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn index() -> impl IntoResponse {
    Html(INDEX_HTML)
}

async fn health() -> impl IntoResponse {
    Json(serde_json::json!({ "status": "ok" }))
}

// ---------------------------------------------------------------------------
// GET /auth/start: Initiates Google redirect flow via AB
// ---------------------------------------------------------------------------

// GET /auth/start: Initiates browser redirect flow via AB (with provider support)

#[derive(Debug, Deserialize)]
struct AuthStartParams {
    #[serde(default = "default_provider")]
    provider: String,
}

fn default_provider() -> String {
    "google".to_string()
}

async fn auth_start(
    State(state): State<Arc<AppState>>,
    Query(params): Query<AuthStartParams>,
) -> impl IntoResponse {
    let client_id = "demo_app_hello";
    let redirect_uri = format!("{}/callback", state.demo_public_base);

    // Generate PKCE parameters
    let code_verifier = generate_code_verifier();
    let code_challenge = compute_code_challenge(&code_verifier);

    // Generate nonce and state
    let nonce = generate_random_string(32);
    let state_val = generate_random_string(16);

    tracing::info!(
        state = %state_val,
        provider = %params.provider,
        "[Demo App] Starting {} redirect flow: PKCE + state generated",
        params.provider
    );

    // Store session
    {
        let mut sessions = state.pkce_sessions.write().await;
        // GC expired sessions
        sessions.retain(|_, v| v.created_at.elapsed().as_secs() < SESSION_TTL_SECS);

        sessions.insert(
            state_val.clone(),
            PkceSession {
                code_verifier,
                nonce: nonce.clone(),
                created_at: Instant::now(),
            },
        );
    }

    // Build redirect URL to AB HTTP :4010/authorize with provider param
    let authorize_url = format!(
        "{}/authorize?\
         response_type=code\
         &client_id={client_id}\
         &redirect_uri={redirect_uri}\
         &scope=openid\
         &nonce={nonce}\
         &state={state_val}\
         &code_challenge={code_challenge}\
         &code_challenge_method=S256\
         &provider={provider}",
        state.ab_http_base,
        provider = params.provider
    );

    tracing::info!(
        "[Demo App] Redirecting browser to AB (HTTP :4010) for {}",
        params.provider
    );
    (StatusCode::FOUND, [("location", authorize_url)])
}

// ---------------------------------------------------------------------------
// GET /callback: Receives authorization code, exchanges for JWT
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct CallbackParams {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

async fn callback(
    State(state): State<Arc<AppState>>,
    Query(params): Query<CallbackParams>,
) -> impl IntoResponse {
    tracing::info!("[Demo App] Callback received");

    // Check for errors
    if let Some(err) = &params.error {
        let desc = params
            .error_description
            .as_deref()
            .unwrap_or("unknown error");
        tracing::error!("[Demo App] Error from broker: {err}: {desc}");
        return Html(render_error_page(&format!("{err}: {desc}"))).into_response();
    }

    let code = match &params.code {
        Some(c) => c.clone(),
        None => {
            return Html(render_error_page("No authorization code received")).into_response();
        }
    };

    let state_val = match &params.state {
        Some(s) => s.clone(),
        None => {
            return Html(render_error_page("No state parameter received")).into_response();
        }
    };

    // Look up PKCE session
    let session = {
        let mut sessions = state.pkce_sessions.write().await;
        sessions.retain(|_, v| v.created_at.elapsed().as_secs() < SESSION_TTL_SECS);
        sessions.remove(&state_val)
    };

    let session = match session {
        Some(s) => s,
        None => {
            return Html(render_error_page(
                "Invalid or expired state parameter (CSRF protection)",
            ))
            .into_response();
        }
    };

    tracing::info!(
        code_prefix = %&code[..code.len().min(16)],
        "[Demo App] State verified, exchanging code for JWT"
    );

    // ── Exchange code for JWT at AB HTTPS :4001/token ──────────────
    // We need to trust our self-signed CA for this request
    let ca_pem = match std::fs::read("certs/ca.pem") {
        Ok(p) => p,
        Err(e) => {
            return Html(render_error_page(&format!("Cannot read CA cert: {e}"))).into_response();
        }
    };
    let ca_cert = match reqwest::Certificate::from_pem(&ca_pem) {
        Ok(c) => c,
        Err(e) => {
            return Html(render_error_page(&format!("Invalid CA cert: {e}"))).into_response();
        }
    };

    let http_client = match reqwest::Client::builder()
        .add_root_certificate(ca_cert)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return Html(render_error_page(&format!("HTTP client error: {e}"))).into_response();
        }
    };

    let client_id = "demo_app_hello";
    let redirect_uri = format!("{}/callback", state.demo_public_base);

    let token_resp = http_client
        .post(format!("{}/token", state.ab_https_base))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(format!(
            "grant_type=authorization_code&code={}&redirect_uri={}&code_verifier={}&client_id={}",
            code, redirect_uri, session.code_verifier, client_id
        ))
        .send()
        .await;

    let token_resp = match token_resp {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("[Demo App] Token exchange request failed (TLS/connection?): {e}");
            return Html(render_error_page(&format!("Token exchange failed: {e}"))).into_response();
        }
    };

    if !token_resp.status().is_success() {
        let status = token_resp.status();
        let body = token_resp.text().await.unwrap_or_default();
        tracing::error!("[Demo App] Token endpoint returned {status}: {body}");
        return Html(render_error_page(&format!(
            "Token endpoint returned {status}: {body}"
        )))
        .into_response();
    }

    let token_data: serde_json::Value = match token_resp.json().await {
        Ok(v) => v,
        Err(e) => {
            return Html(render_error_page(&format!("Invalid token response: {e}")))
                .into_response();
        }
    };

    let id_token = match token_data["id_token"].as_str() {
        Some(t) => t.to_string(),
        None => {
            return Html(render_error_page("No id_token in token response")).into_response();
        }
    };

    tracing::info!("[Demo App] JWT received successfully: rendering result page");

    // Decode JWT payload for display
    let payload = decode_jwt_payload(&id_token);

    // Verify nonce
    let nonce_ok = payload
        .as_ref()
        .and_then(|p| p["nonce"].as_str())
        .map(|n| n == session.nonce)
        .unwrap_or(false);

    // ── Verify aud == our blind_app_id ───────────────────────────────
    // The RP learns its own blind_app_id from AB (registration metadata) and
    // checks the token's aud against it. The aud is the BLINDED app id (the
    // plaintext client_id never reaches the tTS), and the RP never holds k_AB.
    match fetch_blind_app_id(&http_client, &state.ab_https_base, client_id).await {
        Ok(expected_aud) => {
            let token_aud = payload
                .as_ref()
                .and_then(|p| p["aud"].as_str())
                .unwrap_or_default();
            if token_aud != expected_aud {
                tracing::error!(
                    "[Demo App] aud MISMATCH: token aud={token_aud}, expected blind_app_id={expected_aud}"
                );
                return Html(render_error_page(
                    "Audience mismatch: token aud does not match this app's blind_app_id",
                ))
                .into_response();
            }
            tracing::info!("[Demo App] aud check VALID (aud == our blind_app_id)");
        }
        Err(e) => {
            tracing::warn!("[Demo App] Could not fetch blind_app_id for aud check: {e}");
        }
    }

    Html(render_success_page(&id_token, payload.as_ref(), nonce_ok)).into_response()
}

// ---------------------------------------------------------------------------
// POST /api/sign-in: Legacy proxied flow (backwards compatible)
// ---------------------------------------------------------------------------

async fn sign_in(
    Json(req): Json<SignInRequest>,
) -> Result<Json<SignInResponse>, (StatusCode, Json<SignInResponse>)> {
    let mut steps = Vec::new();
    let client_id = req.client_id;
    let redirect_uri = "http://localhost:3000/callback";

    // ── Step 1: Generate PKCE parameters ─────────────────────────────
    let code_verifier = generate_code_verifier();
    let code_challenge = compute_code_challenge(&code_verifier);

    steps.push(FlowStep {
        number: 1,
        actor: "Demo App".into(),
        action: "Generate PKCE parameters".into(),
        detail: format!(
            "code_verifier = {}... (43 chars)\ncode_challenge = {}...",
            &code_verifier[..12],
            &code_challenge[..16]
        ),
    });

    // ── Step 2: Generate nonce and state ──────────────────────────────
    let nonce = generate_random_string(32);
    let state_val = generate_random_string(16);

    steps.push(FlowStep {
        number: 2,
        actor: "Demo App".into(),
        action: "Generate nonce + state".into(),
        detail: format!("nonce = {}...\nstate = {}", &nonce[..12], &state_val),
    });

    // ── Step 3: Build authorize URL ──────────────────────────────────
    let authorize_url = format!(
        "https://localhost:4001/authorize?\
         response_type=code\
         &client_id={client_id}\
         &redirect_uri={redirect_uri}\
         &scope=openid\
         &nonce={nonce}\
         &state={state_val}\
         &code_challenge={code_challenge}\
         &code_challenge_method=S256"
    );

    steps.push(FlowStep {
        number: 3,
        actor: "Browser".into(),
        action: "Redirect to /authorize".into(),
        detail: format!(
            "GET /authorize?response_type=code&client_id={}&scope=openid&code_challenge=...&code_challenge_method=S256",
            client_id
        ),
    });

    // ── Step 4: Call /authorize (proxy: in production this is browser redirect)
    let ca_pem = std::fs::read("certs/ca.pem")
        .map_err(|e| make_err(&steps, &format!("Cannot read CA cert: {e}")))?;
    let ca_cert = reqwest::Certificate::from_pem(&ca_pem)
        .map_err(|e| make_err(&steps, &format!("Invalid CA cert: {e}")))?;

    let http_client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none()) // capture the 302
        .add_root_certificate(ca_cert.clone())
        .build()
        .map_err(|e| make_err(&steps, &format!("HTTP client error: {e}")))?;

    let authorize_resp = http_client
        .get(&authorize_url)
        .send()
        .await
        .map_err(|e| make_err(&steps, &format!("Cannot reach AB: {e}")))?;

    if authorize_resp.status() != reqwest::StatusCode::FOUND {
        let status = authorize_resp.status();
        let body = authorize_resp.text().await.unwrap_or_default();
        return Err(make_err(
            &steps,
            &format!("Expected 302 redirect, got {status}: {body}"),
        ));
    }

    let location = authorize_resp
        .headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| make_err(&steps, "No Location header in redirect"))?
        .to_string();

    // Parse the authorization code from the redirect URL
    let code = extract_query_param(&location, "code")
        .ok_or_else(|| make_err(&steps, &format!("No code in redirect URL: {location}")))?;

    let returned_state = extract_query_param(&location, "state");

    steps.push(FlowStep {
        number: 4,
        actor: "AB".into(),
        action: "Validates params, generates authorization code".into(),
        detail: format!(
            "AB blinds client_id via PRF, stores code with PKCE challenge.\n302 redirect with code={}...",
            &code[..16]
        ),
    });

    // Verify state matches (CSRF protection)
    if returned_state.as_deref() != Some(&state_val) {
        return Err(make_err(
            &steps,
            "State parameter mismatch (CSRF protection)",
        ));
    }

    steps.push(FlowStep {
        number: 5,
        actor: "Demo App".into(),
        action: "Receives authorization code + verifies state".into(),
        detail: format!("code = {}...\nstate matches: true", &code[..16]),
    });

    // ── Step 6: Exchange code for id_token at POST /token ────────────
    let token_url = "https://localhost:4001/token";

    let token_client = reqwest::Client::builder()
        .add_root_certificate(ca_cert)
        .build()
        .map_err(|e| make_err(&steps, &format!("HTTP client error: {e}")))?;

    let token_resp = token_client
        .post(token_url)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(format!(
            "grant_type=authorization_code&code={}&redirect_uri={}&code_verifier={}&client_id={}",
            code, redirect_uri, code_verifier, client_id
        ))
        .send()
        .await
        .map_err(|e| make_err(&steps, &format!("Token exchange failed: {e}")))?;

    steps.push(FlowStep {
        number: 6,
        actor: "Demo App -> AB".into(),
        action: "POST /token: exchange code + code_verifier".into(),
        detail: format!(
            "grant_type=authorization_code\ncode={}...\ncode_verifier={}...\nAB verifies: SHA256(code_verifier) == code_challenge",
            &code[..16],
            &code_verifier[..12]
        ),
    });

    if !token_resp.status().is_success() {
        let status = token_resp.status();
        let body = token_resp.text().await.unwrap_or_default();
        return Err(make_err(
            &steps,
            &format!("Token endpoint returned {status}: {body}"),
        ));
    }

    let token_data: serde_json::Value = token_resp
        .json()
        .await
        .map_err(|e| make_err(&steps, &format!("Invalid token response: {e}")))?;

    let id_token = token_data["id_token"]
        .as_str()
        .ok_or_else(|| make_err(&steps, "No id_token in token response"))?
        .to_string();

    steps.push(FlowStep {
        number: 7,
        actor: "AB".into(),
        action: "PKCE verified -> runs partitioned flow".into(),
        detail: "AB blinds app_id -> IB contacts IdP, blinds iss/sub -> tTS computes DI + FROST signs JWT".into(),
    });

    // ── Step 8: Decode and display the JWT ───────────────────────────
    let payload = decode_jwt_payload(&id_token);

    let di = payload
        .as_ref()
        .and_then(|p| p["sub"].as_str())
        .unwrap_or("unknown");

    steps.push(FlowStep {
        number: 8,
        actor: "Demo App".into(),
        action: "Receives id_token, verifies JWT".into(),
        detail: format!(
            "DI (sub) = {}...\naud = {}\nnonce matches: {}",
            &di[..di.len().min(32)],
            payload
                .as_ref()
                .and_then(|p| p["aud"].as_str())
                .unwrap_or("?"),
            payload.as_ref().and_then(|p| p["nonce"].as_str()) == Some(&nonce)
        ),
    });

    Ok(Json(SignInResponse {
        success: true,
        authorize_url: Some(authorize_url),
        token_url: Some(token_url.into()),
        id_token: Some(id_token),
        payload,
        error: None,
        steps,
    }))
}

// ---------------------------------------------------------------------------
// HTML rendering helpers (for callback result pages)
// ---------------------------------------------------------------------------

fn render_error_page(error: &str) -> String {
    ERROR_HTML.replace("{{ERROR_MESSAGE}}", &html_escape(error))
}

fn render_success_page(
    id_token: &str,
    payload: Option<&serde_json::Value>,
    nonce_ok: bool,
) -> String {
    let jwt_parts: Vec<&str> = id_token.split('.').collect();
    let jwt_html = if jwt_parts.len() == 3 {
        format!(
            "<span style='color:#f87171'>{}</span>.<span style='color:#34d399'>{}</span>.<span style='color:#a78bfa'>{}</span>",
            jwt_parts[0], jwt_parts[1], jwt_parts[2]
        )
    } else {
        html_escape(id_token)
    };

    let mut claims_html = String::new();
    if let Some(p) = payload {
        let claim_order = [
            "sub",
            "iss",
            "aud",
            "name",
            "email",
            "email_verified",
            "picture",
            "nonce",
            "jti",
            "iat",
            "exp",
        ];
        let mut shown = std::collections::HashSet::new();

        for &key in &claim_order {
            if let Some(val) = p.get(key) {
                claims_html.push_str(&format_claim_row(key, val));
                shown.insert(key.to_string());
            }
        }

        if let Some(obj) = p.as_object() {
            for (key, val) in obj {
                if !shown.contains(key.as_str()) {
                    claims_html.push_str(&format_claim_row(key, val));
                }
            }
        }
    }

    let payload_json = payload
        .map(|p| serde_json::to_string_pretty(p).unwrap_or_default())
        .unwrap_or_default();

    let sub_display = payload
        .and_then(|p| p["sub"].as_str())
        .map(|s| {
            if s.len() > 32 {
                format!("{}...", &s[..32])
            } else {
                s.to_string()
            }
        })
        .unwrap_or_else(|| "unknown".to_string());

    let name_display = payload
        .and_then(|p| p["name"].as_str())
        .unwrap_or("Unknown User");

    let email_display = payload.and_then(|p| p["email"].as_str()).unwrap_or("");

    let picture_url = payload.and_then(|p| p["picture"].as_str()).unwrap_or("");

    let avatar_html = if !picture_url.is_empty() {
        format!("<img src='{}' style='width:48px;height:48px;border-radius:50%;margin-right:1rem;' alt='avatar'>", html_escape(picture_url))
    } else {
        "<div style='width:48px;height:48px;border-radius:50%;background:linear-gradient(135deg,#ff6b6b,#ffd93d,#6bcb77,#4d96ff);margin-right:1rem;flex-shrink:0;'></div>".to_string()
    };

    let (nonce_badge_class, nonce_status) = if nonce_ok {
        ("badge-nonce-ok", "verified")
    } else {
        ("badge-nonce-fail", "mismatch")
    };

    SUCCESS_HTML
        .replace("{{NONCE_BADGE_CLASS}}", nonce_badge_class)
        .replace("{{NONCE_STATUS}}", nonce_status)
        .replace("{{AVATAR_HTML}}", &avatar_html)
        .replace("{{NAME_DISPLAY}}", &html_escape(name_display))
        .replace("{{EMAIL_DISPLAY}}", &html_escape(email_display))
        .replace("{{SUB_DISPLAY}}", &html_escape(&sub_display))
        .replace("{{JWT_HTML}}", &jwt_html)
        .replace("{{CLAIMS_HTML}}", &claims_html)
        .replace("{{PAYLOAD_JSON}}", &html_escape(&payload_json))
}

fn format_claim_row(key: &str, val: &serde_json::Value) -> String {
    let role = match key {
        "sub" => "Direct Identifier (DI)",
        "iss" => "Issuer",
        "aud" => "Audience (blinded app id)",
        "nonce" => "OIDC replay protection",
        "jti" => "Unique token identifier",
        "iat" => "Issued at (Unix timestamp)",
        "exp" => "Expiration (Unix timestamp)",
        "name" => "User display name (from Google)",
        "email" => "User email (from Google)",
        "email_verified" => "Email verified by Google",
        "picture" => "Profile picture URL (from Google)",
        _ => "",
    };

    let display_val = match val {
        serde_json::Value::String(s) => {
            if s.len() > 64 {
                format!("{}...", &s[..64])
            } else {
                s.clone()
            }
        }
        serde_json::Value::Number(n) => {
            if key == "iat" || key == "exp" {
                if let Some(ts) = n.as_i64() {
                    format!("{ts}")
                } else {
                    n.to_string()
                }
            } else {
                n.to_string()
            }
        }
        serde_json::Value::Bool(b) => b.to_string(),
        other => other.to_string(),
    };

    let tag = match key {
        "aud" | "nonce" | "jti" => " <span style='color:#60a5fa;font-size:0.65rem;'>[OIDC]</span>",
        "name" | "email" | "email_verified" | "picture" => {
            " <span style='color:#a78bfa;font-size:0.65rem;'>[Profile]</span>"
        }
        _ => "",
    };

    format!(
        "<tr><td>{key}{tag}</td><td>{}</td><td style='font-family:-apple-system,sans-serif;color:#8b8fa3;'>{role}</td></tr>",
        html_escape(&display_val),
    )
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Generate a PKCE code_verifier (43-128 chars of unreserved characters)
fn generate_code_verifier() -> String {
    let bytes: [u8; 32] = rand::thread_rng().gen();
    URL_SAFE_NO_PAD.encode(bytes) // 43 chars
}

/// Compute PKCE code_challenge = base64url(SHA256(code_verifier))
fn compute_code_challenge(verifier: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    let hash = hasher.finalize();
    URL_SAFE_NO_PAD.encode(hash)
}

/// Generate a random hex string of the given byte length
fn generate_random_string(byte_len: usize) -> String {
    let mut bytes = vec![0u8; byte_len];
    rand::thread_rng().fill(&mut bytes[..]);
    hex::encode(&bytes)
}

/// Extract a query parameter from a URL
fn extract_query_param(url: &str, key: &str) -> Option<String> {
    let query = url.split_once('?')?.1;
    for pair in query.split('&') {
        if let Some(val) = pair.strip_prefix(&format!("{key}=")) {
            return Some(val.to_string());
        }
    }
    None
}

/// Fetch this RP's own blinded app id from AB (registration metadata), so the
/// RP can validate the token's `aud == blind_app_id` without ever holding k_AB.
async fn fetch_blind_app_id(
    client: &reqwest::Client,
    ab_https_base: &str,
    client_id: &str,
) -> Result<String, String> {
    let resp = client
        .get(format!("{ab_https_base}/client_blind_id"))
        .query(&[("client_id", client_id)])
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("AB /client_blind_id returned {}", resp.status()));
    }
    let v: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    v["blind_app_id"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "no blind_app_id in response".to_string())
}

fn decode_jwt_payload(jwt: &str) -> Option<serde_json::Value> {
    let parts: Vec<&str> = jwt.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let bytes = URL_SAFE_NO_PAD.decode(parts[1]).ok()?;
    let text = String::from_utf8(bytes).ok()?;
    serde_json::from_str(&text).ok()
}

fn make_err(steps: &[FlowStep], msg: &str) -> (StatusCode, Json<SignInResponse>) {
    (
        StatusCode::BAD_GATEWAY,
        Json(SignInResponse {
            success: false,
            authorize_url: None,
            token_url: None,
            id_token: None,
            payload: None,
            error: Some(msg.to_string()),
            steps: steps.to_vec(),
        }),
    )
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    // Ensure certs exist (for the CA: we don't serve TLS ourselves)
    shared::tls::install_default_provider();
    shared::tls::ensure_certs("certs").expect("Failed to generate TLS certificates");

    let ab_https_base = env_or_default("AB_HTTPS_BASE", "https://localhost:4001");
    let ab_http_base = env_or_default("AB_HTTP_BASE", "http://localhost:4010");
    let demo_public_base = env_or_default("DEMO_APP_PUBLIC_BASE", "http://localhost:3000");
    let bind = env_or_default("DEMO_APP_BIND", "127.0.0.1:3000");
    let addr: SocketAddr = bind.parse().expect("Invalid DEMO_APP_BIND");

    let state = Arc::new(AppState {
        pkce_sessions: RwLock::new(HashMap::new()),
        ab_https_base,
        ab_http_base,
        demo_public_base: demo_public_base.clone(),
    });

    let app = Router::new()
        .route("/", get(index))
        .route("/auth/start", get(auth_start))
        .route("/callback", get(callback))
        .route("/api/sign-in", post(sign_in))
        .route("/api/health", get(health))
        .with_state(state);

    tracing::info!("[Demo App] Relying Party on http://{}", addr);
    tracing::info!("[Demo App] Open {} in your browser", demo_public_base);
    tracing::info!("[Demo App] Google redirect flow: GET /auth/start");
    tracing::info!("[Demo App] Legacy proxied flow:  POST /api/sign-in");

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("Failed to bind Demo App");

    axum::serve(listener, app)
        .await
        .expect("Demo App server error");
}
