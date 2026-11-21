use crate::repo::{emit_event, emit_log, extract_json_from_text, RepoSnapshot};
use crate::UploadedAttachmentBytes;
use epicenter_common::{
    Gemini4Model, LiveEvent, LogLevel, MediaKind, MultimodalFinding, TaskKind, TournamentSession,
    TournamentStage,
};
use epicenter_gemini::{ContentPart, CreateInteractionRequest, GeminiClient, ResponseFormat};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::broadcast;
use tracing::warn;

#[derive(Debug, Deserialize)]
struct IntakeModelOutput {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    task_kind: Option<TaskKind>,
    #[serde(default)]
    repro_steps: Vec<String>,
    #[serde(default)]
    multimodal_findings: Vec<RawMultimodalFinding>,
    #[serde(default)]
    suspect_files: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RawMultimodalFinding {
    #[serde(default)]
    source_name: String,
    #[serde(default)]
    media_kind: Option<MediaKind>,
    #[serde(default)]
    timestamp_hint: Option<String>,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    extracted_symbols: Vec<String>,
}

/// Stage 1: Multimodal Intake powered by `Gemini4Model::Hydrogen`.
///
/// Uploads raw attachments via resumable Files API, feeds the repository tree and multimodal
/// parts to `gemini-4-hydrogen` with a strict JSON schema, and populates `session.intake`.
pub async fn run_intake_stage(
    gemini: &GeminiClient,
    session: &mut TournamentSession,
    attachments: &[UploadedAttachmentBytes],
    repo: &RepoSnapshot,
    event_tx: &Option<broadcast::Sender<LiveEvent>>,
) -> anyhow::Result<()> {
    session.stage = TournamentStage::Intake;
    session.updated_at = chrono::Utc::now();
    emit_event(
        event_tx,
        LiveEvent::StageChanged {
            session_id: session.id,
            stage: TournamentStage::Intake,
            message: format!(
                "Stage 1 — Hydrogen multimodal intake ({} attachments, {} repo files)",
                attachments.len(),
                repo.file_tree.len()
            ),
        },
    );

    let mut content_parts: Vec<ContentPart> = Vec::new();
    let mut uploaded_labels: Vec<String> = Vec::new();

    for att in attachments {
        if att.bytes.is_empty() {
            continue;
        }
        match gemini
            .upload_file_resumable(&att.meta.filename, &att.meta.mime_type, att.bytes.clone())
            .await
        {
            Ok(file_ref) => {
                uploaded_labels.push(format!("{} ({})", att.meta.filename, att.meta.mime_type));
                if let Some(existing) = session
                    .intake
                    .attachments
                    .iter_mut()
                    .find(|a| a.id == att.meta.id || a.filename == att.meta.filename)
                {
                    existing.file_uri = Some(file_ref.uri.clone());
                } else {
                    let mut meta = att.meta.clone();
                    meta.file_uri = Some(file_ref.uri.clone());
                    session.intake.attachments.push(meta);
                }
                content_parts.push(ContentPart::from_file_ref(&file_ref));
                emit_log(
                    event_tx,
                    session.id,
                    LogLevel::Info,
                    "intake",
                    format!("Uploaded multimodal attachment '{}' to Gemini Files API", att.meta.filename),
                );
            }
            Err(err) => {
                warn!(
                    filename = %att.meta.filename,
                    error = %err,
                    "failed to upload attachment; continuing intake with remaining evidence"
                );
                emit_log(
                    event_tx,
                    session.id,
                    LogLevel::Warn,
                    "intake",
                    format!(
                        "Attachment '{}' upload failed ({}); proceeding with text & remaining evidence",
                        att.meta.filename, err
                    ),
                );
            }
        }
    }

    let prompt = format!(
        "You are Stage 1 (Multimodal Intake Triage) of the Epicenter engineering tournament.\n\
         Analyze the issue description, attached screen recordings/screenshots/PDFs/logs, and repository file tree.\n\
         Extract:\n\
         1. A crisp engineering `title` (<= 90 chars).\n\
         2. `task_kind`: \"bug\" or \"feature\".\n\
         3. Concrete, ordered `repro_steps`.\n\
         4. `multimodal_findings`: concrete symptoms, stack traces, UI frames, timestamps, and code symbols extracted from attachments.\n\
         5. `suspect_files`: up to 8 exact relative file paths from the repository tree most likely involved.\n\n\
         Repository: {}\n\
         Git Ref / Commit: {}\n\
         Uploaded attachments: {}\n\
         Problem Statement:\n{}\n\n\
         Repository File Tree:\n{}\n\n\
         Return STRICT JSON matching the schema.",
        session.intake.repo_url,
        session
            .intake
            .commit_sha
            .as_deref()
            .or(session.intake.git_ref.as_deref())
            .unwrap_or("default"),
        if uploaded_labels.is_empty() {
            "none".to_string()
        } else {
            uploaded_labels.join(", ")
        },
        session.intake.problem_statement,
        repo.prompt_tree_listing(),
    );

    // Put text prompt first, followed by any multimodal file references
    let mut parts = vec![ContentPart::text(prompt)];
    parts.extend(content_parts);

    let mut req = CreateInteractionRequest::for_model(Gemini4Model::Hydrogen, parts);
    req.system_instruction = Some(
        "You are Gemini 4 Hydrogen performing fast, exact multimodal issue triage. Always return valid JSON.".to_string(),
    );
    req.response_format = Some(ResponseFormat::json_schema(intake_json_schema()));

    match gemini.create_interaction(&req).await {
        Ok(resp) => {
            let text = resp.extract_text();
            match extract_json_from_text::<IntakeModelOutput>(&text) {
                Ok(parsed) => {
                    if let Some(title) = parsed.title.filter(|t| !t.trim().is_empty()) {
                        session.intake.title = title.trim().to_string();
                    }
                    if let Some(kind) = parsed.task_kind {
                        session.intake.task_kind = kind;
                    }
                    if !parsed.repro_steps.is_empty() {
                        session.intake.repro_steps = parsed.repro_steps;
                    }
                    if !parsed.multimodal_findings.is_empty() {
                        session.intake.multimodal_findings = parsed
                            .multimodal_findings
                            .into_iter()
                            .map(|f| MultimodalFinding {
                                source_name: if f.source_name.is_empty() {
                                    "attachment".to_string()
                                } else {
                                    f.source_name
                                },
                                media_kind: f.media_kind.unwrap_or(MediaKind::Other),
                                timestamp_hint: f.timestamp_hint,
                                summary: f.summary,
                                extracted_symbols: f.extracted_symbols,
                            })
                            .collect();
                    }
                    if !parsed.suspect_files.is_empty() {
                        session.intake.suspect_files = parsed.suspect_files;
                    }
                }
                Err(err) => {
                    warn!(error = %err, "failed to parse Hydrogen intake JSON; keeping fallback brief");
                    emit_log(
                        event_tx,
                        session.id,
                        LogLevel::Warn,
                        "intake",
                        format!("Hydrogen intake JSON parse fallback: {err}"),
                    );
                }
            }
        }
        Err(err) => {
            warn!(error = %err, "Hydrogen intake call failed; continuing with raw user brief");
            emit_log(
                event_tx,
                session.id,
                LogLevel::Warn,
                "intake",
                format!("Hydrogen intake API error ({err}); continuing with user brief"),
            );
        }
    }

    if session.intake.title == "Analyzing issue..." || session.intake.title.trim().is_empty() {
        session.intake.title = session
            .intake
            .problem_statement
            .lines()
            .next()
            .unwrap_or("Engineering Tournament")
            .chars()
            .take(88)
            .collect();
    }
    if session.intake.repro_steps.is_empty() {
        session.intake.repro_steps.push(format!(
            "Reproduce issue described in problem statement on {}",
            session.intake.repo_url
        ));
    }

    session.updated_at = chrono::Utc::now();
    emit_event(
        event_tx,
        LiveEvent::IntakeReady {
            session_id: session.id,
            brief: Box::new(session.intake.clone()),
        },
    );
    Ok(())
}

fn intake_json_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "title": { "type": "string" },
            "task_kind": { "type": "string", "enum": ["bug", "feature"] },
            "repro_steps": {
                "type": "array",
                "items": { "type": "string" }
            },
            "multimodal_findings": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "source_name": { "type": "string" },
                        "media_kind": {
                            "type": "string",
                            "enum": ["screen_recording", "screenshot", "pdf", "voice_note", "log", "other"]
                        },
                        "timestamp_hint": { "type": "string" },
                        "summary": { "type": "string" },
                        "extracted_symbols": {
                            "type": "array",
                            "items": { "type": "string" }
                        }
                    },
                    "required": ["source_name", "summary"]
                }
            },
            "suspect_files": {
                "type": "array",
                "items": { "type": "string" }
            }
        },
        "required": ["title", "task_kind", "repro_steps", "suspect_files"]
    })
}
