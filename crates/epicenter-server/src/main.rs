use anyhow::Result;
use axum::{routing::get, Json, Router};
use epicenter_common::{Gemini4Model, GEMINI_TTS_MODEL, MANAGED_AGENT_BASE};
use epicenter_gemini::GeminiClient;
use epicenter_graph::BlastAnalysisConfig;
use rusqlite::Connection;
use serde::Serialize;
use std::net::SocketAddr;
use tower_http::cors::CorsLayer;
use tracing::info;

#[derive(Debug, Serialize)]
struct HealthResponse {
    status: &'static str,
    service: &'static str,
    models: Vec<&'static str>,
    tts_model: &'static str,
    managed_agent_base: &'static str,
    gemini_configured: bool,
    graph_ring2_radius: f32,
    sqlite_ok: bool,
}

async fn health_handler() -> Json<HealthResponse> {
    let gemini_configured = GeminiClient::from_env().is_ok();
    let sqlite_ok = Connection::open_in_memory().is_ok();
    let graph_cfg = BlastAnalysisConfig::default();

    Json(HealthResponse {
        status: "ok",
        service: "epicenter-server",
        models: Gemini4Model::all().iter().map(|m| m.as_str()).collect(),
        tts_model: GEMINI_TTS_MODEL,
        managed_agent_base: MANAGED_AGENT_BASE,
        gemini_configured,
        graph_ring2_radius: graph_cfg.ring2_radius,
        sqlite_ok,
    })
}

pub fn build_router() -> Router {
    Router::new()
        .route("/api/health", get(health_handler))
        .layer(CorsLayer::permissive())
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "epicenter_server=info,tower_http=info".into()),
        )
        .init();

    let port: u16 = std::env::var("EPICENTER_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8787);
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    info!(%addr, "starting epicenter-server");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, build_router()).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn health_handler_returns_ok() {
        let Json(resp) = health_handler().await;
        assert_eq!(resp.status, "ok");
        assert!(resp.sqlite_ok);
        assert_eq!(resp.models.len(), 4);
    }
}
