use crate::repo::{
    emit_event, emit_log, extract_json_from_text, is_ignored_path, is_source_file,
    is_test_like_path, RepoSnapshot,
};
use crate::UploadedAttachmentBytes;
use epicenter_common::{
    Gemini4Model, LiveEvent, LogLevel, MediaKind, MultimodalFinding, TaskKind, TournamentSession,
    TournamentStage,
};
use epicenter_gemini::{
    ContentPart, CreateDecisionRequest, CreateInteractionRequest, DecisionQuestion,
    DecisionResponse, GeminiClient, GenerationConfig, ResponseFormat,
};
use epicenter_graph::normalize_rel_path;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::json;
use std::collections::HashSet;
use std::time::Duration;
use tokio::sync::broadcast;
use tracing::warn;

const REMOTE_MEDIA_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_REMOTE_MEDIA_BYTES: usize = 20 * 1024 * 1024;
const DECISIONS_TRIAGE_TIMEOUT: Duration = Duration::from_secs(12);
const MAX_DECISION_CANDIDATE_FILES: usize = 60;
const DECISION_FILE_PROB_THRESHOLD: f64 = 0.45;
const MAX_MERGED_SUSPECT_FILES: usize = 10;
const LOW_UNIT_TESTABLE_THRESHOLD: f32 = 0.35;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DecisionsIntakeTriage {
    pub task_kind: Option<TaskKind>,
    pub task_kind_confidence: Option<f64>,
    pub unit_testable_probability: Option<f32>,
    pub suggested_k: Option<u8>,
    /// High-probability source files (`prob >= 0.45`), sorted descending by probability.
    pub ranked_files: Vec<(String, f64)>,
}

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

/// Formats Stage 1 multimodal visual/audio findings into a crisp, readable block for all
/// downstream stages (Red-Gate, Planner, Workers, Auditor, and War-Room Briefing).
pub fn format_multimodal_findings(findings: &[MultimodalFinding]) -> String {
    if findings.is_empty() {
        return "None".to_string();
    }
    findings
        .iter()
        .enumerate()
        .map(|(idx, f)| {
            let ts = f
                .timestamp_hint
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|t| format!(" (@ {t})"))
                .unwrap_or_default();
            let syms = if f.extracted_symbols.is_empty() {
                String::new()
            } else {
                format!(" | Symbols: {}", f.extracted_symbols.join(", "))
            };
            format!(
                "{}. [{:?}] {}{}: {}{}",
                idx + 1,
                f.media_kind,
                f.source_name,
                ts,
                f.summary.trim(),
                syms
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Stage 1: Multimodal Intake powered by `Gemini4Model::Hydrogen`.
///
/// Uploads raw attachments (and hydrates any remote `http://`/`https://` attachment URLs on
/// `session.intake.attachments`) via resumable Files API, feeds the repository tree and multimodal
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
    let total_attachments = attachments.len().max(session.intake.attachments.len());
    emit_event(
        event_tx,
        LiveEvent::StageChanged {
            session_id: session.id,
            stage: TournamentStage::Intake,
            message: format!(
                "Stage 1 — Hydrogen multimodal intake ({total_attachments} attachments, {} repo files)",
                repo.file_tree.len()
            ),
        },
    );

    let mut content_parts: Vec<ContentPart> = Vec::new();
    let mut uploaded_labels: Vec<String> = Vec::new();
    let mut handled_ids: HashSet<uuid::Uuid> = HashSet::new();
    let mut handled_names: HashSet<String> = HashSet::new();

    for att in attachments {
        handled_ids.insert(att.meta.id);
        handled_names.insert(att.meta.filename.clone());
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
                    format!(
                        "Uploaded multimodal attachment '{}' to Gemini Files API",
                        att.meta.filename
                    ),
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

    // Hydrate remote HTTP(S) attachments referenced by URL in `session.intake.attachments`
    // that were not already uploaded via raw `attachments` bytes.
    let remote_candidates: Vec<(usize, String, String, String)> = session
        .intake
        .attachments
        .iter()
        .enumerate()
        .filter_map(|(idx, meta)| {
            if handled_ids.contains(&meta.id) || handled_names.contains(&meta.filename) {
                return None;
            }
            let uri = meta.file_uri.as_deref()?.trim();
            let is_http = uri.starts_with("http://") || uri.starts_with("https://");
            let is_gemini_files = uri.contains("generativelanguage.googleapis.com/");
            if is_http && !is_gemini_files {
                Some((
                    idx,
                    uri.to_string(),
                    meta.filename.clone(),
                    meta.mime_type.clone(),
                ))
            } else {
                None
            }
        })
        .collect();

    if !remote_candidates.is_empty() {
        let http = reqwest::Client::builder()
            .timeout(REMOTE_MEDIA_TIMEOUT)
            .user_agent("epicenter-engine/0.1")
            .build();

        if let Ok(client) = http {
            for (idx, url, filename, declared_mime) in remote_candidates {
                match download_remote_media(&client, &url).await {
                    Ok((bytes, detected_mime)) => {
                        let mime = resolve_media_mime(&declared_mime, detected_mime.as_deref(), &filename, &url);
                        let byte_len = bytes.len() as u64;
                        match gemini.upload_file_resumable(&filename, &mime, bytes).await {
                            Ok(file_ref) => {
                                uploaded_labels.push(format!("{filename} ({mime})"));
                                if let Some(slot) = session.intake.attachments.get_mut(idx) {
                                    slot.file_uri = Some(file_ref.uri.clone());
                                    slot.mime_type = mime.clone();
                                    slot.size_bytes = byte_len;
                                }
                                content_parts.push(ContentPart::from_file_ref(&file_ref));
                                emit_log(
                                    event_tx,
                                    session.id,
                                    LogLevel::Info,
                                    "intake",
                                    format!(
                                        "Downloaded & uploaded remote multimodal asset '{filename}' ({byte_len} bytes, {mime})"
                                    ),
                                );
                            }
                            Err(err) => {
                                warn!(url = %url, error = %err, "failed uploading downloaded remote media");
                                emit_log(
                                    event_tx,
                                    session.id,
                                    LogLevel::Warn,
                                    "intake",
                                    format!("Remote attachment '{filename}' Files API upload failed ({err})"),
                                );
                            }
                        }
                    }
                    Err(err) => {
                        warn!(url = %url, error = %err, "failed downloading remote media attachment");
                        emit_log(
                            event_tx,
                            session.id,
                            LogLevel::Warn,
                            "intake",
                            format!("Remote attachment '{filename}' download skipped ({err})"),
                        );
                    }
                }
            }
        }
    }

    let repo_tree_listing = repo.prompt_tree_listing();
    let git_ref_label = session
        .intake
        .commit_sha
        .as_deref()
        .or(session.intake.git_ref.as_deref())
        .unwrap_or("default");
    let attachments_label = if uploaded_labels.is_empty() {
        "none".to_string()
    } else {
        uploaded_labels.join(", ")
    };

    let shared_context = format!(
        "Repository: {}\n\
         Git Ref / Commit: {}\n\
         Uploaded attachments: {}\n\
         Problem Statement:\n{}\n\n\
         Repository File Tree:\n{}",
        session.intake.repo_url,
        git_ref_label,
        attachments_label,
        session.intake.problem_statement,
        repo_tree_listing,
    );

    let prompt = format!(
        "You are Stage 1 (Multimodal Intake Triage) of the Epicenter engineering tournament.\n\
         Analyze the issue description, attached screen recordings/screenshots/PDFs/audio/logs, and repository file tree.\n\
         Extract:\n\
         1. A crisp engineering `title` (<= 90 chars).\n\
         2. `task_kind`: \"bug\" or \"feature\".\n\
         3. Concrete, ordered `repro_steps`.\n\
         4. `multimodal_findings`: concrete symptoms, stack traces, UI frames, timestamps, spoken details, and code symbols extracted from attachments.\n\
         5. `suspect_files`: up to 8 exact relative file paths from the repository tree most likely involved.\n\n\
         {shared_context}\n\n\
         Return STRICT JSON matching the schema."
    );

    // Put text prompt first, followed by any multimodal file references
    let mut hydrogen_parts = vec![ContentPart::text(prompt)];
    hydrogen_parts.extend(content_parts.iter().cloned());

    let mut decision_parts = vec![ContentPart::text(shared_context)];
    decision_parts.extend(content_parts);

    let mut req = CreateInteractionRequest::for_model(Gemini4Model::Hydrogen, hydrogen_parts);
    req.system_instruction = Some(
        "You are Gemini 4 Hydrogen performing fast, exact multimodal issue triage. Always return valid JSON.".to_string(),
    );
    req.response_format = Some(ResponseFormat::json_schema(intake_json_schema()));
    req.generation_config = Some(GenerationConfig {
        thinking_level: Some("low".to_string()),
        ..Default::default()
    });

    // Run Hydrogen multimodal intake and Gemini Decisions API calibrated triage concurrently
    let (hydrogen_res, decisions_res) = tokio::join!(
        gemini.create_interaction(&req),
        run_decisions_intake_triage(gemini, decision_parts, &repo.file_tree),
    );

    let mut hydrogen_set_task_kind = false;
    match hydrogen_res {
        Ok(resp) => {
            let text = resp.extract_text();
            match extract_json_from_text::<IntakeModelOutput>(&text) {
                Ok(parsed) => {
                    if let Some(title) = parsed.title.filter(|t| !t.trim().is_empty()) {
                        session.intake.title = title.trim().to_string();
                    }
                    if let Some(kind) = parsed.task_kind {
                        session.intake.task_kind = kind;
                        hydrogen_set_task_kind = true;
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

    match decisions_res {
        Ok(triage) => {
            if !hydrogen_set_task_kind {
                if let Some(kind) = triage.task_kind {
                    session.intake.task_kind = kind;
                }
            }

            if let Some(prob) = triage.unit_testable_probability {
                session.intake.unit_testable_probability = Some(prob);
                if prob < LOW_UNIT_TESTABLE_THRESHOLD {
                    emit_log(
                        event_tx,
                        session.id,
                        LogLevel::Warn,
                        "intake",
                        format!(
                            "gemini-decision-1 calibrated headless unit-test reproducibility is low ({:.0}%); issue may require a live browser, GUI, or external database",
                            prob * 100.0
                        ),
                    );
                }
            }

            if let Some(k) = triage.suggested_k {
                session.intake.suggested_k = Some(k);
            }

            session.intake.suspect_files = merge_suspect_files(
                &session.intake.suspect_files,
                &triage.ranked_files,
                MAX_MERGED_SUSPECT_FILES,
            );

            let ut_label = triage
                .unit_testable_probability
                .map(|p| format!("{:.0}%", p * 100.0))
                .unwrap_or_else(|| "n/a".to_string());
            let k_label = triage
                .suggested_k
                .map(|k| k.to_string())
                .unwrap_or_else(|| "n/a".to_string());
            let top_files_label = if triage.ranked_files.is_empty() {
                "none >= 0.45".to_string()
            } else {
                triage
                    .ranked_files
                    .iter()
                    .take(5)
                    .map(|(path, prob)| format!("{path} ({:.0}%)", prob * 100.0))
                    .collect::<Vec<_>>()
                    .join(", ")
            };

            emit_log(
                event_tx,
                session.id,
                LogLevel::Info,
                "intake",
                format!(
                    "gemini-decision-1 triage: unit_testable={ut_label}, suggested_k={k_label}, top_files=[{top_files_label}]"
                ),
            );
        }
        Err(err) => {
            warn!(error = %err, "gemini-decision-1 intake triage failed; proceeding with Hydrogen intake only");
            emit_log(
                event_tx,
                session.id,
                LogLevel::Warn,
                "intake",
                format!("gemini-decision-1 triage skipped ({err}); proceeding with Hydrogen intake"),
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

/// Runs calibrated Stage 1 triage (`task_kind`, `is_unit_testable`, `optimal_k`, and up to 60
/// source-file relevance probabilities) in a single batched `gemini-decision-1` call bounded by a
/// 12-second timeout so transient Decisions API errors never stall Stage 1.
async fn run_decisions_intake_triage(
    gemini: &GeminiClient,
    parts: Vec<ContentPart>,
    file_tree: &[String],
) -> anyhow::Result<DecisionsIntakeTriage> {
    let candidate_files =
        select_decision_candidate_files(file_tree, MAX_DECISION_CANDIDATE_FILES);
    let (req, file_keys) = build_decisions_intake_request(parts, &candidate_files);
    let resp = tokio::time::timeout(DECISIONS_TRIAGE_TIMEOUT, gemini.evaluate_decisions(&req))
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "gemini-decision-1 intake triage timed out after {}s",
                DECISIONS_TRIAGE_TIMEOUT.as_secs()
            )
        })??;
    Ok(parse_decisions_intake_triage(&resp, &file_keys))
}

/// Selects up to `max_files` non-ignored, deduplicated source files from `file_tree`, prioritizing
/// production source files ahead of test files.
pub(crate) fn select_decision_candidate_files(
    file_tree: &[String],
    max_files: usize,
) -> Vec<String> {
    if max_files == 0 {
        return Vec::new();
    }
    let mut seen = HashSet::new();
    let mut prod_files = Vec::new();
    let mut test_files = Vec::new();

    for raw in file_tree {
        let norm = normalize_rel_path(raw);
        if norm.is_empty() || is_ignored_path(&norm) || !is_source_file(&norm) {
            continue;
        }
        if !seen.insert(norm.clone()) {
            continue;
        }
        if is_test_like_path(&norm) {
            test_files.push(norm);
        } else {
            prod_files.push(norm);
        }
    }

    prod_files.extend(test_files);
    prod_files.truncate(max_files);
    prod_files
}

/// Builds the single batched `CreateDecisionRequest` for Stage 1 triage along with the ordered
/// `(question_key, file_path)` mapping (`"file_00"`..`"file_59"`).
pub(crate) fn build_decisions_intake_request(
    parts: Vec<ContentPart>,
    candidate_files: &[String],
) -> (CreateDecisionRequest, Vec<(String, String)>) {
    let mut req = CreateDecisionRequest::new(parts)
        .with_question(
            "task_kind",
            DecisionQuestion::choice(
                "Classify whether this issue is a software bug fix or a new feature / optimization.",
                [
                    (
                        "bug",
                        "Fixes broken, crashing, regressed, or incorrect existing behavior.",
                    ),
                    (
                        "feature",
                        "Adds a new capability, API, enhancement, or standalone optimization.",
                    ),
                ],
            ),
        )
        .with_question(
            "is_unit_testable",
            DecisionQuestion::noul(
                "Can this issue be reproduced and verified with headless CLI unit tests (e.g. pytest, vitest, cargo test) without launching a live browser, GUI, or external database?",
            ),
        )
        .with_question(
            "optimal_k",
            DecisionQuestion::score(
                "How many genuinely distinct architectural approaches (1 to 5) are warranted to explore in parallel for this issue?",
                [
                    "1 strategy: single obvious localized fix with essentially no architectural degrees of freedom.",
                    "2 strategies: two clear tradeoffs (e.g. minimal hotfix vs. cleaner targeted refactor).",
                    "3 strategies: three orthogonal approaches spanning surgical hotfix, algorithmic/targeted optimization, and architectural root-cause cure.",
                    "4 strategies: complex multi-subsystem issue with four meaningfully distinct designs worth comparing.",
                    "5 strategies: high-uncertainty architectural problem with five genuinely distinct designs across safety, throughput, and structure.",
                ],
            ),
        );

    let mut file_keys = Vec::new();
    for (idx, path) in candidate_files
        .iter()
        .take(MAX_DECISION_CANDIDATE_FILES)
        .enumerate()
    {
        let key = format!("file_{idx:02}");
        req.insert_question(
            key.clone(),
            DecisionQuestion::noul(format!(
                "Is inspecting or editing `{path}` likely needed to diagnose or fix this issue?"
            )),
        );
        file_keys.push((key, path.clone()));
    }

    (req, file_keys)
}

/// Parses calibrated Stage 1 triage signals and high-probability (`>= 0.45`) ranked files from a
/// `gemini-decision-1` response.
pub(crate) fn parse_decisions_intake_triage(
    resp: &DecisionResponse,
    file_keys: &[(String, String)],
) -> DecisionsIntakeTriage {
    let (task_kind, task_kind_confidence) = match resp.choice_with_confidence("task_kind") {
        Some(("bug", conf)) => (Some(TaskKind::Bug), Some(conf)),
        Some(("feature", conf)) => (Some(TaskKind::Feature), Some(conf)),
        _ => (None, None),
    };

    let unit_testable_probability = resp
        .noul_prob("is_unit_testable")
        .filter(|p| p.is_finite())
        .map(|p| p.clamp(0.0, 1.0) as f32);

    let suggested_k = resp
        .score("optimal_k")
        .filter(|s| s.is_finite())
        .map(|s| (s.clamp(0.0, 4.0).round() as u8 + 1).clamp(1, 5));

    let mut ranked_files: Vec<(String, f64)> = file_keys
        .iter()
        .filter_map(|(key, path)| {
            let prob = resp.noul_prob(key)?;
            if prob.is_finite() && prob >= DECISION_FILE_PROB_THRESHOLD {
                Some((path.clone(), prob))
            } else {
                None
            }
        })
        .collect();
    ranked_files.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    DecisionsIntakeTriage {
        task_kind,
        task_kind_confidence,
        unit_testable_probability,
        suggested_k,
        ranked_files,
    }
}

/// Merges Hydrogen's `suspect_files` with high-probability (`>= 0.45`) files ranked by
/// `gemini-decision-1`, sorting Decisions additions descending by probability, deduplicating on
/// normalized relative paths, and capping the combined list at `max_files` (`<= 10`).
pub(crate) fn merge_suspect_files(
    hydrogen_files: &[String],
    ranked_decision_files: &[(String, f64)],
    max_files: usize,
) -> Vec<String> {
    if max_files == 0 {
        return Vec::new();
    }

    let mut seen = HashSet::new();
    let mut merged = Vec::new();

    for raw in hydrogen_files {
        let norm = normalize_rel_path(raw);
        if norm.is_empty() {
            continue;
        }
        if seen.insert(norm.clone()) {
            merged.push(norm);
            if merged.len() >= max_files {
                return merged;
            }
        }
    }

    let mut eligible_decisions: Vec<(String, f64)> = ranked_decision_files
        .iter()
        .filter(|(_, prob)| prob.is_finite() && *prob >= DECISION_FILE_PROB_THRESHOLD)
        .filter_map(|(path, prob)| {
            let norm = normalize_rel_path(path);
            if norm.is_empty() {
                None
            } else {
                Some((norm, *prob))
            }
        })
        .collect();
    eligible_decisions.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    for (path, _) in eligible_decisions {
        if seen.insert(path.clone()) {
            merged.push(path);
            if merged.len() >= max_files {
                break;
            }
        }
    }

    merged
}

async fn download_remote_media(
    client: &reqwest::Client,
    url: &str,
) -> anyhow::Result<(Vec<u8>, Option<String>)> {
    let resp = client.get(url).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("HTTP {}", resp.status().as_u16());
    }
    if let Some(len) = resp.content_length() {
        if len as usize > MAX_REMOTE_MEDIA_BYTES {
            anyhow::bail!("remote asset exceeds 20MB limit ({len} bytes)");
        }
    }
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.split(';').next().unwrap_or(s).trim().to_string())
        .filter(|s| !s.is_empty());

    let mut bytes = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk_res) = stream.next().await {
        let chunk = chunk_res?;
        if bytes.len() + chunk.len() > MAX_REMOTE_MEDIA_BYTES {
            anyhow::bail!("remote asset exceeded 20MB cap while streaming");
        }
        bytes.extend_from_slice(&chunk);
    }
    if bytes.is_empty() {
        anyhow::bail!("remote asset was empty");
    }
    Ok((bytes, content_type))
}

fn resolve_media_mime(
    declared: &str,
    http_header: Option<&str>,
    filename: &str,
    url: &str,
) -> String {
    let decl = declared.trim();
    if !decl.is_empty() && decl != "application/octet-stream" {
        return decl.to_string();
    }
    if let Some(ct) = http_header {
        if !ct.is_empty() && ct != "application/octet-stream" && !ct.starts_with("text/html") {
            return ct.to_string();
        }
    }
    let probe = format!("{} {}", filename.to_ascii_lowercase(), url.to_ascii_lowercase());
    if probe.contains(".png") {
        "image/png".to_string()
    } else if probe.contains(".jpg") || probe.contains(".jpeg") {
        "image/jpeg".to_string()
    } else if probe.contains(".webp") {
        "image/webp".to_string()
    } else if probe.contains(".gif") {
        "image/gif".to_string()
    } else if probe.contains(".mp4") {
        "video/mp4".to_string()
    } else if probe.contains(".webm") {
        "video/webm".to_string()
    } else if probe.contains(".mov") {
        "video/quicktime".to_string()
    } else if probe.contains(".wav") {
        "audio/wav".to_string()
    } else if probe.contains(".mp3") {
        "audio/mpeg".to_string()
    } else if probe.contains(".pdf") {
        "application/pdf".to_string()
    } else {
        "application/octet-stream".to_string()
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use epicenter_gemini::{DecisionAnswer, FileRef};
    use std::collections::BTreeMap;

    #[test]
    fn select_decision_candidate_files_filters_prioritizes_and_caps_at_60() {
        let mut tree = vec![
            "README.md".to_string(),
            "Cargo.lock".to_string(),
            "node_modules/pkg/index.ts".to_string(),
            "target/debug/main.rs".to_string(),
            ".venv/lib/site.py".to_string(),
            "tests/test_alpha.py".to_string(),
            "./src/core.py".to_string(),
            "src/core.py".to_string(),
        ];
        for idx in 0..70 {
            tree.push(format!("src/mod_{idx:02}.py"));
        }

        let selected = select_decision_candidate_files(&tree, MAX_DECISION_CANDIDATE_FILES);
        assert_eq!(selected.len(), 60);
        // Non-ignored production files come before tests, deduplicated & normalized
        assert_eq!(selected[0], "src/core.py");
        assert_eq!(selected[1], "src/mod_00.py");
        assert!(!selected.iter().any(|p| p.contains("node_modules")));
        assert!(!selected.iter().any(|p| p.contains("target/")));
        assert!(!selected.iter().any(|p| p.ends_with(".md")));
        // Test file is pushed after the 71 production files, so capped out when >60 prod files exist
        assert!(!selected.iter().any(|p| p == "tests/test_alpha.py"));

        // When fewer than 60 prod files exist, test source files fill remaining slots
        let small_tree = vec![
            "tests/test_alpha.py".to_string(),
            "app/pricing.py".to_string(),
            "docs/guide.md".to_string(),
        ];
        let small = select_decision_candidate_files(&small_tree, MAX_DECISION_CANDIDATE_FILES);
        assert_eq!(small, vec!["app/pricing.py", "tests/test_alpha.py"]);
    }

    #[test]
    fn build_decisions_intake_request_creates_valid_batch_and_key_mapping() {
        let file_ref = FileRef {
            uri: "https://generativelanguage.googleapis.com/v1beta/files/vid1".to_string(),
            mime_type: "video/mp4".to_string(),
            name: "files/vid1".to_string(),
        };
        let parts = vec![
            ContentPart::text("Problem statement and repo tree"),
            ContentPart::from_file_ref(&file_ref),
        ];
        let candidates: Vec<String> = (0..65).map(|i| format!("src/file_{i:02}.rs")).collect();

        let (req, file_keys) = build_decisions_intake_request(parts, &candidates);
        assert_eq!(req.model, epicenter_common::GEMINI_DECISION_MODEL);
        // 3 base questions + capped 60 file questions
        assert_eq!(file_keys.len(), 60);
        assert_eq!(req.questions.len(), 63);
        assert_eq!(file_keys[0], ("file_00".to_string(), "src/file_00.rs".to_string()));
        assert_eq!(file_keys[59], ("file_59".to_string(), "src/file_59.rs".to_string()));

        // Every question key satisfies [A-Za-z0-9_-]+ and max 64 chars
        for key in req.questions.keys() {
            assert!(!key.is_empty() && key.len() <= 64);
            assert!(
                key.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
                "invalid decision question key: {key}"
            );
        }

        // Verify question types
        match req.questions.get("task_kind") {
            Some(DecisionQuestion::Choice { criteria, .. }) => {
                assert!(criteria.contains_key("bug"));
                assert!(criteria.contains_key("feature"));
            }
            other => panic!("expected Choice for task_kind, got {other:?}"),
        }
        assert!(matches!(
            req.questions.get("is_unit_testable"),
            Some(DecisionQuestion::Noul { .. })
        ));
        match req.questions.get("optimal_k") {
            Some(DecisionQuestion::Score { criteria, .. }) => {
                assert_eq!(criteria.len(), 5);
            }
            other => panic!("expected 5-level Score for optimal_k, got {other:?}"),
        }
        match req.questions.get("file_00") {
            Some(DecisionQuestion::Noul { instructions }) => {
                assert!(instructions.contains("`src/file_00.rs`"));
            }
            other => panic!("expected Noul for file_00, got {other:?}"),
        }
    }

    #[test]
    fn parse_decisions_intake_triage_maps_scores_and_filters_file_probs() {
        let mut answers = BTreeMap::new();
        answers.insert(
            "task_kind".to_string(),
            DecisionAnswer::Choice {
                choice: "feature".to_string(),
                probabilities: BTreeMap::new(),
                confidence: 0.91,
            },
        );
        answers.insert(
            "is_unit_testable".to_string(),
            DecisionAnswer::Noul {
                noul: 0.28,
                confidence: 0.72,
            },
        );
        answers.insert(
            "optimal_k".to_string(),
            DecisionAnswer::Score {
                score: 2.6,
                legend: BTreeMap::new(),
                probabilities: BTreeMap::new(),
                confidence: 0.85,
            },
        );
        answers.insert(
            "file_00".to_string(),
            DecisionAnswer::Noul {
                noul: 0.44,
                confidence: 0.56,
            },
        );
        answers.insert(
            "file_01".to_string(),
            DecisionAnswer::Noul {
                noul: 0.62,
                confidence: 0.62,
            },
        );
        answers.insert(
            "file_02".to_string(),
            DecisionAnswer::Noul {
                noul: 0.93,
                confidence: 0.93,
            },
        );
        answers.insert(
            "file_03".to_string(),
            DecisionAnswer::Noul {
                noul: 0.45,
                confidence: 0.55,
            },
        );

        let resp = DecisionResponse {
            answers,
            usage: None,
        };
        let file_keys = vec![
            ("file_00".to_string(), "src/low.rs".to_string()),
            ("file_01".to_string(), "src/mid.rs".to_string()),
            ("file_02".to_string(), "src/high.rs".to_string()),
            ("file_03".to_string(), "src/edge.rs".to_string()),
        ];

        let triage = parse_decisions_intake_triage(&resp, &file_keys);
        assert_eq!(triage.task_kind, Some(TaskKind::Feature));
        assert_eq!(triage.task_kind_confidence, Some(0.91));
        assert!((triage.unit_testable_probability.unwrap() - 0.28).abs() < 1e-6);
        // 2.6 rounds to 3 -> 3 + 1 = 4
        assert_eq!(triage.suggested_k, Some(4));
        assert_eq!(
            triage.ranked_files,
            vec![
                ("src/high.rs".to_string(), 0.93),
                ("src/mid.rs".to_string(), 0.62),
                ("src/edge.rs".to_string(), 0.45),
            ]
        );

        // Verify boundary clamping of optimal_k (0.0 -> 1, 4.0 -> 5)
        let mut edge_answers = BTreeMap::new();
        edge_answers.insert(
            "optimal_k".to_string(),
            DecisionAnswer::Score {
                score: 0.0,
                legend: BTreeMap::new(),
                probabilities: BTreeMap::new(),
                confidence: 0.9,
            },
        );
        let low_k = parse_decisions_intake_triage(
            &DecisionResponse {
                answers: edge_answers,
                usage: None,
            },
            &[],
        );
        assert_eq!(low_k.suggested_k, Some(1));

        let mut high_answers = BTreeMap::new();
        high_answers.insert(
            "optimal_k".to_string(),
            DecisionAnswer::Score {
                score: 4.0,
                legend: BTreeMap::new(),
                probabilities: BTreeMap::new(),
                confidence: 0.9,
            },
        );
        let high_k = parse_decisions_intake_triage(
            &DecisionResponse {
                answers: high_answers,
                usage: None,
            },
            &[],
        );
        assert_eq!(high_k.suggested_k, Some(5));
    }

    #[test]
    fn merge_suspect_files_dedupes_sorts_decisions_and_caps_at_10() {
        let hydrogen = vec![
            "./app/pricing.py".to_string(),
            "app/billing.py".to_string(),
            "app/pricing.py".to_string(),
        ];
        let decisions = vec![
            ("app/below_threshold.py".to_string(), 0.44),
            ("app/nested/tax.py".to_string(), 0.71),
            ("app/pricing.py".to_string(), 0.96),
            ("app/nested/cache.py".to_string(), 0.89),
            ("app/routes.py".to_string(), 0.55),
            ("app/extra_1.py".to_string(), 0.54),
            ("app/extra_2.py".to_string(), 0.53),
            ("app/extra_3.py".to_string(), 0.52),
            ("app/extra_4.py".to_string(), 0.51),
            ("app/extra_5.py".to_string(), 0.50),
            ("app/extra_6.py".to_string(), 0.49),
            ("app/extra_7.py".to_string(), 0.48),
        ];

        let merged = merge_suspect_files(&hydrogen, &decisions, MAX_MERGED_SUSPECT_FILES);
        assert_eq!(merged.len(), 10);
        // Hydrogen entries first (normalized & deduped), followed by Decisions descending by prob
        assert_eq!(merged[0], "app/pricing.py");
        assert_eq!(merged[1], "app/billing.py");
        assert_eq!(merged[2], "app/nested/cache.py");
        assert_eq!(merged[3], "app/nested/tax.py");
        assert_eq!(merged[4], "app/routes.py");
        assert!(!merged.iter().any(|p| p == "app/below_threshold.py"));
        assert!(!merged.iter().any(|p| p == "app/extra_7.py"));
    }
}
