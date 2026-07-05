//! Mock IdP: Identity Provider simulator
//!
//! HTTPS server on port 3001.
//! Returns a hardcoded ID Token with `iss` = "mock_idp" and `sub` = "user_123".

use axum::{routing::get, Json, Router};
use shared::types::IdpResponse;
use std::net::SocketAddr;

/// GET /authorize -> simulates IdP authentication, returns (iss, sub) + profile claims.
async fn authorize() -> Json<IdpResponse> {
    tracing::info!("──────────────────────────────────────────────────");
    tracing::info!("[Mock IdP] /authorize called");
    tracing::info!(
        "[Mock IdP] Returning ID Token: iss=\"mock_idp\", sub=\"user_123\", name=\"Jane Doe\""
    );

    Json(IdpResponse {
        iss: "mock_idp".into(),
        sub: "user_123".into(),
        name: Some("Jane Doe".into()),
        email: Some("jane.doe@example.com".into()),
        email_verified: Some(true),
        picture: Some("https://i.pravatar.cc/150?u=jane.doe@example.com".into()),
    })
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    shared::tls::install_default_provider();
    shared::tls::ensure_certs("certs").expect("Failed to generate TLS certificates");

    let app = Router::new().route("/authorize", get(authorize));
    let addr = SocketAddr::from(([127, 0, 0, 1], 3001));

    let tls_config = axum_server::tls_rustls::RustlsConfig::from_pem_file(
        "certs/mock_idp.pem",
        "certs/mock_idp.key",
    )
    .await
    .expect("Failed to load Mock IdP TLS config");

    tracing::info!("[Mock IdP] Listening on https://{}", addr);
    axum_server::bind_rustls(addr, tls_config)
        .serve(app.into_make_service())
        .await
        .expect("Mock IdP server error");
}
