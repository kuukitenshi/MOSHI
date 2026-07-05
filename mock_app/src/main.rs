//! Mock App: Relying Party simulator
//!
//! HTTPS client that:
//! 1. Sends a login request (with `app_id`) to AB
//! 2. Receives the final JWT
//! 3. Decodes the payload and **verifies the FROST group signature**
//!
//! Run this AFTER all four servers (mock_idp, ab, ib, tts) are up.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use frost_ed25519 as frost;
use shared::types::{LoginRequest, TokenResponse};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    shared::tls::install_default_provider();
    shared::tls::ensure_certs("certs")?;

    // Build HTTPS client that trusts our self-signed CA
    let ca_pem = std::fs::read("certs/ca.pem")?;
    let ca_cert = reqwest::Certificate::from_pem(&ca_pem)?;
    let client = reqwest::Client::builder()
        .add_root_certificate(ca_cert)
        .build()?;

    tracing::info!("══════════════════════════════════════════════════");
    tracing::info!("[Mock App] Starting authentication flow ...");

    let app_id = "app_example_456";

    // ── Registration metadata: learn our own blind_app_id from AB once ──
    // The RP never holds k_AB, but needs blind_app_id to validate the token's
    // aud (the tTS stamps aud = blind_app_id; AB does NOT re-sign).
    let meta: serde_json::Value = client
        .get("https://localhost:4001/client_blind_id")
        .query(&[("client_id", app_id)])
        .send()
        .await?
        .json()
        .await?;
    let expected_aud = meta["blind_app_id"]
        .as_str()
        .ok_or("AB did not return blind_app_id")?
        .to_string();
    tracing::info!("[Mock App] Registered: blind_app_id (expected aud) = {expected_aud}");

    tracing::info!("[Mock App] Step 1: POST https://localhost:4001/login");

    let login_req = LoginRequest {
        app_id: app_id.into(),
    };

    let resp = client
        .post("https://localhost:4001/login")
        .json(&login_req)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await?;
        tracing::error!("[Mock App] Login failed: {status}: {body}");
        return Err(format!("Login failed: {status}").into());
    }

    let token: TokenResponse = resp.json().await?;

    tracing::info!("══════════════════════════════════════════════════");
    tracing::info!("[Mock App] AUTHENTICATION COMPLETE");
    tracing::info!("[Mock App] JWT = {} (DI = sub, decoded below)", token.jwt);

    // ── Decode the JWT payload for display ────────────────────────────
    let parts: Vec<&str> = token.jwt.split('.').collect();
    if parts.len() == 3 {
        if let Ok(payload_bytes) = URL_SAFE_NO_PAD.decode(parts[1]) {
            if let Ok(payload_str) = String::from_utf8(payload_bytes) {
                let payload: serde_json::Value = serde_json::from_str(&payload_str)?;
                tracing::info!(
                    "[Mock App] JWT Payload (decoded):\n{}",
                    serde_json::to_string_pretty(&payload)?
                );
            }
        }
    }

    // ── Verify JWT signature using FROST group verifying key ─────────
    tracing::info!("──────────────────────────────────────────────────");
    tracing::info!("[Mock App] Verifying JWT signature with FROST group public key ...");

    let vk_hex = std::fs::read_to_string("certs/tts_verifying.pub")?;
    let vk_bytes = hex::decode(vk_hex.trim())?;
    let verifying_key = frost::VerifyingKey::deserialize(&vk_bytes)?;

    if parts.len() == 3 {
        let signing_input = format!("{}.{}", parts[0], parts[1]);
        let sig_bytes = URL_SAFE_NO_PAD.decode(parts[2])?;
        let signature = frost::Signature::deserialize(&sig_bytes)?;

        match verifying_key.verify(signing_input.as_bytes(), &signature) {
            Ok(()) => {
                tracing::info!("[Mock App] JWT signature VALID (FROST group signature verified)");
            }
            Err(e) => {
                tracing::error!("[Mock App] JWT signature INVALID: {e}");
                return Err(format!("JWT signature verification failed: {e}").into());
            }
        }
    }

    // ── Validate aud == our blind_app_id (RP knows its own blinded id) ──
    tracing::info!("──────────────────────────────────────────────────");
    if parts.len() == 3 {
        let payload_bytes = URL_SAFE_NO_PAD.decode(parts[1])?;
        let payload: serde_json::Value = serde_json::from_slice(&payload_bytes)?;
        let aud = payload["aud"].as_str().unwrap_or_default();
        if aud == expected_aud {
            tracing::info!("[Mock App] aud check VALID (aud == our blind_app_id)");
        } else {
            tracing::error!(
                "[Mock App] aud MISMATCH: token aud={aud}, expected blind_app_id={expected_aud}"
            );
            return Err("aud does not match our blind_app_id".into());
        }
    }

    // ── Privacy summary ──────────────────────────────────────────────
    tracing::info!("══════════════════════════════════════════════════");
    tracing::info!("[Privacy Summary]");
    tracing::info!("  AB    saw: app_id=\"app_example_456\"   | NEVER saw iss, sub");
    tracing::info!("  IB    saw: iss=\"mock_idp\", sub=\"user_123\" | NEVER saw app_id");
    tracing::info!("  tTS   saw: only blinded values        | NEVER saw app_id, iss, sub");
    tracing::info!("  => No single component saw the full tuple (app_id, iss, sub)");
    tracing::info!("  => JWT signed with REAL FROST threshold signature (t=2, n=3)");
    tracing::info!("══════════════════════════════════════════════════");

    Ok(())
}
