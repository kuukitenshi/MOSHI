//! Web UI: browser-friendly frontend for the moshi identity broker
//!
//! Plain HTTP server on port 8080 (no TLS: avoids self-signed cert issues
//! in the browser). Serves a static HTML page and proxies requests
//! to AB over HTTPS internally.
//!
//! ## Endpoints
//!
//! - `GET  /`: Main SPA frontend
//! - `GET  /callback`: OIDC redirect callback (receives id_token)
//! - `GET  /api/health`: Health check
//! - `POST /api/login`: Direct flow proxy (original mode)
//! - `POST /api/oidc-demo`: OIDC Implicit Flow demo proxy

use axum::{
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::env;
use std::net::SocketAddr;
use std::sync::Arc;

fn env_or_default(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_string())
}

// ---------------------------------------------------------------------------
// Embedded HTML (compiled into the binary)
// ---------------------------------------------------------------------------

const INDEX_HTML: &str = include_str!("index.html");
const CALLBACK_HTML: &str = include_str!("callback.html");

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct UiLoginRequest {
    app_id: String,
}

#[derive(Debug, Deserialize)]
struct OidcDemoRequest {
    client_id: String,
    redirect_uri: String,
    nonce: String,
    scope: String,
}

#[derive(Debug, Serialize)]
struct UiLoginResponse {
    success: bool,
    jwt: Option<String>,
    di: Option<String>,
    payload: Option<serde_json::Value>,
    error: Option<String>,
    steps: Vec<StepInfo>,
}

#[derive(Debug, Serialize)]
struct OidcDemoResponse {
    success: bool,
    /// The constructed authorize URL (for display)
    authorize_url: String,
    /// JWT returned from the flow
    jwt: Option<String>,
    di: Option<String>,
    payload: Option<serde_json::Value>,
    /// The redirect URL that would be sent to the browser
    redirect_url: Option<String>,
    error: Option<String>,
    steps: Vec<StepInfo>,
}

#[derive(Debug, Serialize, Clone)]
struct StepInfo {
    number: u8,
    component: String,
    description: String,
    detail: String,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

struct AppState {
    http_client: reqwest::Client,
    ab_https_base: String,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// GET /: Serve the single-page frontend
async fn index() -> impl IntoResponse {
    Html(INDEX_HTML)
}

/// GET /callback: OIDC redirect target (parses fragment client-side)
async fn callback() -> impl IntoResponse {
    Html(CALLBACK_HTML)
}

/// GET /api/health: Quick health check
async fn health() -> impl IntoResponse {
    Json(serde_json::json!({ "status": "ok" }))
}

/// POST /api/login: Proxy the login flow to AB (direct mode)
async fn api_login(
    State(state): State<Arc<AppState>>,
    Json(req): Json<UiLoginRequest>,
) -> Result<Json<UiLoginResponse>, (StatusCode, Json<UiLoginResponse>)> {
    let mut steps: Vec<StepInfo> = Vec::new();

    steps.push(StepInfo {
        number: 1,
        component: "Mock App".into(),
        description: "App sends login request".into(),
        detail: format!("POST /login with app_id=\"{}\" to AB (:4001)", req.app_id),
    });

    let login_payload = serde_json::json!({ "app_id": req.app_id });

    let resp = state
        .http_client
        .post(format!("{}/login", state.ab_https_base))
        .json(&login_payload)
        .send()
        .await;

    let resp = match resp {
        Ok(r) => r,
        Err(e) => {
            return Err((
                StatusCode::BAD_GATEWAY,
                Json(UiLoginResponse {
                    success: false,
                    jwt: None,
                    di: None,
                    payload: None,
                    error: Some(format!("Could not reach AB: {e}")),
                    steps,
                }),
            ));
        }
    };

    steps.push(StepInfo {
        number: 2,
        component: "AB".into(),
        description: "Blinds app_id via PRF (HMAC-SHA256)".into(),
        detail: "AB sees app_id in plaintext but NEVER sees iss or sub".into(),
    });
    steps.push(StepInfo {
        number: 3,
        component: "AB".into(),
        description: "Forwards blinded app_id to IB".into(),
        detail: "HTTPS POST to IB (:4002) with blind_app_id only".into(),
    });
    steps.push(StepInfo {
        number: 4,
        component: "IB".into(),
        description: "Contacts Mock IdP for identity assertion".into(),
        detail: "HTTPS GET to Mock IdP (:3001), receives iss + sub".into(),
    });
    steps.push(StepInfo {
        number: 5,
        component: "IB".into(),
        description: "Blinds iss and sub via PRF".into(),
        detail: "IB sees iss/sub in plaintext but NEVER sees app_id".into(),
    });
    steps.push(StepInfo {
        number: 6,
        component: "IB".into(),
        description: "Forwards all blinded values to tTS".into(),
        detail: "HTTPS POST to tTS (:5001) with blind_app_id, blind_iss, blind_sub".into(),
    });
    steps.push(StepInfo {
        number: 7,
        component: "tTS".into(),
        description: "Computes Direct Identifier (DI)".into(),
        detail: "DI = HMAC-SHA256(blind_app_id || blind_iss || blind_sub). tTS NEVER sees any plaintext.".into(),
    });
    steps.push(StepInfo {
        number: 8,
        component: "tTS".into(),
        description: "Signs JWT with FROST threshold signature".into(),
        detail: "Real FROST Ed25519 ceremony: DKG (t=2, n=3) + Round1 (commit) + Round2 (sign) + Aggregate".into(),
    });
    steps.push(StepInfo {
        number: 9,
        component: "IB / AB".into(),
        description: "JWT propagates back through the chain".into(),
        detail: "tTS -> IB -> AB -> App (each node only relays the token)".into(),
    });

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err((
            StatusCode::BAD_GATEWAY,
            Json(UiLoginResponse {
                success: false,
                jwt: None,
                di: None,
                payload: None,
                error: Some(format!("AB returned {status}: {body}")),
                steps,
            }),
        ));
    }

    let token: shared::types::TokenResponse = match resp.json().await {
        Ok(t) => t,
        Err(e) => {
            return Err((
                StatusCode::BAD_GATEWAY,
                Json(UiLoginResponse {
                    success: false,
                    jwt: None,
                    di: None,
                    payload: None,
                    error: Some(format!("Invalid response from AB: {e}")),
                    steps,
                }),
            ));
        }
    };

    let payload = decode_jwt_payload(&token.jwt);
    // DI is the `sub` claim inside the JWT (no longer a separate field).
    let di = payload
        .as_ref()
        .and_then(|p| p.get("sub"))
        .and_then(|s| s.as_str())
        .map(String::from);

    steps.push(StepInfo {
        number: 10,
        component: "Mock App".into(),
        description: "JWT received and verified".into(),
        detail: format!("DI = {}", di.clone().unwrap_or_default()),
    });

    Ok(Json(UiLoginResponse {
        success: true,
        jwt: Some(token.jwt),
        di,
        payload,
        error: None,
        steps,
    }))
}

/// POST /api/oidc-demo: Simulate OIDC Implicit Flow via AB's /authorize
///
/// Instead of redirecting the browser (which can't reach HTTPS :4001),
/// we proxy the request server-side and return the results for display.
async fn api_oidc_demo(
    State(state): State<Arc<AppState>>,
    Json(req): Json<OidcDemoRequest>,
) -> Result<Json<OidcDemoResponse>, (StatusCode, Json<OidcDemoResponse>)> {
    let mut steps: Vec<StepInfo> = Vec::new();

    // Build the authorize URL (for display)
    let authorize_url = format!(
        "{}/authorize?client_id={}&redirect_uri={}&scope={}&nonce={}&response_type=id_token&response_mode=fragment",
        state.ab_https_base, req.client_id, req.redirect_uri, req.scope, req.nonce
    );

    steps.push(StepInfo {
        number: 1,
        component: "Browser".into(),
        description: "Browser redirects to /authorize".into(),
        detail: format!("GET {}", authorize_url),
    });

    // Call AB's /authorize via server-side proxy (follow redirects disabled
    // so we can capture the Location header)
    let proxy_client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .add_root_certificate({
            let ca = std::fs::read("certs/ca.pem").unwrap_or_default();
            reqwest::Certificate::from_pem(&ca).expect("parse CA")
        })
        .build()
        .expect("proxy client");

    let resp = proxy_client.get(&authorize_url).send().await;

    let resp = match resp {
        Ok(r) => r,
        Err(e) => {
            return Err((
                StatusCode::BAD_GATEWAY,
                Json(OidcDemoResponse {
                    success: false,
                    authorize_url,
                    jwt: None,
                    di: None,
                    payload: None,
                    redirect_url: None,
                    error: Some(format!("Could not reach AB: {e}")),
                    steps,
                }),
            ));
        }
    };

    steps.push(StepInfo {
        number: 2,
        component: "AB".into(),
        description: "Validates OIDC parameters".into(),
        detail: format!(
            "client_id=\"{}\", scope=\"{}\", nonce present: {}",
            req.client_id,
            req.scope,
            !req.nonce.is_empty()
        ),
    });
    steps.push(StepInfo {
        number: 3,
        component: "AB".into(),
        description: "Blinds client_id (= app_id) via PRF".into(),
        detail: "AB treats client_id as app_id, blinds with HMAC-SHA256".into(),
    });
    steps.push(StepInfo {
        number: 4,
        component: "AB -> IB".into(),
        description: "Forwards blinded app_id + OIDC metadata to IB".into(),
        detail: "nonce and aud travel as metadata (not privacy-sensitive)".into(),
    });
    steps.push(StepInfo {
        number: 5,
        component: "IB".into(),
        description: "Contacts IdP, blinds iss/sub, forwards to tTS".into(),
        detail: "Same partitioned flow, IB NEVER sees app_id/client_id".into(),
    });
    steps.push(StepInfo {
        number: 6,
        component: "tTS".into(),
        description: "Computes DI + signs OIDC-compliant JWT via FROST".into(),
        detail: "JWT now includes nonce, aud=client_id, jti (OIDC standard claims)".into(),
    });

    // The response should be a 302 redirect with Location containing id_token in fragment
    let status = resp.status();

    if status == reqwest::StatusCode::FOUND {
        // Extract Location header
        let location = resp
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        // Parse id_token from fragment
        let jwt = extract_id_token_from_fragment(&location);
        let payload = jwt.as_deref().and_then(decode_jwt_payload);
        let di = payload
            .as_ref()
            .and_then(|p| p["sub"].as_str())
            .map(String::from);

        steps.push(StepInfo {
            number: 7,
            component: "AB".into(),
            description: "Redirects browser back to redirect_uri".into(),
            detail: format!("302 Location: {}#id_token=...", req.redirect_uri),
        });
        steps.push(StepInfo {
            number: 8,
            component: "Browser".into(),
            description: "App receives id_token in URL fragment".into(),
            detail: "App verifies JWT signature with FROST public key (from /jwks.json)".into(),
        });

        Ok(Json(OidcDemoResponse {
            success: jwt.is_some(),
            authorize_url,
            jwt,
            di,
            payload,
            redirect_url: Some(location),
            error: None,
            steps,
        }))
    } else if status == reqwest::StatusCode::OK {
        // form_post mode: response body is HTML with the token
        let body = resp.text().await.unwrap_or_default();
        // Extract id_token from the form HTML
        let jwt = extract_id_token_from_form(&body);
        let payload = jwt.as_deref().and_then(decode_jwt_payload);
        let di = payload
            .as_ref()
            .and_then(|p| p["sub"].as_str())
            .map(String::from);

        steps.push(StepInfo {
            number: 7,
            component: "AB".into(),
            description: "Returns form_post HTML to browser".into(),
            detail: "Auto-submitting form POSTs id_token to redirect_uri".into(),
        });

        Ok(Json(OidcDemoResponse {
            success: jwt.is_some(),
            authorize_url,
            jwt,
            di,
            payload,
            redirect_url: Some(req.redirect_uri),
            error: None,
            steps,
        }))
    } else {
        let body = resp.text().await.unwrap_or_default();
        Err((
            StatusCode::BAD_GATEWAY,
            Json(OidcDemoResponse {
                success: false,
                authorize_url,
                jwt: None,
                di: None,
                payload: None,
                redirect_url: None,
                error: Some(format!("AB returned {status}: {body}")),
                steps,
            }),
        ))
    }
}

/// Extract id_token from a fragment URL like `http://...#id_token=XXX&token_type=bearer`
fn extract_id_token_from_fragment(url: &str) -> Option<String> {
    let fragment = url.split_once('#')?.1;
    for param in fragment.split('&') {
        if let Some(val) = param.strip_prefix("id_token=") {
            return Some(val.to_string());
        }
    }
    None
}

/// Extract id_token from a form_post HTML body
fn extract_id_token_from_form(html: &str) -> Option<String> {
    // Look for: value="<jwt>"  after name="id_token"
    let idx = html.find("name=\"id_token\"")?;
    let after = &html[idx..];
    let val_start = after.find("value=\"")? + 7;
    let val_end = after[val_start..].find('"')? + val_start;
    Some(after[val_start..val_end].to_string())
}

fn decode_jwt_payload(jwt: &str) -> Option<serde_json::Value> {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
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
    dotenvy::dotenv().ok();

    shared::tls::install_default_provider();
    shared::tls::ensure_certs("certs").expect("Failed to generate TLS certificates");

    let ab_https_base = env_or_default("AB_HTTPS_BASE", "https://localhost:4001");
    let bind = env_or_default("WEB_UI_BIND", "127.0.0.1:8080");
    let addr: SocketAddr = bind.parse().expect("Invalid WEB_UI_BIND");

    // Build HTTPS client that trusts our self-signed CA
    let ca_pem = std::fs::read("certs/ca.pem").expect("read CA cert");
    let ca_cert = reqwest::Certificate::from_pem(&ca_pem).expect("parse CA cert");
    let http_client = reqwest::Client::builder()
        .add_root_certificate(ca_cert)
        .build()
        .expect("build HTTP client");

    let state = Arc::new(AppState {
        http_client,
        ab_https_base,
    });

    let app = Router::new()
        .route("/", get(index))
        .route("/callback", get(callback))
        .route("/api/health", get(health))
        .route("/api/login", post(api_login))
        .route("/api/oidc-demo", post(api_oidc_demo))
        .with_state(state);

    tracing::info!("[Web UI] Serving frontend on http://{}", addr);
    tracing::info!("[Web UI] Open http://{} in your browser", addr);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("Failed to bind Web UI");

    axum::serve(listener, app)
        .await
        .expect("Web UI server error");
}
