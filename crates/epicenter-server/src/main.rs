pub mod db;
pub mod github;
pub mod live_judge;
pub mod routes;
pub mod state;

#[cfg(test)]
mod tests;

use anyhow::Result;
use db::DbStore;
use routes::build_router;
use state::AppState;
use std::net::SocketAddr;
use std::path::PathBuf;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "epicenter_server=info,epicenter_engine=info,tower_http=info".into()),
        )
        .init();

    let db_path = std::env::var("EPICENTER_DB_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("epicenter.db"));
    let db = DbStore::open(&db_path)?;
    let state = AppState::from_env(db);

    let port: u16 = std::env::var("EPICENTER_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8787);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));

    info!(
        %addr,
        db = %db_path.display(),
        has_gemini_key = state.engine.has_live_gemini(),
        has_github_token = state.github.has_token(),
        "starting epicenter-server"
    );

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, build_router(state)).await?;
    Ok(())
}
