use crate::db::infer_media_kind;
use crate::live_judge::run_live_judge_pr;
use crate::state::{apply_event_to_session, AppState};
use axum::{
    extract::{DefaultBodyLimit, Multipart, Path, State},
    http::{header, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::{any, get, post},
    Json, Router,
};
use chrono::Utc;
use epicenter_common::{
    AttachmentMeta, CreatePrRequest, CreatePrResponse, CreateSessionRequest, CreateSessionResponse,
    ExportBundleResponse, FiveSigmaEval, LiveEvent, LiveJudgePrRequest, LiveJudgePrResponse,
    ServerStatusResponse, SessionListItem, TournamentSession, TournamentStage,
};
use epicenter_engine::{EngineRunOptions, TournamentEngine, UploadedAttachmentBytes};
use futures::stream::{self, Stream};
use serde_json::json;
use std::convert::Infallible;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::{broadcast, oneshot};
use tower_http::{
    cors::CorsLayer,
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};
use tracing::{error, info, warn};
use uuid::Uuid;

const MAX_UPLOAD_BYTES: usize = 64 * 1024 * 1024;

/// Structured JSON error wrapper for HTTP API handlers.
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: msg.into(),
        }
    }

    pub fn not_found(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: msg.into(),
        }
    }

    pub fn internal(err: impl std::fmt::Display) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: err.to_string(),
        }
    }

    pub fn bad_gateway(err: impl std::fmt::Display) -> Self {
        Self {
            status: StatusCode::BAD_GATEWAY,
            message: err.to_string(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.message }))).into_response()
    }
}

/// Builds the full Axum `Router` with REST/SSE APIs, permissive CORS, and static WASM hosting.
pub fn build_router(state: AppState) -> Router {
    let dist_dir = std::env::var("EPICENTER_WEB_DIST")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("crates/epicenter-web/dist"));
    build_router_with_dist(state, dist_dir)
}

/// Builds the Axum `Router` with a configurable static WASM `dist_dir` path.
pub fn build_router_with_dist(state: AppState, dist_dir: PathBuf) -> Router {
    let index_file = dist_dir.join("index.html");
    let static_service = ServeDir::new(dist_dir).not_found_service(ServeFile::new(index_file));

    let upload_route = Router::new()
        .route("/api/attachments", post(upload_attachment))
        .layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES));

    Router::new()
        .route("/api/health", get(health_handler))
        .merge(upload_route)
        .route("/api/sessions", post(create_session).get(list_sessions))
        .route("/api/sessions/{id}", get(get_session))
        .route("/api/sessions/{id}/events", get(session_events_sse))
        .route("/api/sessions/{id}/audio.wav", get(get_session_audio))
        .route(
            "/api/sessions/{id}/candidates/{strategy_id}/export",
            get(export_candidate_bundle),
        )
        .route(
            "/api/sessions/{id}/candidates/{strategy_id}/pr",
            post(create_candidate_pr),
        )
        .route("/api/eval/latest", get(get_latest_eval))
        .route("/api/eval/judge-pr", post(judge_pr_handler))
        .route("/api/eval/{run_id}", get(get_eval_by_id))
        .route("/api/{*rest}", any(api_not_found))
        .fallback_service(static_service)
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn api_not_found() -> ApiError {
    ApiError::not_found("API endpoint not found")
}

/// `GET /api/health`
pub async fn health_handler(State(state): State<AppState>) -> Json<ServerStatusResponse> {
    Json(ServerStatusResponse {
        status: "ok".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        has_gemini_key: state.engine.has_live_gemini(),
        has_github_token: state.github.has_token(),
    })
}

/// `POST /api/attachments`
pub async fn upload_attachment(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<Json<AttachmentMeta>, ApiError> {
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::bad_request(format!("invalid multipart payload: {e}")))?
    {
        let filename = field
            .file_name()
            .map(str::to_string)
            .or_else(|| field.name().map(str::to_string))
            .unwrap_or_else(|| "attachment.bin".to_string());

        let mime_type = field
            .content_type()
            .map(str::to_string)
            .filter(|m| !m.trim().is_empty())
            .unwrap_or_else(|| infer_mime_from_filename(&filename).to_string());

        let data = field
            .bytes()
            .await
            .map_err(|e| ApiError::bad_request(format!("failed reading attachment bytes: {e}")))?;

        let meta = AttachmentMeta {
            id: Uuid::new_v4(),
            filename,
            media_kind: infer_media_kind(&mime_type),
            mime_type,
            size_bytes: data.len() as u64,
            file_uri: None,
        };

        state
            .db
            .insert_attachment(&meta, &data)
            .map_err(ApiError::internal)?;

        return Ok(Json(meta));
    }

    Err(ApiError::bad_request(
        "multipart request must include at least one file field",
    ))
}

/// `POST /api/sessions`
pub async fn create_session(
    State(state): State<AppState>,
    Json(mut req): Json<CreateSessionRequest>,
) -> Result<Json<CreateSessionResponse>, ApiError> {
    if req.repo_url.trim().is_empty() {
        return Err(ApiError::bad_request("repo_url is required"));
    }
    if req.problem_statement.trim().is_empty() {
        return Err(ApiError::bad_request("problem_statement is required"));
    }

    // Load uploaded attachment bytes from SQLite for every requested attachment_id.
    let loaded = state
        .db
        .get_attachments_by_ids(&req.attachment_ids)
        .map_err(ApiError::internal)?;

    let mut uploaded_attachments = Vec::with_capacity(loaded.len());
    for (meta, bytes) in loaded {
        if !req.attachments.iter().any(|a| a.id == meta.id) {
            req.attachments.push(meta.clone());
        }
        uploaded_attachments.push(UploadedAttachmentBytes { meta, bytes });
    }

    let session = TournamentEngine::initialize_session(&req);
    let session_id = session.id;

    state
        .db
        .upsert_session(&session)
        .map_err(ApiError::internal)?;

    let tx = state.get_or_create_channel(session_id);
    let _ = tx.send(LiveEvent::SessionCreated {
        session_id,
        timestamp: session.created_at,
    });

    spawn_tournament_background(state.clone(), session.clone(), uploaded_attachments, tx);

    Ok(Json(CreateSessionResponse { session }))
}

fn spawn_tournament_background(
    state: AppState,
    initial_session: TournamentSession,
    attachments: Vec<UploadedAttachmentBytes>,
    tx: broadcast::Sender<LiveEvent>,
) {
    let session_id = initial_session.id;
    let mut rx = tx.subscribe();
    let (stop_tx, mut stop_rx) = oneshot::channel::<()>();

    // Incremental SQLite persistence listener while the tournament streams LiveEvents.
    let db_listener = state.db.clone();
    let mut working_session = initial_session.clone();
    let listener_handle = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut stop_rx => {
                    // Drain any buffered events before exiting so intermediate state is never lost.
                    while let Ok(ev) = rx.try_recv() {
                        if apply_event_to_session(&mut working_session, &ev) {
                            let _ = db_listener.upsert_session(&working_session);
                        }
                    }
                    break;
                }
                recv = rx.recv() => {
                    match recv {
                        Ok(ev) => {
                            if apply_event_to_session(&mut working_session, &ev) {
                                if let Err(err) = db_listener.upsert_session(&working_session) {
                                    warn!(%session_id, %err, "failed to persist incremental session update");
                                }
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            warn!(%session_id, skipped, "session persistence listener lagged");
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }
    });

    // Authoritative engine execution task.
    tokio::spawn(async move {
        let mut opts = EngineRunOptions::default();
        opts.github_token = state.github.token().map(str::to_string);

        let result = state
            .engine
            .execute_session(initial_session, attachments, opts, Some(tx.clone()))
            .await;

        // Stop the incremental listener first so our authoritative final write cannot race with it.
        let _ = stop_tx.send(());
        let _ = listener_handle.await;

        match result {
            Ok(output) => {
                let mut final_session = output.session;
                let mut ready_audio_url = None;
                if let Some(wav_bytes) = output.briefing_wav {
                    match state.db.save_audio_briefing(session_id, &wav_bytes) {
                        Ok(()) => {
                            let audio_url = format!("/api/sessions/{session_id}/audio.wav");
                            final_session.audio_briefing_url = Some(audio_url.clone());
                            ready_audio_url = Some(audio_url);
                        }
                        Err(err) => {
                            error!(%session_id, %err, "failed to persist synthesized WAV briefing");
                        }
                    }
                }
                final_session.updated_at = Utc::now();
                if let Err(err) = state.db.upsert_session(&final_session) {
                    error!(%session_id, %err, "failed to persist final tournament session");
                } else {
                    info!(%session_id, stage = ?final_session.stage, "tournament session finalized");
                }
                if let Some(audio_url) = ready_audio_url {
                    let _ = tx.send(LiveEvent::AudioBriefingReady {
                        session_id,
                        audio_url,
                    });
                }
            }
            Err(err) => {
                error!(%session_id, %err, "tournament execution failed");
                if let Ok(Some(mut failed_session)) = state.db.get_session(session_id) {
                    failed_session.stage = TournamentStage::Failed;
                    failed_session.updated_at = Utc::now();
                    let _ = state.db.upsert_session(&failed_session);
                }
                let _ = tx.send(LiveEvent::Error {
                    session_id: Some(session_id),
                    message: err.to_string(),
                });
                let _ = tx.send(LiveEvent::StageChanged {
                    session_id,
                    stage: TournamentStage::Failed,
                    message: format!("Tournament failed: {err}"),
                });
            }
        }
    });
}

/// `GET /api/sessions`
pub async fn list_sessions(
    State(state): State<AppState>,
) -> Result<Json<Vec<SessionListItem>>, ApiError> {
    let items = state.db.list_sessions().map_err(ApiError::internal)?;
    Ok(Json(items))
}

/// `GET /api/sessions/{id}`
pub async fn get_session(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<TournamentSession>, ApiError> {
    let session = state
        .db
        .get_session(id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found(format!("session '{id}' not found")))?;
    Ok(Json(session))
}

/// `GET /api/sessions/{id}/events`
pub async fn session_events_sse(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let session = state
        .db
        .get_session(id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found(format!("session '{id}' not found")))?;

    // Subscribe BEFORE constructing the initial sync event so no transition is missed.
    let rx = state.subscribe(id);

    let initial_event = LiveEvent::StageChanged {
        session_id: id,
        stage: session.stage,
        message: format!("Connected to session stream (stage: {:?})", session.stage),
    };

    let initial_stream = stream::once(async move { Ok(encode_sse_event(&initial_event)) });

    let live_stream = stream::unfold(rx, move |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(ev) => return Some((Ok(encode_sse_event(&ev)), rx)),
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    let notice = LiveEvent::Log {
                        session_id: id,
                        level: epicenter_common::LogLevel::Warn,
                        source: "epicenter-server".to_string(),
                        message: format!("SSE subscriber skipped {skipped} lagged events"),
                        timestamp: Utc::now(),
                    };
                    return Some((Ok(encode_sse_event(&notice)), rx));
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    });

    let combined = futures::StreamExt::chain(initial_stream, live_stream);

    Ok(Sse::new(combined).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    ))
}

fn encode_sse_event(ev: &LiveEvent) -> Event {
    let payload = serde_json::to_string(ev).unwrap_or_else(|_| "{}".to_string());
    Event::default().data(payload)
}

/// `GET /api/sessions/{id}/audio.wav`
pub async fn get_session_audio(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let wav = state
        .db
        .get_audio_briefing(id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| {
            ApiError::not_found(format!("audio briefing not yet available for session '{id}'"))
        })?;

    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "audio/wav"),
            (header::CACHE_CONTROL, "public, max-age=3600"),
        ],
        wav,
    )
        .into_response())
}

/// `GET /api/sessions/{id}/candidates/{strategy_id}/export`
pub async fn export_candidate_bundle(
    State(state): State<AppState>,
    Path((id, strategy_id)): Path<(Uuid, String)>,
) -> Result<Json<ExportBundleResponse>, ApiError> {
    let session = state
        .db
        .get_session(id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found(format!("session '{id}' not found")))?;

    let bundle = TournamentEngine::build_export_bundle(&session, &strategy_id).ok_or_else(
        || {
            ApiError::not_found(format!(
                "candidate '{strategy_id}' not found in session '{id}'"
            ))
        },
    )?;

    Ok(Json(bundle))
}

/// `POST /api/sessions/{id}/candidates/{strategy_id}/pr`
pub async fn create_candidate_pr(
    State(state): State<AppState>,
    Path((id, strategy_id)): Path<(Uuid, String)>,
    Json(req): Json<CreatePrRequest>,
) -> Result<Json<CreatePrResponse>, ApiError> {
    let session = state
        .db
        .get_session(id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found(format!("session '{id}' not found")))?;

    let bundle = TournamentEngine::build_export_bundle(&session, &strategy_id).ok_or_else(
        || {
            ApiError::not_found(format!(
                "candidate '{strategy_id}' not found in session '{id}'"
            ))
        },
    )?;

    let created = state
        .github
        .create_pr_from_diff(
            &session.intake.repo_url,
            &strategy_id,
            session.id,
            &bundle.git_diff,
            &bundle.audit_markdown,
            &req,
        )
        .await
        .map_err(ApiError::bad_gateway)?;

    Ok(Json(created))
}

/// `GET /api/eval/latest`
pub async fn get_latest_eval(
    State(state): State<AppState>,
) -> Result<Json<Option<FiveSigmaEval>>, ApiError> {
    let latest = state
        .db
        .get_latest_eval_run_or_seed()
        .map_err(ApiError::internal)?;
    Ok(Json(latest))
}

/// `POST /api/eval/judge-pr`
pub async fn judge_pr_handler(
    State(state): State<AppState>,
    Json(req): Json<LiveJudgePrRequest>,
) -> Result<Json<LiveJudgePrResponse>, ApiError> {
    if req.pr_url.trim().is_empty() {
        return Err(ApiError::bad_request("pr_url is required"));
    }
    let resp = run_live_judge_pr(&state, &req)
        .await
        .map_err(ApiError::bad_gateway)?;
    Ok(Json(resp))
}

/// `GET /api/eval/{run_id}`
pub async fn get_eval_by_id(
    State(state): State<AppState>,
    Path(run_id): Path<Uuid>,
) -> Result<Json<FiveSigmaEval>, ApiError> {
    let eval = state
        .db
        .get_eval_run(run_id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found(format!("eval run '{run_id}' not found")))?;
    Ok(Json(eval))
}

fn infer_mime_from_filename(filename: &str) -> &'static str {
    let lower = filename.to_ascii_lowercase();
    if lower.ends_with(".mp4") {
        "video/mp4"
    } else if lower.ends_with(".webm") {
        "video/webm"
    } else if lower.ends_with(".mov") {
        "video/quicktime"
    } else if lower.ends_with(".png") {
        "image/png"
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "image/jpeg"
    } else if lower.ends_with(".gif") {
        "image/gif"
    } else if lower.ends_with(".webp") {
        "image/webp"
    } else if lower.ends_with(".pdf") {
        "application/pdf"
    } else if lower.ends_with(".wav") {
        "audio/wav"
    } else if lower.ends_with(".mp3") {
        "audio/mpeg"
    } else if lower.ends_with(".ogg") {
        "audio/ogg"
    } else if lower.ends_with(".log") || lower.ends_with(".txt") || lower.ends_with(".md") {
        "text/plain"
    } else if lower.ends_with(".json") {
        "application/json"
    } else {
        "application/octet-stream"
    }
}
