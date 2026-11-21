use crate::grader::{grade_submission, CandidateSubmission, GraderConfig, GradingMode};
use crate::verify::{ensure_verify_passed, verify_cases_static, VerifyReport};
use anyhow::{bail, Context, Result};
use chrono::Utc;
use clap::ValueEnum;
use epicenter_common::{
    ArmOutcome, AttachmentMeta, CalibrationFinding, CandidateResult, CreateSessionRequest, EvalArm,
    EvalIssueCase, EvalScorecard, EvalSplit, FiveSigmaEval, MediaKind, TaskKind,
};
use epicenter_engine::{EngineRunOptions, TournamentEngine, UploadedAttachmentBytes};
use epicenter_gemini::GeminiClient;
use futures::stream::{self, StreamExt};
use reqwest::header::CONTENT_TYPE;
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::path::Path;
use std::time::{Duration, Instant};
use tracing::{info, warn};
use uuid::Uuid;

const MEDIA_DOWNLOAD_TIMEOUT_SECS: u64 = 15;
const MAX_MEDIA_BYTES: usize = 15 * 1024 * 1024; // 15 MiB

/// CLI split selector for `epicenter-eval run --split ...`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SplitFilter {
    All,
    Calibration,
    Unseen,
    LiveJudge,
}

impl SplitFilter {
    pub fn matches(self, split: EvalSplit) -> bool {
        match self {
            Self::All => true,
            Self::Calibration => split == EvalSplit::Calibration,
            Self::Unseen => split == EvalSplit::LockedUnseen,
            Self::LiveJudge => split == EvalSplit::LiveJudge,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Calibration => "calibration",
            Self::Unseen => "unseen",
            Self::LiveJudge => "live_judge",
        }
    }
}

/// Runtime options for executing a live 3-way 5-Sigma evaluation suite.
#[derive(Debug, Clone)]
pub struct RunConfig {
    pub split: SplitFilter,
    pub concurrency: usize,
    pub dry_run: bool,
    pub grading_mode: GradingMode,
    pub label: Option<String>,
    pub github_token: Option<String>,
    pub calibration_findings: Vec<CalibrationFinding>,
}

impl Default for RunConfig {
    fn default() -> Self {
        Self {
            split: SplitFilter::All,
            concurrency: 3,
            dry_run: false,
            grading_mode: GradingMode::HostSandbox,
            label: None,
            github_token: std::env::var("GITHUB_TOKEN")
                .ok()
                .filter(|s| !s.trim().is_empty()),
            calibration_findings: Vec::new(),
        }
    }
}

/// Validated execution plan produced by `--dry-run`. Never generates synthetic outcomes or
/// overwrites report artifacts.
#[derive(Debug, Clone)]
pub struct DryRunPlan {
    pub split: SplitFilter,
    pub grading_mode: GradingMode,
    pub concurrency: usize,
    pub selected_cases: Vec<EvalIssueCase>,
    pub verification: VerifyReport,
    pub gemini_api_key_set: bool,
    pub github_token_set: bool,
}

impl DryRunPlan {
    pub fn render_summary(&self) -> String {
        let mut out = String::new();
        let cal = self
            .selected_cases
            .iter()
            .filter(|c| c.split == EvalSplit::Calibration)
            .count();
        let unseen = self
            .selected_cases
            .iter()
            .filter(|c| c.split == EvalSplit::LockedUnseen)
            .count();
        let judge = self
            .selected_cases
            .iter()
            .filter(|c| c.split == EvalSplit::LiveJudge)
            .count();
        let mm = self
            .selected_cases
            .iter()
            .filter(|c| c.has_multimodal_media)
            .count();

        let _ = writeln!(out, "=== Epicenter 5-Sigma Dry-Run Validation & Live Run Matrix ===");
        let _ = writeln!(
            out,
            "Dataset Integrity: PASS ({}/{} cases verified statically)",
            self.verification.passed_cases, self.verification.total_cases
        );
        let _ = writeln!(
            out,
            "Planned Live Matrix: {} cases (cal={}, unseen={}, live_judge={}, multimodal={}) x {} arms = {} executions",
            self.selected_cases.len(),
            cal,
            unseen,
            judge,
            mm,
            EvalArm::all().len(),
            self.selected_cases.len() * EvalArm::all().len()
        );
        let _ = writeln!(
            out,
            "Grading Mode: {:?} | Concurrency: {} | GEMINI_API_KEY set: {} | GITHUB_TOKEN set: {}",
            self.grading_mode,
            self.concurrency.max(1),
            self.gemini_api_key_set,
            self.github_token_set
        );
        let _ = writeln!(out, "Arms: {:?}", EvalArm::all());
        let _ = writeln!(
            out,
            "Note: --dry-run ONLY validates dataset integrity and prints the plan; no synthetic outcomes or report files were written.\n"
        );

        let _ = writeln!(
            out,
            "{:<28} {:<12} {:<8} {:<10} {:<6} Repo",
            "Case ID", "Split", "PR #", "Base SHA", "Media"
        );
        let _ = writeln!(out, "{}", "-".repeat(92));
        for c in &self.selected_cases {
            let split_str = match c.split {
                EvalSplit::Calibration => "calibration",
                EvalSplit::LockedUnseen => "unseen",
                EvalSplit::LiveJudge => "live_judge",
            };
            let pr_str = c
                .pr_number
                .map(|n| format!("#{n}"))
                .unwrap_or_else(|| "—".to_string());
            let short_sha = &c.parent_commit[..c.parent_commit.len().min(8)];
            let _ = writeln!(
                out,
                "{:<28} {:<12} {:<8} {:<10} {:<6} {}",
                c.id,
                split_str,
                pr_str,
                short_sha,
                c.media_urls.len(),
                c.repo_url
            );
        }
        out
    }
}

/// Loads genuinely observed calibration findings from an optional JSON file path.
pub fn load_calibration_findings(path: Option<&Path>) -> Result<Vec<CalibrationFinding>> {
    let Some(p) = path else {
        return Ok(Vec::new());
    };
    let raw = std::fs::read_to_string(p)
        .with_context(|| format!("failed to read calibration findings file {}", p.display()))?;
    let findings: Vec<CalibrationFinding> = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse calibration findings JSON {}", p.display()))?;
    Ok(findings)
}

/// Validates dataset integrity and computes the planned live evaluation matrix for `--dry-run`.
/// Never fabricates diffs, outcomes, or artifacts.
pub fn validate_dry_run_plan(
    all_cases: &[EvalIssueCase],
    config: &RunConfig,
) -> Result<DryRunPlan> {
    let selected_cases: Vec<EvalIssueCase> = all_cases
        .iter()
        .filter(|c| config.split.matches(c.split))
        .cloned()
        .collect();

    if selected_cases.is_empty() {
        bail!(
            "no evaluation cases matched split '{}'",
            config.split.as_str()
        );
    }

    let verification = verify_cases_static(&selected_cases);
    ensure_verify_passed(&verification)?;

    Ok(DryRunPlan {
        split: config.split,
        grading_mode: config.grading_mode,
        concurrency: config.concurrency.max(1),
        selected_cases,
        verification,
        gemini_api_key_set: GeminiClient::from_env().is_ok(),
        github_token_set: config.github_token.is_some(),
    })
}

/// Constructs a ground-truth-blind problem statement for all 3 evaluation arms.
/// Strictly excludes `case.ground_truth_test_patch` so no arm can see hidden verification tests.
pub fn build_blind_problem_statement(case: &EvalIssueCase) -> String {
    let mut prompt = format!(
        "Repository: {}\nIssue #{}: {}\nBase Commit: {}\n\nProblem Description:\n{}\n",
        case.repo_url, case.issue_number, case.title, case.parent_commit, case.issue_body
    );
    if case.has_multimodal_media && !case.media_urls.is_empty() {
        prompt.push_str("\nAttached Multimodal Evidence URLs:\n");
        for url in &case.media_urls {
            prompt.push_str(&format!("- {url}\n"));
        }
    }
    prompt
}

/// Runs all 3 live `EvalArm`s against the selected split with bounded concurrency and grades every
/// submission independently against the hidden `ground_truth_test_patch`.
///
/// Refuses to run in synthetic/offline mode: requires live `GEMINI_API_KEY` and `config.dry_run == false`.
pub async fn run_evaluation(
    all_cases: &[EvalIssueCase],
    config: &RunConfig,
) -> Result<FiveSigmaEval> {
    if config.dry_run {
        bail!(
            "dry_run mode only validates dataset integrity via validate_dry_run_plan() and never produces synthetic FiveSigmaEval outcomes"
        );
    }

    let selected_cases: Vec<EvalIssueCase> = all_cases
        .iter()
        .filter(|c| config.split.matches(c.split))
        .cloned()
        .collect();

    if selected_cases.is_empty() {
        bail!(
            "no evaluation cases matched split '{}'",
            config.split.as_str()
        );
    }

    let verification = verify_cases_static(&selected_cases);
    ensure_verify_passed(&verification)?;

    let engine = TournamentEngine::from_env();
    let gemini = GeminiClient::from_env().ok();
    if !engine.has_live_gemini() || gemini.is_none() {
        bail!(
            "GEMINI_API_KEY is required for live evaluation runs (synthetic/offline fallback has been permanently removed)"
        );
    }

    let grader_cfg = GraderConfig {
        mode: config.grading_mode,
        ..GraderConfig::default()
    };

    let concurrency = config.concurrency.max(1);
    info!(
        cases = selected_cases.len(),
        split = config.split.as_str(),
        concurrency,
        grading_mode = ?grader_cfg.mode,
        "starting live 3-way 5-Sigma evaluation run"
    );

    let indexed_results: Vec<(usize, Vec<ArmOutcome>)> = stream::iter(
        selected_cases
            .iter()
            .cloned()
            .enumerate()
            .collect::<Vec<_>>(),
    )
    .map(|(case_idx, case)| {
        let engine = engine.clone();
        let gemini = gemini.clone();
        let grader_cfg = grader_cfg.clone();
        let github_token = config.github_token.clone();

        async move {
            let mut case_outcomes = Vec::with_capacity(3);
            for &arm in EvalArm::all() {
                let submission =
                    execute_live_arm_for_case(&engine, &case, arm, github_token.clone()).await;
                let outcome =
                    grade_submission(&case, &submission, gemini.as_ref(), &grader_cfg).await;
                case_outcomes.push(outcome);
            }
            (case_idx, case_outcomes)
        }
    })
    .buffer_unordered(concurrency)
    .collect()
    .await;

    let mut ordered = indexed_results;
    ordered.sort_by_key(|(idx, _)| *idx);
    let outcomes: Vec<ArmOutcome> = ordered
        .into_iter()
        .flat_map(|(_, outs)| outs)
        .collect();

    let scorecard = EvalScorecard::from_outcomes(&selected_cases, &outcomes);
    let label = config
        .label
        .clone()
        .unwrap_or_else(|| format!("live-five-sigma-{}", config.split.as_str()));

    let calibration_findings = config.calibration_findings.clone();
    let remaining_failure_modes = summarize_remaining_failure_modes(&outcomes);

    Ok(FiveSigmaEval {
        run_id: Uuid::new_v4(),
        label,
        cases: selected_cases,
        outcomes,
        scorecard,
        calibration_findings,
        remaining_failure_modes,
        created_at: Utc::now(),
    })
}

async fn execute_live_arm_for_case(
    engine: &TournamentEngine,
    case: &EvalIssueCase,
    arm: EvalArm,
    github_token: Option<String>,
) -> CandidateSubmission {
    let started = Instant::now();
    let blind_prompt = build_blind_problem_statement(case);

    match arm {
        EvalArm::SingleModelCall => {
            let repo_files = BTreeMap::new();
            match engine
                .run_single_model_baseline(
                    &case.repo_url,
                    Some(&case.parent_commit),
                    &blind_prompt,
                    &repo_files,
                )
                .await
            {
                Ok(diff) => CandidateSubmission {
                    case_id: case.id.clone(),
                    arm,
                    git_diff: diff,
                    duration_ms: started.elapsed().as_millis() as u64,
                    reported_speedup_pct: None,
                    reported_blast_radius: None,
                    notes: Some("Live Arm 1 single Gemini 4 Neon model call".to_string()),
                },
                Err(err) => {
                    warn!(case_id = %case.id, error = %err, "Arm 1 live call failed");
                    CandidateSubmission {
                        case_id: case.id.clone(),
                        arm,
                        git_diff: String::new(),
                        duration_ms: started.elapsed().as_millis() as u64,
                        reported_speedup_pct: None,
                        reported_blast_radius: None,
                        notes: Some(format!("Arm 1 error: {err}")),
                    }
                }
            }
        }
        EvalArm::SingleManagedAgent => {
            match engine
                .run_single_managed_agent_baseline(
                    &case.repo_url,
                    Some(&case.parent_commit),
                    &blind_prompt,
                )
                .await
            {
                Ok(diff) => CandidateSubmission {
                    case_id: case.id.clone(),
                    arm,
                    git_diff: diff,
                    duration_ms: started.elapsed().as_millis() as u64,
                    reported_speedup_pct: None,
                    reported_blast_radius: None,
                    notes: Some("Live Arm 2 single Managed Agent sandbox run".to_string()),
                },
                Err(err) => {
                    warn!(case_id = %case.id, error = %err, "Arm 2 live call failed");
                    CandidateSubmission {
                        case_id: case.id.clone(),
                        arm,
                        git_diff: String::new(),
                        duration_ms: started.elapsed().as_millis() as u64,
                        reported_speedup_pct: None,
                        reported_blast_radius: None,
                        notes: Some(format!("Arm 2 error: {err}")),
                    }
                }
            }
        }
        EvalArm::EpicenterTournament => {
            let (attachment_metas, uploaded_attachments) =
                download_case_media_attachments(&case.media_urls).await;
            info!(
                case_id = %case.id,
                declared_media = case.media_urls.len(),
                downloaded_attachments = uploaded_attachments.len(),
                "downloaded real multimodal media bytes for Arm 3 tournament intake"
            );

            let req = CreateSessionRequest {
                repo_url: case.repo_url.clone(),
                commit_sha: Some(case.parent_commit.clone()),
                git_ref: None,
                task_kind: Some(TaskKind::Bug),
                title: Some(case.title.clone()),
                problem_statement: blind_prompt,
                attachment_ids: Vec::new(),
                attachments: attachment_metas,
            };
            let session = TournamentEngine::initialize_session(&req);
            let opts = EngineRunOptions {
                synthesize_tts: false,
                github_token,
            };

            match engine
                .execute_session(session, uploaded_attachments, opts, None)
                .await
            {
                Ok(output) => {
                    let chosen = select_winning_candidate(
                        &output.session.candidates,
                        output
                            .session
                            .arena_summary
                            .as_ref()
                            .and_then(|s| s.recommended_strategy_id.as_deref()),
                    );
                    match chosen {
                        Some(cand) => CandidateSubmission {
                            case_id: case.id.clone(),
                            arm,
                            git_diff: cand.git_diff.clone(),
                            duration_ms: started.elapsed().as_millis() as u64,
                            reported_speedup_pct: cand
                                .arena_metrics
                                .as_ref()
                                .map(|m| m.speedup_pct),
                            reported_blast_radius: cand.blast_radius.clone(),
                            notes: Some(format!(
                                "Live Arm 3 Epicenter Tournament winner strategy '{}'",
                                cand.strategy_id
                            )),
                        },
                        None => CandidateSubmission {
                            case_id: case.id.clone(),
                            arm,
                            git_diff: String::new(),
                            duration_ms: started.elapsed().as_millis() as u64,
                            reported_speedup_pct: None,
                            reported_blast_radius: None,
                            notes: Some(
                                "Live Arm 3 tournament finished without a non-empty candidate diff"
                                    .to_string(),
                            ),
                        },
                    }
                }
                Err(err) => {
                    warn!(case_id = %case.id, error = %err, "Arm 3 live tournament failed");
                    CandidateSubmission {
                        case_id: case.id.clone(),
                        arm,
                        git_diff: String::new(),
                        duration_ms: started.elapsed().as_millis() as u64,
                        reported_speedup_pct: None,
                        reported_blast_radius: None,
                        notes: Some(format!("Arm 3 tournament error: {err}")),
                    }
                }
            }
        }
    }
}

/// Downloads reachable `case.media_urls` over HTTP (15s timeout, max 15 MiB per file) into real
/// `UploadedAttachmentBytes` payloads for Stage 1 multimodal intake.
pub async fn download_case_media_attachments(
    media_urls: &[String],
) -> (Vec<AttachmentMeta>, Vec<UploadedAttachmentBytes>) {
    if media_urls.is_empty() {
        return (Vec::new(), Vec::new());
    }

    let Ok(client) = reqwest::Client::builder()
        .user_agent("epicenter-eval/0.1")
        .timeout(Duration::from_secs(MEDIA_DOWNLOAD_TIMEOUT_SECS))
        .build()
    else {
        warn!("failed to construct HTTP client for media download");
        return (Vec::new(), Vec::new());
    };

    let mut metas = Vec::new();
    let mut uploads = Vec::new();

    for (idx, url) in media_urls.iter().enumerate() {
        match fetch_single_media_bytes(&client, url).await {
            Ok((mime_type, bytes)) => {
                let media_kind = infer_media_kind(&mime_type, url);
                let ext = extension_for_mime(&mime_type);
                let meta = AttachmentMeta {
                    id: Uuid::new_v4(),
                    filename: format!("issue-media-{}.{ext}", idx + 1),
                    mime_type,
                    media_kind,
                    size_bytes: bytes.len() as u64,
                    file_uri: Some(url.clone()),
                };
                metas.push(meta.clone());
                uploads.push(UploadedAttachmentBytes { meta, bytes });
            }
            Err(err) => {
                warn!(url = %url, error = %err, "skipping unreachable or oversized case media URL");
            }
        }
    }

    (metas, uploads)
}

async fn fetch_single_media_bytes(
    client: &reqwest::Client,
    url: &str,
) -> Result<(String, Vec<u8>)> {
    let mut resp = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("GET {url} failed"))?;
    if !resp.status().is_success() {
        bail!("HTTP {} downloading {url}", resp.status());
    }
    if let Some(len) = resp.content_length() {
        if len as usize > MAX_MEDIA_BYTES {
            bail!("media payload {len} bytes exceeds 15 MiB limit");
        }
    }

    let header_mime = resp
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|raw| raw.split(';').next())
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|m| m.starts_with("image/") || m.starts_with("video/") || m == "application/pdf");

    let mut buf = Vec::new();
    while let Some(chunk) = resp.chunk().await.context("failed reading media stream")? {
        if buf.len() + chunk.len() > MAX_MEDIA_BYTES {
            bail!("media stream exceeded 15 MiB limit");
        }
        buf.extend_from_slice(&chunk);
    }
    if buf.is_empty() {
        bail!("downloaded media payload is empty");
    }

    let mime = header_mime.unwrap_or_else(|| infer_mime_type(url).to_string());
    Ok((mime, buf))
}

fn select_winning_candidate<'a>(
    candidates: &'a [CandidateResult],
    recommended_id: Option<&str>,
) -> Option<&'a CandidateResult> {
    if let Some(rec) = recommended_id {
        if let Some(found) = candidates
            .iter()
            .find(|c| c.strategy_id == rec && !c.git_diff.trim().is_empty())
        {
            return Some(found);
        }
    }
    candidates
        .iter()
        .filter(|c| c.is_green() && !c.git_diff.trim().is_empty())
        .max_by(|a, b| a.dual_utility_score().total_cmp(&b.dual_utility_score()))
        .or_else(|| candidates.iter().find(|c| !c.git_diff.trim().is_empty()))
}

fn infer_mime_type(url: &str) -> &'static str {
    let lower = url
        .split(['?', '#'])
        .next()
        .unwrap_or(url)
        .to_ascii_lowercase();
    if lower.ends_with(".mp4") {
        "video/mp4"
    } else if lower.ends_with(".mov") {
        "video/quicktime"
    } else if lower.ends_with(".webm") {
        "video/webm"
    } else if lower.ends_with(".gif") {
        "image/gif"
    } else if lower.ends_with(".webp") {
        "image/webp"
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "image/jpeg"
    } else {
        "image/png"
    }
}

fn extension_for_mime(mime: &str) -> &'static str {
    match mime {
        "video/mp4" => "mp4",
        "video/quicktime" => "mov",
        "video/webm" => "webm",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/jpeg" => "jpg",
        "application/pdf" => "pdf",
        _ => "png",
    }
}

fn infer_media_kind(mime: &str, url: &str) -> MediaKind {
    let lower_url = url.to_ascii_lowercase();
    if mime.starts_with("video/")
        || lower_url.ends_with(".mp4")
        || lower_url.ends_with(".mov")
        || lower_url.ends_with(".webm")
    {
        MediaKind::ScreenRecording
    } else {
        MediaKind::Screenshot
    }
}

/// Aggregates honest failure mode counts across all unresolved or regressing outcomes.
pub fn summarize_remaining_failure_modes(outcomes: &[ArmOutcome]) -> Vec<String> {
    let mut counts: HashMap<(EvalArm, String), usize> = HashMap::new();
    for outcome in outcomes {
        if let Some(ref cat) = outcome.failure_category {
            *counts.entry((outcome.arm, cat.clone())).or_insert(0) += 1;
        }
    }

    if counts.is_empty() {
        return vec![
            "No failures observed in this filtered split; verify against full 15-case locked unseen suite."
                .to_string(),
        ];
    }

    let mut entries: Vec<(u8, EvalArm, String, usize)> = counts
        .into_iter()
        .map(|((arm, cat), count)| {
            let arm_ord = match arm {
                EvalArm::EpicenterTournament => 0,
                EvalArm::SingleManagedAgent => 1,
                EvalArm::SingleModelCall => 2,
            };
            (arm_ord, arm, cat, count)
        })
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.2.cmp(&b.2)));

    entries
        .into_iter()
        .map(|(_, arm, cat, count)| {
            format!(
                "{} — `{}` ({} {})",
                arm.label(),
                cat,
                count,
                if count == 1 { "case" } else { "cases" }
            )
        })
        .collect()
}
