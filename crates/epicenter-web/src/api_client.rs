use chrono::{DateTime, Utc};
use epicenter_common::{
    AttachmentMeta, CandidateArenaMetrics, CandidateResult, CandidateStatus, CreatePrRequest,
    CreatePrResponse, CreateSessionRequest, CreateSessionResponse, ExportBundleResponse, LiveEvent,
    LogLevel, ServerStatusResponse, SessionListItem, TaskKind, TournamentSession, TournamentStage,
};
use gloo_net::http::Request;
use serde::{de::DeserializeOwned, Deserialize};
use uuid::Uuid;
use wasm_bindgen::{closure::Closure, JsCast, JsValue};
use web_sys::{EventSource, FormData, MessageEvent};

/// Response from `POST /api/presets/real-bug` providing a real pinned GitHub bug + screenshot attachment.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RealBugPresetResponse {
    pub repo_url: String,
    #[serde(default)]
    pub commit_sha: Option<String>,
    #[serde(default)]
    pub git_ref: Option<String>,
    #[serde(default)]
    pub pr_url: Option<String>,
    pub task_kind: TaskKind,
    pub title: String,
    pub problem_statement: String,
    #[serde(default)]
    pub attachment: Option<AttachmentMeta>,
}

/// Single extracted video keyframe returned by `GET /api/attachments/{id}/keyframes`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct KeyframeItem {
    pub index: usize,
    pub timestamp_hint: String,
    pub data_url: String,
}

/// Response from `GET /api/attachments/{id}/keyframes`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AttachmentKeyframesResponse {
    pub attachment_id: String,
    #[serde(default)]
    pub keyframes: Vec<KeyframeItem>,
}

/// Single structured entry rendered in the live Agent Telemetry Drawer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelemetryLogEntry {
    pub timestamp: DateTime<Utc>,
    pub level: LogLevel,
    pub source: String,
    pub message: String,
}

/// Optional runtime API base override (`window.__EPICENTER_API_BASE__`), defaulting to same-origin.
pub fn api_base() -> String {
    web_sys::window()
        .and_then(|w| js_sys::Reflect::get(&w, &JsValue::from_str("__EPICENTER_API_BASE__")).ok())
        .and_then(|v| v.as_string())
        .map(|s| s.trim_end_matches('/').to_string())
        .unwrap_or_default()
}

/// Resolves a server-relative path (such as `/api/sessions/{id}/audio.wav`) against `api_base()`.
pub fn api_url(path: &str) -> String {
    if path.starts_with("http://") || path.starts_with("https://") {
        return path.to_string();
    }
    let base = api_base();
    if path.starts_with('/') {
        format!("{base}{path}")
    } else {
        format!("{base}/{path}")
    }
}

/// Direct media content URL (`GET /api/attachments/{id}`) usable as `src` on `<img>`, `<video>`, `<audio>`.
pub fn attachment_content_url(id: Uuid) -> String {
    api_url(&format!("/api/attachments/{id}"))
}

async fn parse_json_response<T: DeserializeOwned>(
    resp: gloo_net::http::Response,
) -> Result<T, String> {
    let status = resp.status();
    if !resp.ok() {
        let body = resp.text().await.unwrap_or_default();
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&body) {
            if let Some(msg) = val.get("error").and_then(|v| v.as_str()) {
                return Err(format!("HTTP {status}: {msg}"));
            }
            if let Some(msg) = val.get("message").and_then(|v| v.as_str()) {
                return Err(format!("HTTP {status}: {msg}"));
            }
        }
        let detail = if body.trim().is_empty() {
            resp.status_text()
        } else {
            body
        };
        return Err(format!("HTTP {status}: {detail}"));
    }
    resp.json::<T>()
        .await
        .map_err(|e| format!("Failed to parse JSON response: {e}"))
}

/// `GET /api/health`
pub async fn fetch_server_status() -> Result<ServerStatusResponse, String> {
    let resp = Request::get(&api_url("/api/health"))
        .send()
        .await
        .map_err(|e| format!("Health check failed: {e}"))?;
    parse_json_response(resp).await
}

/// `POST /api/attachments` uploading a browser `web_sys::File` via multipart/form-data.
pub async fn upload_attachment(file: &web_sys::File) -> Result<AttachmentMeta, String> {
    let form = FormData::new().map_err(|e| format!("FormData init failed: {e:?}"))?;
    form.append_with_blob_and_filename("file", file, &file.name())
        .map_err(|e| format!("Failed to append file to FormData: {e:?}"))?;

    let resp = Request::post(&api_url("/api/attachments"))
        .body(form)
        .map_err(|e| format!("Request build error: {e}"))?
        .send()
        .await
        .map_err(|e| format!("Attachment upload failed: {e}"))?;
    parse_json_response(resp).await
}

/// `POST /api/attachments` uploading an in-memory `web_sys::Blob` with an explicit filename.
pub async fn upload_blob_attachment(
    blob: &web_sys::Blob,
    filename: &str,
) -> Result<AttachmentMeta, String> {
    let form = FormData::new().map_err(|e| format!("FormData init failed: {e:?}"))?;
    form.append_with_blob_and_filename("file", blob, filename)
        .map_err(|e| format!("Failed to append blob to FormData: {e:?}"))?;

    let resp = Request::post(&api_url("/api/attachments"))
        .body(form)
        .map_err(|e| format!("Request build error: {e}"))?
        .send()
        .await
        .map_err(|e| format!("Attachment upload failed: {e}"))?;
    parse_json_response(resp).await
}

/// `GET /api/attachments/{id}/keyframes` returning extracted timestamped PNG keyframes for video attachments.
pub async fn fetch_attachment_keyframes(id: Uuid) -> Result<AttachmentKeyframesResponse, String> {
    let resp = Request::get(&api_url(&format!("/api/attachments/{id}/keyframes")))
        .send()
        .await
        .map_err(|e| format!("Attachment keyframes request failed: {e}"))?;
    parse_json_response(resp).await
}

/// `POST /api/presets/real-bug` returning a pinned real GitHub bug preset with its screenshot attachment.
pub async fn load_real_bug_preset() -> Result<RealBugPresetResponse, String> {
    let resp = Request::post(&api_url("/api/presets/real-bug"))
        .send()
        .await
        .map_err(|e| format!("Real bug preset request failed: {e}"))?;
    parse_json_response(resp).await
}

/// `POST /api/sessions`
pub async fn create_session(req: &CreateSessionRequest) -> Result<CreateSessionResponse, String> {
    let resp = Request::post(&api_url("/api/sessions"))
        .json(req)
        .map_err(|e| format!("Failed to serialize create session request: {e}"))?
        .send()
        .await
        .map_err(|e| format!("Create session request failed: {e}"))?;
    parse_json_response(resp).await
}

/// `GET /api/sessions`
pub async fn list_sessions() -> Result<Vec<SessionListItem>, String> {
    let resp = Request::get(&api_url("/api/sessions"))
        .send()
        .await
        .map_err(|e| format!("List sessions failed: {e}"))?;
    parse_json_response(resp).await
}

/// `GET /api/sessions/{id}`
pub async fn get_session(session_id: Uuid) -> Result<TournamentSession, String> {
    let resp = Request::get(&api_url(&format!("/api/sessions/{session_id}")))
        .send()
        .await
        .map_err(|e| format!("Fetch session failed: {e}"))?;
    parse_json_response(resp).await
}

/// `GET /api/sessions/{id}/candidates/{strategy_id}/export`
pub async fn export_candidate_bundle(
    session_id: Uuid,
    strategy_id: &str,
) -> Result<ExportBundleResponse, String> {
    let resp = Request::get(&api_url(&format!(
        "/api/sessions/{session_id}/candidates/{strategy_id}/export"
    )))
    .send()
    .await
    .map_err(|e| format!("Export bundle request failed: {e}"))?;
    parse_json_response(resp).await
}

/// `POST /api/sessions/{id}/candidates/{strategy_id}/pr`
pub async fn create_candidate_pr(
    session_id: Uuid,
    strategy_id: &str,
    req: &CreatePrRequest,
) -> Result<CreatePrResponse, String> {
    let resp = Request::post(&api_url(&format!(
        "/api/sessions/{session_id}/candidates/{strategy_id}/pr"
    )))
    .json(req)
    .map_err(|e| format!("Failed to serialize PR request: {e}"))?
    .send()
    .await
    .map_err(|e| format!("Create PR request failed: {e}"))?;
    parse_json_response(resp).await
}

/// RAII handle wrapping an active `EventSource` SSE connection and its JS closures.
/// Dropping or calling `close()` cleanly detaches handlers and terminates the browser stream.
pub struct SseSubscription {
    source: EventSource,
    _on_message: Closure<dyn FnMut(MessageEvent)>,
    _on_error: Closure<dyn FnMut(JsValue)>,
}

impl SseSubscription {
    pub fn close(&self) {
        self.source.set_onmessage(None);
        self.source.set_onerror(None);
        self.source.close();
    }
}

impl Drop for SseSubscription {
    fn drop(&mut self) {
        self.close();
    }
}

/// Opens `/api/sessions/{session_id}/events` via browser `EventSource` and streams parsed `LiveEvent`s.
pub fn subscribe_session_events<F, E>(
    session_id: Uuid,
    mut on_event: F,
    mut on_error: E,
) -> Result<SseSubscription, String>
where
    F: FnMut(LiveEvent) + 'static,
    E: FnMut(String) + 'static,
{
    let url = api_url(&format!("/api/sessions/{session_id}/events"));
    let source = EventSource::new(&url)
        .map_err(|e| format!("Failed to open SSE EventSource ({url}): {e:?}"))?;

    let on_message = Closure::<dyn FnMut(MessageEvent)>::new(move |ev: MessageEvent| {
        if let Some(raw) = ev.data().as_string() {
            match serde_json::from_str::<LiveEvent>(&raw) {
                Ok(parsed) => on_event(parsed),
                Err(err) => {
                    on_event(LiveEvent::Log {
                        session_id,
                        level: LogLevel::Debug,
                        source: "sse".to_string(),
                        message: format!("Skipped unparsed SSE frame: {err}"),
                        timestamp: Utc::now(),
                    });
                }
            }
        }
    });

    let on_err = Closure::<dyn FnMut(JsValue)>::new(move |_| {
        on_error("SSE stream interrupted (browser will auto-reconnect)".to_string());
    });

    source.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
    source.set_onerror(Some(on_err.as_ref().unchecked_ref()));

    Ok(SseSubscription {
        source,
        _on_message: on_message,
        _on_error: on_err,
    })
}

/// Applies an incoming `LiveEvent` incrementally onto the active `TournamentSession` state.
/// Returns `true` when the caller should perform a full `GET /api/sessions/{id}` sync
/// (e.g. after tournament completion or audio ready so `war_room_script` and badges stay exact).
pub fn apply_live_event(session: &mut TournamentSession, event: &LiveEvent) -> bool {
    session.updated_at = Utc::now();
    match event {
        LiveEvent::SessionCreated { .. } => false,
        LiveEvent::StageChanged { stage, .. } => {
            session.stage = *stage;
            matches!(stage, TournamentStage::Completed | TournamentStage::Failed)
        }
        LiveEvent::IntakeReady { brief, .. } => {
            session.intake = (**brief).clone();
            false
        }
        LiveEvent::RedGateVerified { red_gate, .. } => {
            session.red_gate = Some((**red_gate).clone());
            false
        }
        LiveEvent::StrategyPlanReady { plan, .. } => {
            if session.candidates.is_empty() {
                session.candidates = plan
                    .strategies
                    .iter()
                    .map(|s| CandidateResult::pending(s.id.clone()))
                    .collect();
            }
            session.strategy_plan = Some((**plan).clone());
            false
        }
        LiveEvent::CandidateStatusChanged {
            strategy_id,
            status,
            ..
        } => {
            if let Some(c) = session
                .candidates
                .iter_mut()
                .find(|c| c.strategy_id == *strategy_id)
            {
                c.status = *status;
            } else {
                let mut c = CandidateResult::pending(strategy_id.clone());
                c.status = *status;
                session.candidates.push(c);
            }
            false
        }
        LiveEvent::CandidateUpdated { candidate, .. } => {
            if let Some(existing) = session
                .candidates
                .iter_mut()
                .find(|c| c.strategy_id == candidate.strategy_id)
            {
                *existing = (**candidate).clone();
            } else {
                session.candidates.push((**candidate).clone());
            }
            false
        }
        LiveEvent::ArenaTrialSample {
            strategy_id,
            baseline_ms,
            candidate_ms,
            ..
        } => {
            let candidate = if let Some(c) = session
                .candidates
                .iter_mut()
                .find(|c| c.strategy_id == *strategy_id)
            {
                c
            } else {
                session
                    .candidates
                    .push(CandidateResult::pending(strategy_id.clone()));
                session.candidates.last_mut().unwrap()
            };
            let mut base_samples = candidate
                .arena_metrics
                .as_ref()
                .map(|m| m.baseline_samples_ms.clone())
                .unwrap_or_default();
            let mut cand_samples = candidate
                .arena_metrics
                .as_ref()
                .map(|m| m.candidate_samples_ms.clone())
                .unwrap_or_default();
            let peak_mem = candidate
                .arena_metrics
                .as_ref()
                .and_then(|m| m.peak_memory_kb);
            let calls = candidate
                .arena_metrics
                .as_ref()
                .and_then(|m| m.call_or_query_count);
            base_samples.push(*baseline_ms);
            cand_samples.push(*candidate_ms);
            candidate.arena_metrics = Some(CandidateArenaMetrics::from_interleaved_samples(
                base_samples,
                cand_samples,
                peak_mem,
                calls,
            ));
            false
        }
        LiveEvent::ArenaCandidateDone {
            strategy_id,
            metrics,
            ..
        } => {
            if let Some(c) = session
                .candidates
                .iter_mut()
                .find(|c| c.strategy_id == *strategy_id)
            {
                c.arena_metrics = Some(metrics.clone());
                c.status = CandidateStatus::BenchmarkDone;
            }
            false
        }
        LiveEvent::ArenaCompleted { summary, .. } => {
            session.arena_summary = Some(summary.clone());
            false
        }
        LiveEvent::BlastRadiusReady {
            strategy_id,
            report,
            ..
        } => {
            if let Some(c) = session
                .candidates
                .iter_mut()
                .find(|c| c.strategy_id == *strategy_id)
            {
                c.blast_radius = Some((**report).clone());
            }
            false
        }
        LiveEvent::HazardAuditReady {
            strategy_id,
            hazards,
            composite_risk_score,
            ..
        } => {
            if let Some(c) = session
                .candidates
                .iter_mut()
                .find(|c| c.strategy_id == *strategy_id)
            {
                if let Some(br) = c.blast_radius.as_mut() {
                    br.semantic_hazards = hazards.clone();
                    br.composite_risk_score = *composite_risk_score;
                }
                c.status = CandidateStatus::Audited;
            }
            false
        }
        LiveEvent::AudioBriefingReady { audio_url, .. } => {
            session.audio_briefing_url = Some(audio_url.clone());
            true
        }
        LiveEvent::TournamentCompleted { .. } => {
            session.stage = TournamentStage::Completed;
            true
        }
        LiveEvent::Log { .. } => false,
        LiveEvent::Error { .. } => false,
    }
}

/// Converts any `LiveEvent` into a crisp human-readable entry for the streaming telemetry drawer.
pub fn live_event_to_log(event: &LiveEvent) -> TelemetryLogEntry {
    match event {
        LiveEvent::SessionCreated { timestamp, .. } => TelemetryLogEntry {
            timestamp: *timestamp,
            level: LogLevel::Info,
            source: "orchestrator".to_string(),
            message: "Tournament session initialized.".to_string(),
        },
        LiveEvent::StageChanged { stage, message, .. } => TelemetryLogEntry {
            timestamp: Utc::now(),
            level: if matches!(stage, TournamentStage::Failed) {
                LogLevel::Error
            } else {
                LogLevel::Info
            },
            source: format!("stage::{stage:?}").to_ascii_lowercase(),
            message: message.clone(),
        },
        LiveEvent::IntakeReady { brief, .. } => TelemetryLogEntry {
            timestamp: Utc::now(),
            level: LogLevel::Info,
            source: "hydrogen".to_string(),
            message: format!(
                "Intake synthesized: '{}' ({} findings, {} suspect files)",
                brief.title,
                brief.multimodal_findings.len(),
                brief.suspect_files.len()
            ),
        },
        LiveEvent::RedGateVerified { red_gate, .. } => TelemetryLogEntry {
            timestamp: Utc::now(),
            level: LogLevel::Info,
            source: "red-gate".to_string(),
            message: format!("Red-Gate locked with command `{}`", red_gate.test_command()),
        },
        LiveEvent::StrategyPlanReady { plan, .. } => TelemetryLogEntry {
            timestamp: Utc::now(),
            level: LogLevel::Info,
            source: "carbon-architect".to_string(),
            message: format!(
                "Planned K={} orthogonal strategies ({})",
                plan.strategies.len(),
                plan.shared_benchmark_spec.name
            ),
        },
        LiveEvent::CandidateStatusChanged {
            strategy_id,
            status,
            detail,
            ..
        } => TelemetryLogEntry {
            timestamp: Utc::now(),
            level: if matches!(status, CandidateStatus::FailedTests) {
                LogLevel::Warn
            } else {
                LogLevel::Info
            },
            source: format!("neon::{strategy_id}"),
            message: format!("[{status:?}] {detail}"),
        },
        LiveEvent::CandidateUpdated { candidate, .. } => TelemetryLogEntry {
            timestamp: Utc::now(),
            level: LogLevel::Info,
            source: format!("neon::{}", candidate.strategy_id),
            message: format!(
                "Candidate updated: status={:?}, repro={}, regression={}",
                candidate.status, candidate.repro_passed, candidate.regression_suite_passed
            ),
        },
        LiveEvent::ArenaTrialSample {
            strategy_id,
            trial_index,
            baseline_ms,
            candidate_ms,
            ..
        } => TelemetryLogEntry {
            timestamp: Utc::now(),
            level: LogLevel::Debug,
            source: format!("arena::{strategy_id}"),
            message: format!(
                "Trial #{trial_index}: baseline={baseline_ms:.2}ms vs candidate={candidate_ms:.2}ms"
            ),
        },
        LiveEvent::ArenaCandidateDone {
            strategy_id,
            metrics,
            ..
        } => TelemetryLogEntry {
            timestamp: Utc::now(),
            level: LogLevel::Info,
            source: format!("arena::{strategy_id}"),
            message: format!(
                "Arena complete: p50={:.2}ms p95={:.2}ms speedup={:+.1}%",
                metrics.p50_ms, metrics.p95_ms, metrics.speedup_pct
            ),
        },
        LiveEvent::ArenaCompleted { summary, .. } => TelemetryLogEntry {
            timestamp: Utc::now(),
            level: LogLevel::Info,
            source: "arena".to_string(),
            message: format!(
                "Benchmark complete ({} trials): Pareto frontier {:?}, recommended {:?}",
                summary.trials_per_candidate,
                summary.pareto_frontier_ids,
                summary.recommended_strategy_id
            ),
        },
        LiveEvent::BlastRadiusReady {
            strategy_id,
            report,
            ..
        } => TelemetryLogEntry {
            timestamp: Utc::now(),
            level: LogLevel::Info,
            source: format!("graph::{strategy_id}"),
            message: format!(
                "Blast radius mapped: {} nodes, {} dark zones, risk={:.1}",
                report.total_nodes(),
                report.dark_zone_count(),
                report.composite_risk_score
            ),
        },
        LiveEvent::HazardAuditReady {
            strategy_id,
            hazards,
            composite_risk_score,
            ..
        } => TelemetryLogEntry {
            timestamp: Utc::now(),
            level: if hazards.is_empty() {
                LogLevel::Info
            } else {
                LogLevel::Warn
            },
            source: format!("carbon-audit::{strategy_id}"),
            message: format!(
                "Semantic audit complete: {} hazards, composite risk={composite_risk_score:.1}",
                hazards.len()
            ),
        },
        LiveEvent::AudioBriefingReady { audio_url, .. } => TelemetryLogEntry {
            timestamp: Utc::now(),
            level: LogLevel::Info,
            source: "flash-tts".to_string(),
            message: format!("2-speaker war-room audio briefing ready ({audio_url})"),
        },
        LiveEvent::TournamentCompleted {
            recommended_strategy_id,
            ..
        } => TelemetryLogEntry {
            timestamp: Utc::now(),
            level: LogLevel::Info,
            source: "orchestrator".to_string(),
            message: format!(
                "Tournament completed! Crowned winner: {}",
                recommended_strategy_id.as_deref().unwrap_or("none")
            ),
        },
        LiveEvent::Log {
            level,
            source,
            message,
            timestamp,
            ..
        } => TelemetryLogEntry {
            timestamp: *timestamp,
            level: *level,
            source: source.clone(),
            message: message.clone(),
        },
        LiveEvent::Error { message, .. } => TelemetryLogEntry {
            timestamp: Utc::now(),
            level: LogLevel::Error,
            source: "error".to_string(),
            message: message.clone(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use epicenter_common::MediaKind;

    #[test]
    fn parses_real_bug_preset_contract() {
        let raw = r#"{
            "repo_url": "https://github.com/pallets/click",
            "commit_sha": "bb695aaecdef95832d167fc1d508921745aa824e",
            "git_ref": null,
            "pr_url": "https://github.com/pallets/click/pull/3884",
            "task_kind": "bug",
            "title": "Fix show_envvar output for a sequence or an empty envvar (pallets/click#3884)",
            "problem_statement": "Broken show_envvar formatting",
            "attachment": {
                "id": "11111111-2222-3333-4444-555555555555",
                "filename": "click-3884-envvar-terminal-bug.png",
                "mime_type": "image/png",
                "media_kind": "screenshot",
                "size_bytes": 12345,
                "file_uri": null
            }
        }"#;

        let parsed: RealBugPresetResponse = serde_json::from_str(raw).expect("valid preset JSON");
        assert_eq!(parsed.repo_url, "https://github.com/pallets/click");
        assert_eq!(
            parsed.commit_sha.as_deref(),
            Some("bb695aaecdef95832d167fc1d508921745aa824e")
        );
        assert_eq!(parsed.git_ref, None);
        assert_eq!(
            parsed.pr_url.as_deref(),
            Some("https://github.com/pallets/click/pull/3884")
        );
        assert_eq!(parsed.task_kind, TaskKind::Bug);
        let att = parsed.attachment.expect("attachment present");
        assert_eq!(att.filename, "click-3884-envvar-terminal-bug.png");
        assert_eq!(att.media_kind, MediaKind::Screenshot);
        assert_eq!(att.size_bytes, 12345);
    }

    #[test]
    fn parses_attachment_keyframes_contract() {
        let raw = r#"{
            "attachment_id": "11111111-2222-3333-4444-555555555555",
            "keyframes": [
                { "index": 0, "timestamp_hint": "00:00", "data_url": "data:image/png;base64,iVBORw0KGgo=" },
                { "index": 1, "timestamp_hint": "00:03", "data_url": "data:image/png;base64,AAAA" }
            ]
        }"#;

        let parsed: AttachmentKeyframesResponse =
            serde_json::from_str(raw).expect("valid keyframes JSON");
        assert_eq!(parsed.attachment_id, "11111111-2222-3333-4444-555555555555");
        assert_eq!(parsed.keyframes.len(), 2);
        assert_eq!(parsed.keyframes[0].index, 0);
        assert_eq!(parsed.keyframes[0].timestamp_hint, "00:00");
        assert!(parsed.keyframes[0].data_url.starts_with("data:image/png;base64,"));
    }
}
