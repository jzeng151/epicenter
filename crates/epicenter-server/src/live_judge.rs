use crate::db::infer_media_kind;
use crate::github::{
    parse_github_pr_url, parse_github_repo_url, parse_unified_diff as parse_github_diff,
    FetchedPrContext,
};
use crate::media::TempDirGuard;
use crate::state::AppState;
use anyhow::{bail, Result};
use chrono::Utc;
use epicenter_common::{
    ArmOutcome, AttachmentMeta, BlastRadiusReport, CreateSessionRequest, EvalArm, EvalIssueCase,
    EvalScorecard, EvalSplit, FiveSigmaEval, GraphNode, HazardSeverity, LiveEvent,
    LiveJudgeArmProgress, LiveJudgeJobState, LiveJudgeJobStatusResponse, LiveJudgePrRequest,
    LiveJudgePrResponse, SemanticHazard, TaskKind, TournamentStage,
};
use epicenter_engine::{EngineRunOptions, TournamentEngine, UploadedAttachmentBytes};
use epicenter_gemini::{
    CreateDecisionRequest, DecisionQuestion, DecisionResponse, GeminiClient,
};
use epicenter_graph::{
    normalize_rel_path, parse_unified_diff as parse_graph_diff, BlastAnalysisConfig, RepoCallGraph,
};
use futures::StreamExt;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::process::Command;
use tokio::sync::{broadcast, oneshot, Mutex as AsyncMutex};
use tracing::{info, warn};
use uuid::Uuid;

/// Dedicated host cache directory for Live Judge mirrors & venvs (isolated from `HostVerifier`
/// so concurrent Arm 1/2 grading and Arm 3 tournament runs never contend on `.git` locks).
pub const DEFAULT_REPO_CACHE_ROOT: &str = "/tmp/epicenter-cache/judge/repos";

const GIT_CLONE_TIMEOUT_SECS: u64 = 120;
const ENV_SETUP_TIMEOUT_SECS: u64 = 120;
const TEST_EXEC_TIMEOUT_SECS: u64 = 60;
const MEDIA_DOWNLOAD_TIMEOUT_SECS: u64 = 15;
const MAX_MEDIA_DOWNLOAD_BYTES: usize = 15 * 1024 * 1024;
const MAX_MEDIA_ATTACHMENTS: usize = 6;
const DECISIONS_PREFLIGHT_TIMEOUT_SECS: u64 = 10;
const DECISIONS_CALIBRATION_TIMEOUT_SECS: u64 = 12;
const LOW_SPEC_CLARITY_THRESHOLD: f64 = 0.30;
const HIGH_REGRESSION_HAZARD_THRESHOLD: f64 = 0.78;
const MEDIUM_REGRESSION_HAZARD_THRESHOLD: f64 = 0.58;
const BANDAID_WARNING_THRESHOLD: f64 = 0.65;

/// Calibrated pre-flight PR specification clarity signal from `gemini-decision-1`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PrSpecClaritySignal {
    pub has_actionable_spec: f64,
    pub is_self_contained: f64,
    pub confidence: Option<f64>,
}

/// Independent host verification verdict for a candidate patch evaluated against a PR's
/// hidden `ground_truth_test_patch` and module regression suite.
#[derive(Debug, Clone)]
pub struct HostVerificationResult {
    pub patch_applied: bool,
    pub ground_truth_patch_applied: bool,
    pub resolved_ground_truth: bool,
    pub regression_free: bool,
    pub test_tampering: bool,
    pub blast_radius: Option<BlastRadiusReport>,
    pub failure_category: Option<String>,
    pub diagnostic_tail: String,
}

#[derive(Debug, Clone)]
struct CmdOutput {
    exit_code: Option<i32>,
    timed_out: bool,
    stdout: String,
    stderr: String,
}

impl CmdOutput {
    fn success(&self) -> bool {
        !self.timed_out && self.exit_code == Some(0)
    }

    fn combined_tail(&self, max_chars: usize) -> String {
        let mut combined = String::new();
        if !self.stdout.trim().is_empty() {
            combined.push_str(self.stdout.trim());
        }
        if !self.stderr.trim().is_empty() {
            if !combined.is_empty() {
                combined.push('\n');
            }
            combined.push_str(self.stderr.trim());
        }
        if self.timed_out {
            if !combined.is_empty() {
                combined.push('\n');
            }
            combined.push_str("[timed out]");
        }
        truncate_tail(&combined, max_chars)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RepoLanguage {
    Python,
    TypeScriptJavaScript,
    Unknown,
}

/// Executes a live 3-way comparison (`SingleModelCall`, `SingleManagedAgent`, `EpicenterTournament`)
/// against any merged GitHub Pull Request and persists the resulting `FiveSigmaEval` in SQLite.
pub async fn run_live_judge_pr(
    state: &AppState,
    req: &LiveJudgePrRequest,
) -> Result<LiveJudgePrResponse> {
    let ctx = state.github.fetch_pr_context(&req.pr_url).await?;
    if ctx.ground_truth_test_patch.trim().is_empty() {
        bail!(
            "PR {}/{}#{} does not add or modify any test files, so there are no withheld PR tests to grade against. Paste a merged PR that includes test changes.",
            ctx.slug.owner,
            ctx.slug.repo,
            ctx.slug.number
        );
    }
    let eval = evaluate_fetched_pr(state, ctx).await?;
    state.db.upsert_eval_run(&eval)?;
    Ok(LiveJudgePrResponse { eval })
}

/// Validates `req.pr_url` immediately and spawns an asynchronous background 3-arm Live PR Benchmark
/// job with per-arm progress telemetry (`GET /api/eval/judge-pr/{job_id}`).
pub fn start_live_judge_pr_job(
    state: &AppState,
    req: &LiveJudgePrRequest,
) -> Result<LiveJudgeJobStatusResponse> {
    let pr_url = req.pr_url.trim().to_string();
    let slug = parse_github_pr_url(&pr_url)?;
    let job_id = Uuid::new_v4();

    let initial = LiveJudgeJobStatusResponse {
        job_id,
        pr_url: pr_url.clone(),
        pr_title: Some(format!("{}/{}#{}", slug.owner, slug.repo, slug.number)),
        state: LiveJudgeJobState::Running,
        elapsed_ms: 0,
        arms: vec![
            LiveJudgeArmProgress {
                arm: EvalArm::SingleModelCall,
                state: LiveJudgeJobState::Running,
                detail: "Fetching PR metadata & pre-fix commit from GitHub…".to_string(),
                elapsed_ms: 0,
            },
            LiveJudgeArmProgress {
                arm: EvalArm::SingleManagedAgent,
                state: LiveJudgeJobState::Running,
                detail: "Fetching PR metadata & pre-fix commit from GitHub…".to_string(),
                elapsed_ms: 0,
            },
            LiveJudgeArmProgress {
                arm: EvalArm::EpicenterTournament,
                state: LiveJudgeJobState::Running,
                detail: "Fetching PR metadata & pre-fix commit from GitHub…".to_string(),
                elapsed_ms: 0,
            },
        ],
        eval: None,
        error: None,
    };
    state.set_judge_job(initial.clone());

    let state_bg = state.clone();
    tokio::spawn(async move {
        let job_start = Instant::now();
        let (tick_stop_tx, mut tick_stop_rx) = oneshot::channel::<()>();

        // Smooth 1s elapsed-time ticker for UI polling while LLM / test commands execute.
        let ticker_state = state_bg.clone();
        let ticker_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            loop {
                tokio::select! {
                    _ = &mut tick_stop_rx => break,
                    _ = interval.tick() => {
                        let ms = job_start.elapsed().as_millis() as u64;
                        ticker_state.update_judge_job(job_id, |job| {
                            if job.state == LiveJudgeJobState::Running {
                                job.elapsed_ms = ms;
                                for a in &mut job.arms {
                                    if a.state == LiveJudgeJobState::Running {
                                        a.elapsed_ms = ms;
                                    }
                                }
                            }
                        });
                    }
                }
            }
        });

        let ctx = match state_bg.github.fetch_pr_context(&pr_url).await {
            Ok(c) => c,
            Err(err) => {
                let _ = tick_stop_tx.send(());
                let _ = ticker_handle.await;
                let msg = format!("Failed fetching PR from GitHub: {err}");
                let elapsed = job_start.elapsed().as_millis() as u64;
                state_bg.update_judge_job(job_id, |job| {
                    job.state = LiveJudgeJobState::Failed;
                    job.elapsed_ms = elapsed;
                    job.error = Some(msg.clone());
                    for a in &mut job.arms {
                        a.state = LiveJudgeJobState::Failed;
                        a.detail = "Stopped (GitHub fetch failed)".to_string();
                    }
                });
                return;
            }
        };

        if ctx.ground_truth_test_patch.trim().is_empty() {
            let _ = tick_stop_tx.send(());
            let _ = ticker_handle.await;
            let msg = format!(
                "PR {}/{}#{} does not add or modify any test files, so there are no withheld PR tests to grade against. Paste a merged bug-fix PR that includes test changes.",
                ctx.slug.owner, ctx.slug.repo, ctx.slug.number
            );
            let elapsed = job_start.elapsed().as_millis() as u64;
            state_bg.update_judge_job(job_id, |job| {
                job.pr_title = Some(format!(
                    "{}/{}#{} — {}",
                    ctx.slug.owner, ctx.slug.repo, ctx.slug.number, ctx.title
                ));
                job.state = LiveJudgeJobState::Failed;
                job.elapsed_ms = elapsed;
                job.error = Some(msg);
                for a in &mut job.arms {
                    a.state = LiveJudgeJobState::Failed;
                    a.detail = "PR contains no test file changes to grade against".to_string();
                }
            });
            return;
        }

        state_bg.update_judge_job(job_id, |job| {
            job.pr_title = Some(format!(
                "{}/{}#{} — {}",
                ctx.slug.owner, ctx.slug.repo, ctx.slug.number, ctx.title
            ));
        });

        let eval_res = evaluate_fetched_pr_tracked(&state_bg, ctx, Some(job_id)).await;
        let _ = tick_stop_tx.send(());
        let _ = ticker_handle.await;
        let total_elapsed = job_start.elapsed().as_millis() as u64;

        match eval_res {
            Ok(eval) => {
                if let Err(err) = state_bg.db.upsert_eval_run(&eval) {
                    warn!(%job_id, %err, "failed persisting completed live judge eval run");
                } else {
                    info!(%job_id, label = %eval.label, "live judge PR evaluation completed");
                }
                state_bg.update_judge_job(job_id, |job| {
                    job.state = LiveJudgeJobState::Completed;
                    job.elapsed_ms = total_elapsed;
                    job.eval = Some(eval);
                });
            }
            Err(err) => {
                state_bg.update_judge_job(job_id, |job| {
                    job.state = LiveJudgeJobState::Failed;
                    job.elapsed_ms = total_elapsed;
                    job.error = Some(err.to_string());
                });
            }
        }
    });

    Ok(initial)
}

fn set_arm_progress(
    state: &AppState,
    job_id: Option<Uuid>,
    arm: EvalArm,
    arm_state: LiveJudgeJobState,
    detail: impl Into<String>,
    elapsed_ms: Option<u64>,
) {
    let Some(jid) = job_id else {
        return;
    };
    let detail_str = detail.into();
    state.update_judge_job(jid, |job| {
        if let Some(entry) = job.arms.iter_mut().find(|a| a.arm == arm) {
            entry.state = arm_state;
            entry.detail = detail_str;
            if let Some(ms) = elapsed_ms {
                entry.elapsed_ms = ms;
            }
        }
    });
}

fn finish_arm_progress(state: &AppState, job_id: Option<Uuid>, outcome: &ArmOutcome) {
    let summary = if outcome.resolved_ground_truth && outcome.regression_free {
        "Passed withheld PR tests & existing suite".to_string()
    } else if outcome.resolved_ground_truth {
        "Passed PR tests (existing suite regressed)".to_string()
    } else {
        match outcome.failure_category.as_deref() {
            Some("empty_patch") | Some("empty_or_unparseable_diff") => {
                "Finished — no valid patch produced".to_string()
            }
            Some("patch_apply_conflict") => "Finished — patch failed to apply cleanly".to_string(),
            Some("test_tampering_violation") => {
                "Disqualified — modified locked PR test file".to_string()
            }
            Some("ground_truth_test_failed") => {
                "Finished — failed withheld PR verification tests".to_string()
            }
            _ => "Finished — did not pass withheld PR tests".to_string(),
        }
    };
    set_arm_progress(
        state,
        job_id,
        outcome.arm,
        LiveJudgeJobState::Completed,
        summary,
        Some(outcome.duration_ms),
    );
}

/// Evaluates an already-fetched GitHub PR context across all 3 arms CONCURRENTLY using real
/// independent host-side verification (`git apply` + hidden `ground_truth_test_patch` + real test execution).
pub async fn evaluate_fetched_pr(
    state: &AppState,
    ctx: FetchedPrContext,
) -> Result<FiveSigmaEval> {
    evaluate_fetched_pr_tracked(state, ctx, None).await
}

async fn evaluate_fetched_pr_tracked(
    state: &AppState,
    ctx: FetchedPrContext,
    job_id: Option<Uuid>,
) -> Result<FiveSigmaEval> {
    let case_id = format!(
        "{}-{}-pr-{}",
        ctx.slug.owner, ctx.slug.repo, ctx.slug.number
    );
    let problem_statement = build_problem_statement(&ctx);

    let issue_case = EvalIssueCase {
        id: case_id.clone(),
        repo_url: ctx.repo_url.clone(),
        issue_number: ctx.slug.number,
        pr_number: Some(ctx.slug.number),
        title: ctx.title.clone(),
        issue_body: ctx.body.clone(),
        parent_commit: ctx.parent_commit.clone(),
        ground_truth_test_patch: ctx.ground_truth_test_patch.clone(),
        has_multimodal_media: !ctx.media_urls.is_empty(),
        media_urls: ctx.media_urls.clone(),
        split: EvalSplit::LiveJudge,
    };

    // Fast (<250ms typical, 10s max, non-fatal) pre-flight PR spec clarity triage via gemini-decision-1.
    let spec_clarity = evaluate_pr_spec_clarity(state.engine.gemini.as_ref(), &ctx).await;

    // Run all 3 arms concurrently instead of sequentially!
    let (arm1_outcome, arm2_outcome, arm3_outcome) = tokio::join!(
        run_arm1_single_model(state, &case_id, &ctx, &problem_statement, job_id),
        run_arm2_single_agent(state, &case_id, &ctx, &problem_statement, job_id),
        run_arm3_tournament(state, &case_id, &ctx, &problem_statement, job_id),
    );

    let cases = vec![issue_case];
    let mut outcomes = vec![arm1_outcome, arm2_outcome, arm3_outcome];
    if let Some(clarity) = spec_clarity {
        apply_spec_clarity_note_to_failed_arms(&mut outcomes, &clarity);
    }
    let scorecard = EvalScorecard::from_outcomes(&cases, &outcomes);

    let mut remaining_failure_modes = Vec::new();
    for outcome in &outcomes {
        if !outcome.resolved_ground_truth || !outcome.regression_free {
            if let Some(cat) = &outcome.failure_category {
                remaining_failure_modes.push(format!("{}: {cat}", outcome.arm.label()));
            }
        }
    }

    Ok(FiveSigmaEval {
        run_id: Uuid::new_v4(),
        label: format!(
            "Live Judge: {}/{}#{}",
            ctx.slug.owner, ctx.slug.repo, ctx.slug.number
        ),
        cases,
        outcomes,
        scorecard,
        calibration_findings: Vec::new(),
        remaining_failure_modes,
        created_at: Utc::now(),
    })
}

async fn run_arm1_single_model(
    state: &AppState,
    case_id: &str,
    ctx: &FetchedPrContext,
    problem_statement: &str,
    job_id: Option<Uuid>,
) -> ArmOutcome {
    let start = Instant::now();
    set_arm_progress(
        state,
        job_id,
        EvalArm::SingleModelCall,
        LiveJudgeJobState::Running,
        "Generating single-prompt patch (Gemini 4 Neon)…",
        None,
    );
    let outcome = match state
        .engine
        .run_single_model_baseline(
            &ctx.repo_url,
            Some(&ctx.parent_commit),
            problem_statement,
            &ctx.base_source_files,
        )
        .await
    {
        Ok(diff) => {
            set_arm_progress(
                state,
                job_id,
                EvalArm::SingleModelCall,
                LiveJudgeJobState::Running,
                "Running withheld PR tests on host worktree…",
                None,
            );
            let mut verdict = verify_candidate_on_host(
                &ctx.repo_url,
                &ctx.parent_commit,
                &diff,
                &ctx.ground_truth_test_patch,
            )
            .await;
            if verdict.patch_applied {
                if let Some(ref mut blast) = verdict.blast_radius {
                    calibrate_blast_radius_with_decisions(
                        state.engine.gemini.as_ref(),
                        problem_statement,
                        &diff,
                        blast,
                    )
                    .await;
                }
            }
            let duration_ms = start.elapsed().as_millis() as u64;
            outcome_from_verdict(
                case_id,
                EvalArm::SingleModelCall,
                verdict,
                duration_ms,
                None,
                None,
                None,
            )
        }
        Err(err) => ArmOutcome {
            case_id: case_id.to_string(),
            arm: EvalArm::SingleModelCall,
            resolved_ground_truth: false,
            regression_free: false,
            speedup_pct: None,
            blast_risk_score: None,
            root_cause_depth_score: None,
            dark_zone_touches: None,
            duration_ms: start.elapsed().as_millis() as u64,
            notes: Some(format!("Arm 1 failed: {err}")),
            failure_category: Some("arm1_execution_error".to_string()),
        },
    };
    finish_arm_progress(state, job_id, &outcome);
    outcome
}

async fn run_arm2_single_agent(
    state: &AppState,
    case_id: &str,
    ctx: &FetchedPrContext,
    problem_statement: &str,
    job_id: Option<Uuid>,
) -> ArmOutcome {
    let start = Instant::now();
    set_arm_progress(
        state,
        job_id,
        EvalArm::SingleManagedAgent,
        LiveJudgeJobState::Running,
        "Running single coding agent sandbox (up to 2.5m)…",
        None,
    );
    let outcome = match state
        .engine
        .run_single_managed_agent_baseline(
            &ctx.repo_url,
            Some(&ctx.parent_commit),
            problem_statement,
        )
        .await
    {
        Ok(diff) => {
            set_arm_progress(
                state,
                job_id,
                EvalArm::SingleManagedAgent,
                LiveJudgeJobState::Running,
                "Running withheld PR tests on host worktree…",
                None,
            );
            let mut verdict = verify_candidate_on_host(
                &ctx.repo_url,
                &ctx.parent_commit,
                &diff,
                &ctx.ground_truth_test_patch,
            )
            .await;
            if verdict.patch_applied {
                if let Some(ref mut blast) = verdict.blast_radius {
                    calibrate_blast_radius_with_decisions(
                        state.engine.gemini.as_ref(),
                        problem_statement,
                        &diff,
                        blast,
                    )
                    .await;
                }
            }
            let duration_ms = start.elapsed().as_millis() as u64;
            outcome_from_verdict(
                case_id,
                EvalArm::SingleManagedAgent,
                verdict,
                duration_ms,
                None,
                None,
                None,
            )
        }
        Err(err) => ArmOutcome {
            case_id: case_id.to_string(),
            arm: EvalArm::SingleManagedAgent,
            resolved_ground_truth: false,
            regression_free: false,
            speedup_pct: None,
            blast_risk_score: None,
            root_cause_depth_score: None,
            dark_zone_touches: None,
            duration_ms: start.elapsed().as_millis() as u64,
            notes: Some(format!("Arm 2 failed: {err}")),
            failure_category: Some("arm2_execution_error".to_string()),
        },
    };
    finish_arm_progress(state, job_id, &outcome);
    outcome
}

fn friendly_tournament_stage(stage: TournamentStage) -> &'static str {
    match stage {
        TournamentStage::Intake => "Stage 1/6 — Extracting clues & setting up local repo…",
        TournamentStage::RedGate => "Stage 2/6 — Writing & locking failing reproduction test…",
        TournamentStage::Planning => "Stage 3/6 — Planning distinct implementation approaches…",
        TournamentStage::Executing => "Stage 4/6 — Building & testing fixes in parallel worktrees…",
        TournamentStage::Benchmarking => "Stage 5/6 — Benchmarking passing fixes side-by-side…",
        TournamentStage::Auditing => "Stage 6/6 — Checking caller impact & root-cause quality…",
        TournamentStage::Completed => "Comparison finished — grading winner against PR tests…",
        TournamentStage::Failed => "Comparison stopped early",
    }
}

async fn run_arm3_tournament(
    state: &AppState,
    case_id: &str,
    ctx: &FetchedPrContext,
    problem_statement: &str,
    job_id: Option<Uuid>,
) -> ArmOutcome {
    let start = Instant::now();
    set_arm_progress(
        state,
        job_id,
        EvalArm::EpicenterTournament,
        LiveJudgeJobState::Running,
        "Preparing repo & downloading issue attachments…",
        None,
    );

    // Download reachable PR media URLs (15s timeout, max 15MB) and pass them as real multimodal attachments.
    let uploaded_attachments = download_pr_media_attachments(&ctx.media_urls).await;
    let attachment_metas: Vec<AttachmentMeta> = uploaded_attachments
        .iter()
        .map(|a| a.meta.clone())
        .collect();

    let session_req = CreateSessionRequest {
        repo_url: ctx.repo_url.clone(),
        commit_sha: Some(ctx.parent_commit.clone()),
        git_ref: None,
        task_kind: Some(TaskKind::Bug),
        title: Some(ctx.title.clone()),
        problem_statement: problem_statement.to_string(),
        attachment_ids: Vec::new(),
        attachments: attachment_metas,
    };
    let initial = TournamentEngine::initialize_session(&session_req);
    let opts = EngineRunOptions {
        synthesize_tts: false,
        github_token: state.github.token().map(str::to_string),
    };

    let (ev_tx, mut ev_rx) = broadcast::channel::<LiveEvent>(128);
    let progress_handle = job_id.map(|jid| {
        let st = state.clone();
        tokio::spawn(async move {
            while let Ok(ev) = ev_rx.recv().await {
                if let LiveEvent::StageChanged { stage, .. } = ev {
                    set_arm_progress(
                        &st,
                        Some(jid),
                        EvalArm::EpicenterTournament,
                        LiveJudgeJobState::Running,
                        friendly_tournament_stage(stage),
                        None,
                    );
                }
            }
        })
    });

    let run_res = state
        .engine
        .execute_session(initial, uploaded_attachments, opts, Some(ev_tx))
        .await;

    if let Some(h) = progress_handle {
        let _ = h.await;
    }

    let outcome = match run_res {
        Ok(output) => {
            let recommended_id = output
                .session
                .arena_summary
                .as_ref()
                .and_then(|s| s.recommended_strategy_id.as_deref());

            let winner = recommended_id
                .and_then(|id| {
                    output
                        .session
                        .candidates
                        .iter()
                        .find(|c| c.strategy_id == id)
                })
                .or_else(|| output.session.candidates.iter().find(|c| c.is_green()))
                .or_else(|| {
                    output
                        .session
                        .candidates
                        .iter()
                        .find(|c| !c.git_diff.trim().is_empty())
                })
                .or_else(|| output.session.candidates.first());

            let Some(cand) = winner else {
                let empty_out = ArmOutcome {
                    case_id: case_id.to_string(),
                    arm: EvalArm::EpicenterTournament,
                    resolved_ground_truth: false,
                    regression_free: false,
                    speedup_pct: None,
                    blast_risk_score: None,
                    root_cause_depth_score: None,
                    dark_zone_touches: None,
                    duration_ms: start.elapsed().as_millis() as u64,
                    notes: Some("Tournament produced no candidates".to_string()),
                    failure_category: Some("no_candidates".to_string()),
                };
                finish_arm_progress(state, job_id, &empty_out);
                return empty_out;
            };

            set_arm_progress(
                state,
                job_id,
                EvalArm::EpicenterTournament,
                LiveJudgeJobState::Running,
                format!(
                    "Running withheld PR tests on winner '{}'…",
                    cand.strategy_id
                ),
                None,
            );

            // Independently verify the tournament winner on host against the hidden ground-truth
            // test patch and module regression suite — NEVER trust self-reported cand.is_green().
            let mut verdict = verify_candidate_on_host(
                &ctx.repo_url,
                &ctx.parent_commit,
                &cand.git_diff,
                &ctx.ground_truth_test_patch,
            )
            .await;

            let speedup_pct = cand.arena_metrics.as_ref().map(|m| m.speedup_pct);
            let semantic_root_cause_depth = cand
                .blast_radius
                .as_ref()
                .map(|b| b.root_cause_depth_score);
            if semantic_root_cause_depth.is_none() && verdict.patch_applied {
                if let Some(ref mut blast) = verdict.blast_radius {
                    calibrate_blast_radius_with_decisions(
                        state.engine.gemini.as_ref(),
                        problem_statement,
                        &cand.git_diff,
                        blast,
                    )
                    .await;
                }
            }

            let duration_ms = start.elapsed().as_millis() as u64;
            let note_prefix = format!(
                "Winner '{}' ({} candidates evaluated)",
                cand.strategy_id,
                output.session.candidates.len()
            );

            outcome_from_verdict(
                case_id,
                EvalArm::EpicenterTournament,
                verdict,
                duration_ms,
                speedup_pct,
                semantic_root_cause_depth,
                Some(note_prefix),
            )
        }
        Err(err) => ArmOutcome {
            case_id: case_id.to_string(),
            arm: EvalArm::EpicenterTournament,
            resolved_ground_truth: false,
            regression_free: false,
            speedup_pct: None,
            blast_risk_score: None,
            root_cause_depth_score: None,
            dark_zone_touches: None,
            duration_ms: start.elapsed().as_millis() as u64,
            notes: Some(format!("Arm 3 tournament failed: {err}")),
            failure_category: Some("arm3_execution_error".to_string()),
        },
    };
    finish_arm_progress(state, job_id, &outcome);
    outcome
}

fn outcome_from_verdict(
    case_id: &str,
    arm: EvalArm,
    verdict: HostVerificationResult,
    duration_ms: u64,
    speedup_pct: Option<f64>,
    preferred_root_cause_depth: Option<f32>,
    note_prefix: Option<String>,
) -> ArmOutcome {
    let blast_risk_score = verdict
        .blast_radius
        .as_ref()
        .map(|b| b.composite_risk_score);
    let root_cause_depth_score = preferred_root_cause_depth.or_else(|| {
        verdict
            .blast_radius
            .as_ref()
            .map(|b| b.root_cause_depth_score)
    });
    let dark_zone_touches = verdict.blast_radius.as_ref().map(|b| b.dark_zone_count());

    let notes = match note_prefix {
        Some(prefix) if !verdict.diagnostic_tail.is_empty() => {
            Some(format!("{prefix} | {}", verdict.diagnostic_tail))
        }
        Some(prefix) => Some(prefix),
        None if !verdict.diagnostic_tail.is_empty() => Some(verdict.diagnostic_tail),
        None => None,
    };

    ArmOutcome {
        case_id: case_id.to_string(),
        arm,
        resolved_ground_truth: verdict.resolved_ground_truth,
        regression_free: verdict.regression_free,
        speedup_pct,
        blast_risk_score,
        root_cause_depth_score,
        dark_zone_touches,
        duration_ms,
        notes,
        failure_category: verdict.failure_category,
    }
}

/// Independently verifies a candidate unified diff on the host against a hidden
/// `ground_truth_test_patch` using a cached git mirror under `/tmp/epicenter-cache/repos/<owner>__<repo>`
/// and an isolated detached worktree checked out at `parent_commit`.
pub async fn verify_candidate_on_host(
    repo_url: &str,
    parent_commit: &str,
    candidate_diff: &str,
    ground_truth_test_patch: &str,
) -> HostVerificationResult {
    verify_candidate_on_host_with_cache(
        Path::new(DEFAULT_REPO_CACHE_ROOT),
        repo_url,
        parent_commit,
        candidate_diff,
        ground_truth_test_patch,
    )
    .await
}

/// Same as `verify_candidate_on_host`, with a configurable cache root directory (ideal for tests).
pub async fn verify_candidate_on_host_with_cache(
    cache_root: &Path,
    repo_url: &str,
    parent_commit: &str,
    candidate_diff: &str,
    ground_truth_test_patch: &str,
) -> HostVerificationResult {
    // 1. Validate candidate diff is non-empty and parseable.
    let trimmed_diff = candidate_diff.trim();
    if trimmed_diff.is_empty() {
        return HostVerificationResult {
            patch_applied: false,
            ground_truth_patch_applied: false,
            resolved_ground_truth: false,
            regression_free: false,
            test_tampering: false,
            blast_radius: None,
            failure_category: Some("empty_patch".to_string()),
            diagnostic_tail: "Candidate produced an empty diff".to_string(),
        };
    }

    let candidate_files = extract_touched_paths(trimmed_diff);
    if candidate_files.is_empty() {
        return HostVerificationResult {
            patch_applied: false,
            ground_truth_patch_applied: false,
            resolved_ground_truth: false,
            regression_free: false,
            test_tampering: false,
            blast_radius: None,
            failure_category: Some("empty_or_unparseable_diff".to_string()),
            diagnostic_tail: "Candidate diff could not be parsed into file patches".to_string(),
        };
    }

    // 2. Hard anti-tampering check: reject immediately if candidate modifies ANY file in
    //    ground_truth_test_patch.
    let gt_files = extract_touched_paths(ground_truth_test_patch);
    let tampered: Vec<String> = candidate_files
        .intersection(&gt_files)
        .cloned()
        .collect();
    if !tampered.is_empty() {
        return HostVerificationResult {
            patch_applied: false,
            ground_truth_patch_applied: false,
            resolved_ground_truth: false,
            regression_free: false,
            test_tampering: true,
            blast_radius: None,
            failure_category: Some("test_tampering_violation".to_string()),
            diagnostic_tail: format!(
                "Candidate diff tampered with locked ground-truth test file(s): {}",
                tampered.join(", ")
            ),
        };
    }

    // 3. Ensure cached git mirror and create an isolated detached worktree at parent_commit.
    let (mirror_dir, slug_dir) = match ensure_cached_mirror(cache_root, repo_url, parent_commit).await {
        Ok(res) => res,
        Err((cat, msg)) => {
            return HostVerificationResult {
                patch_applied: false,
                ground_truth_patch_applied: false,
                resolved_ground_truth: false,
                regression_free: false,
                test_tampering: false,
                blast_radius: None,
                failure_category: Some(cat.to_string()),
                diagnostic_tail: msg,
            };
        }
    };

    let wt_guard = match TempDirGuard::new("judge-wt") {
        Ok(g) => g,
        Err(err) => {
            return HostVerificationResult {
                patch_applied: false,
                ground_truth_patch_applied: false,
                resolved_ground_truth: false,
                regression_free: false,
                test_tampering: false,
                blast_radius: None,
                failure_category: Some("worktree_setup_failed".to_string()),
                diagnostic_tail: format!("Failed to create worktree temp dir: {err}"),
            };
        }
    };
    let worktree_path = wt_guard.path().join("repo");

    {
        let mirror_lock = repo_mutex_for(&mirror_dir);
        let _guard = mirror_lock.lock().await;
        let add_out = run_git(
            &mirror_dir,
            &[
                "worktree",
                "add",
                "--detach",
                &worktree_path.to_string_lossy(),
                parent_commit,
            ],
            GIT_CLONE_TIMEOUT_SECS,
        )
        .await;
        if !add_out.success() {
            return HostVerificationResult {
                patch_applied: false,
                ground_truth_patch_applied: false,
                resolved_ground_truth: false,
                regression_free: false,
                test_tampering: false,
                blast_radius: None,
                failure_category: Some("parent_commit_checkout_failed".to_string()),
                diagnostic_tail: format!(
                    "git worktree add failed for {parent_commit}: {}",
                    add_out.combined_tail(240)
                ),
            };
        }
    }

    // Run verification inside worktree, then ALWAYS detach and remove the worktree cleanly.
    let verdict = run_verification_in_worktree(
        cache_root,
        &slug_dir,
        &worktree_path,
        trimmed_diff,
        ground_truth_test_patch,
        &candidate_files,
        &gt_files,
    )
    .await;

    cleanup_worktree(&mirror_dir, &worktree_path).await;
    drop(wt_guard);

    verdict
}

async fn run_verification_in_worktree(
    cache_root: &Path,
    slug_dir: &str,
    worktree_path: &Path,
    candidate_diff: &str,
    ground_truth_test_patch: &str,
    candidate_files: &BTreeSet<String>,
    gt_files: &BTreeSet<String>,
) -> HostVerificationResult {
    let patch_dir = match TempDirGuard::new("judge-patches") {
        Ok(g) => g,
        Err(err) => {
            return HostVerificationResult {
                patch_applied: false,
                ground_truth_patch_applied: false,
                resolved_ground_truth: false,
                regression_free: false,
                test_tampering: false,
                blast_radius: None,
                failure_category: Some("patch_temp_dir_error".to_string()),
                diagnostic_tail: format!("Failed to create patch temp dir: {err}"),
            };
        }
    };

    // 4. Apply candidate diff via real `git apply`.
    let candidate_patch_path = patch_dir.path().join("candidate.patch");
    let normalized_candidate = ensure_trailing_newline(candidate_diff);
    if let Err(err) = tokio::fs::write(&candidate_patch_path, &normalized_candidate).await {
        return HostVerificationResult {
            patch_applied: false,
            ground_truth_patch_applied: false,
            resolved_ground_truth: false,
            regression_free: false,
            test_tampering: false,
            blast_radius: None,
            failure_category: Some("patch_write_error".to_string()),
            diagnostic_tail: format!("Failed to write candidate patch: {err}"),
        };
    }

    let apply_cand = run_git(
        worktree_path,
        &[
            "apply",
            "--whitespace=nowarn",
            &candidate_patch_path.to_string_lossy(),
        ],
        30,
    )
    .await;

    if !apply_cand.success() {
        let base_blast = compute_blast_radius_from_worktree(worktree_path, candidate_diff);
        return HostVerificationResult {
            patch_applied: false,
            ground_truth_patch_applied: false,
            resolved_ground_truth: false,
            regression_free: false,
            test_tampering: false,
            blast_radius: base_blast,
            failure_category: Some("patch_apply_conflict".to_string()),
            diagnostic_tail: format!(
                "git apply failed for candidate diff: {}",
                apply_cand.combined_tail(280)
            ),
        };
    }

    // 5. Compute BlastRadiusReport from the real checked-out (patched) repository files.
    let blast_radius = compute_blast_radius_from_worktree(worktree_path, candidate_diff);

    // 6. Apply hidden ground_truth_test_patch via real `git apply`.
    let trimmed_gt = ground_truth_test_patch.trim();
    if trimmed_gt.is_empty() {
        return HostVerificationResult {
            patch_applied: true,
            ground_truth_patch_applied: false,
            resolved_ground_truth: false,
            regression_free: false,
            test_tampering: false,
            blast_radius,
            failure_category: Some("missing_ground_truth_test_patch".to_string()),
            diagnostic_tail: "PR has no hidden ground-truth test patch to verify".to_string(),
        };
    }

    let gt_patch_path = patch_dir.path().join("ground_truth.patch");
    let normalized_gt = ensure_trailing_newline(trimmed_gt);
    if let Err(err) = tokio::fs::write(&gt_patch_path, &normalized_gt).await {
        return HostVerificationResult {
            patch_applied: true,
            ground_truth_patch_applied: false,
            resolved_ground_truth: false,
            regression_free: false,
            test_tampering: false,
            blast_radius,
            failure_category: Some("ground_truth_patch_write_error".to_string()),
            diagnostic_tail: format!("Failed to write ground-truth test patch: {err}"),
        };
    }

    let mut apply_gt = run_git(
        worktree_path,
        &[
            "apply",
            "--whitespace=nowarn",
            &gt_patch_path.to_string_lossy(),
        ],
        30,
    )
    .await;
    if !apply_gt.success() {
        apply_gt = run_git(
            worktree_path,
            &[
                "apply",
                "--recount",
                "--whitespace=nowarn",
                &gt_patch_path.to_string_lossy(),
            ],
            30,
        )
        .await;
    }

    if !apply_gt.success() {
        return HostVerificationResult {
            patch_applied: true,
            ground_truth_patch_applied: false,
            resolved_ground_truth: false,
            regression_free: false,
            test_tampering: false,
            blast_radius,
            failure_category: Some("ground_truth_patch_apply_failed".to_string()),
            diagnostic_tail: format!(
                "git apply failed for hidden ground-truth test patch: {}",
                apply_gt.combined_tail(280)
            ),
        };
    }

    // Collect existing ground-truth test files in the worktree after applying the test patch.
    let gt_test_files: Vec<String> = gt_files
        .iter()
        .filter(|rel| worktree_path.join(rel).is_file())
        .cloned()
        .collect();

    if gt_test_files.is_empty() {
        return HostVerificationResult {
            patch_applied: true,
            ground_truth_patch_applied: true,
            resolved_ground_truth: false,
            regression_free: false,
            test_tampering: false,
            blast_radius,
            failure_category: Some("no_executable_ground_truth_tests".to_string()),
            diagnostic_tail: "Ground-truth test patch did not produce any runnable test files"
                .to_string(),
        };
    }

    // 7. Execute language-appropriate ground-truth tests (60s timeout) + module regression suite.
    let lang = detect_repo_language(worktree_path, &gt_test_files);
    match lang {
        RepoLanguage::Python => {
            run_python_verification(
                cache_root,
                slug_dir,
                worktree_path,
                &gt_patch_path,
                &gt_test_files,
                candidate_files,
                blast_radius,
            )
            .await
        }
        RepoLanguage::TypeScriptJavaScript => {
            run_ts_js_verification(
                cache_root,
                slug_dir,
                worktree_path,
                &gt_patch_path,
                &gt_test_files,
                blast_radius,
            )
            .await
        }
        RepoLanguage::Unknown => HostVerificationResult {
            patch_applied: true,
            ground_truth_patch_applied: true,
            resolved_ground_truth: false,
            regression_free: false,
            test_tampering: false,
            blast_radius,
            failure_category: Some("unsupported_test_runtime".to_string()),
            diagnostic_tail: format!(
                "Could not detect Python or TS/JS test runtime for ground-truth files: {}",
                gt_test_files.join(", ")
            ),
        },
    }
}

async fn run_python_verification(
    cache_root: &Path,
    slug_dir: &str,
    worktree_path: &Path,
    gt_patch_path: &Path,
    gt_test_files: &[String],
    candidate_files: &BTreeSet<String>,
    blast_radius: Option<BlastRadiusReport>,
) -> HostVerificationResult {
    let venv_python = match ensure_python_venv(cache_root, slug_dir, worktree_path).await {
        Ok(py) => py,
        Err(err) => {
            return HostVerificationResult {
                patch_applied: true,
                ground_truth_patch_applied: true,
                resolved_ground_truth: false,
                regression_free: false,
                test_tampering: false,
                blast_radius,
                failure_category: Some("python_env_setup_failed".to_string()),
                diagnostic_tail: format!("Python/uv environment setup failed: {err}"),
            };
        }
    };

    let pythonpath = build_worktree_pythonpath(worktree_path);

    // Filter to runnable pytest files (`test_*.py` / `*_test.py`) so `.md`/`.json` fixtures
    // or `conftest.py` inside `tests/` never cause pytest collection/exit-code-5 errors.
    let runnable_py_tests: Vec<String> = gt_test_files
        .iter()
        .filter(|rel| {
            let lower = rel.to_ascii_lowercase();
            let fname = lower.rsplit('/').next().unwrap_or(&lower);
            lower.ends_with(".py") && (fname.starts_with("test_") || fname.ends_with("_test.py"))
        })
        .cloned()
        .collect();
    let effective_gt_targets = if runnable_py_tests.is_empty() {
        let dirs = collect_parent_dirs(worktree_path, gt_test_files);
        if dirs.is_empty() {
            gt_test_files.to_vec()
        } else {
            dirs
        }
    } else {
        runnable_py_tests
    };

    // Run hidden ground-truth tests (60s timeout).
    let mut gt_args: Vec<String> = vec![
        "-m".to_string(),
        "pytest".to_string(),
        "-q".to_string(),
        "--no-header".to_string(),
    ];
    gt_args.extend(effective_gt_targets);

    let gt_out = run_python_cmd(
        &venv_python,
        worktree_path,
        &pythonpath,
        &gt_args,
        TEST_EXEC_TIMEOUT_SECS,
    )
    .await;
    let resolved_ground_truth = gt_out.success();

    // If ground-truth tests failed, revert ONLY the hidden ground-truth test patch so
    // module regression testing honestly evaluates existing tests without double-counting
    // the new ground-truth failure as a regression.
    if !resolved_ground_truth {
        revert_ground_truth_test_patch(worktree_path, gt_patch_path, gt_test_files).await;
    }

    // Run module regression tests (60s timeout).
    let regression_targets =
        collect_python_regression_targets(worktree_path, gt_test_files, candidate_files);

    let (regression_free, reg_tail) = if regression_targets.is_empty() {
        // Repo had no pre-existing tests outside the newly added ground-truth test file.
        (true, "no pre-existing module regression tests found".to_string())
    } else {
        let mut reg_args: Vec<String> = vec![
            "-m".to_string(),
            "pytest".to_string(),
            "-q".to_string(),
            "--no-header".to_string(),
        ];
        reg_args.extend(regression_targets);
        let reg_out = run_python_cmd(
            &venv_python,
            worktree_path,
            &pythonpath,
            &reg_args,
            TEST_EXEC_TIMEOUT_SECS,
        )
        .await;
        (reg_out.success(), reg_out.combined_tail(220))
    };

    let (failure_category, diagnostic_tail) =
        summarize_test_verdict(resolved_ground_truth, regression_free, &gt_out, &reg_tail);

    HostVerificationResult {
        patch_applied: true,
        ground_truth_patch_applied: true,
        resolved_ground_truth,
        regression_free,
        test_tampering: false,
        blast_radius,
        failure_category,
        diagnostic_tail,
    }
}

async fn run_ts_js_verification(
    cache_root: &Path,
    slug_dir: &str,
    worktree_path: &Path,
    gt_patch_path: &Path,
    gt_test_files: &[String],
    blast_radius: Option<BlastRadiusReport>,
) -> HostVerificationResult {
    let linked_node_modules = ensure_node_modules_linked(cache_root, slug_dir, worktree_path).await;
    let package_json = std::fs::read_to_string(worktree_path.join("package.json")).ok();
    let uses_vitest = worktree_path
        .join("node_modules/.bin/vitest")
        .exists()
        || package_json
            .as_deref()
            .is_some_and(|s| s.contains("\"vitest\""));
    let has_npm_test = package_json
        .as_deref()
        .is_some_and(|s| s.contains("\"test\""));

    let gt_out = if uses_vitest {
        let mut args = vec!["--no-install", "vitest", "run"];
        let refs: Vec<&str> = gt_test_files.iter().map(String::as_str).collect();
        args.extend(refs);
        run_cmd("npx", worktree_path, &args, &[], TEST_EXEC_TIMEOUT_SECS).await
    } else if has_npm_test {
        let mut args = vec!["test", "--"];
        let refs: Vec<&str> = gt_test_files.iter().map(String::as_str).collect();
        args.extend(refs);
        run_cmd("npm", worktree_path, &args, &[], TEST_EXEC_TIMEOUT_SECS).await
    } else {
        let mut args = vec!["--test"];
        let refs: Vec<&str> = gt_test_files.iter().map(String::as_str).collect();
        args.extend(refs);
        run_cmd("node", worktree_path, &args, &[], TEST_EXEC_TIMEOUT_SECS).await
    };

    let resolved_ground_truth = gt_out.success();

    if !resolved_ground_truth {
        revert_ground_truth_test_patch(worktree_path, gt_patch_path, gt_test_files).await;
    }

    let reg_out = if uses_vitest {
        let module_dirs = collect_parent_dirs(worktree_path, gt_test_files);
        let mut args: Vec<String> = vec![
            "--no-install".to_string(),
            "vitest".to_string(),
            "run".to_string(),
        ];
        args.extend(module_dirs);
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        run_cmd("npx", worktree_path, &refs, &[], TEST_EXEC_TIMEOUT_SECS).await
    } else if has_npm_test {
        run_cmd(
            "npm",
            worktree_path,
            &["test"],
            &[],
            TEST_EXEC_TIMEOUT_SECS,
        )
        .await
    } else {
        let existing_js_tests = collect_js_regression_targets(worktree_path, gt_test_files);
        if existing_js_tests.is_empty() {
            CmdOutput {
                exit_code: Some(0),
                timed_out: false,
                stdout: "no pre-existing JS regression tests found".to_string(),
                stderr: String::new(),
            }
        } else {
            let mut args = vec!["--test"];
            let refs: Vec<&str> = existing_js_tests.iter().map(String::as_str).collect();
            args.extend(refs);
            run_cmd("node", worktree_path, &args, &[], TEST_EXEC_TIMEOUT_SECS).await
        }
    };

    if linked_node_modules {
        let _ = std::fs::remove_file(worktree_path.join("node_modules"));
    }

    let regression_free = reg_out.success();
    let reg_tail = reg_out.combined_tail(220);
    let (failure_category, diagnostic_tail) =
        summarize_test_verdict(resolved_ground_truth, regression_free, &gt_out, &reg_tail);

    HostVerificationResult {
        patch_applied: true,
        ground_truth_patch_applied: true,
        resolved_ground_truth,
        regression_free,
        test_tampering: false,
        blast_radius,
        failure_category,
        diagnostic_tail,
    }
}

fn summarize_test_verdict(
    resolved_ground_truth: bool,
    regression_free: bool,
    gt_out: &CmdOutput,
    reg_tail: &str,
) -> (Option<String>, String) {
    match (resolved_ground_truth, regression_free) {
        (true, true) => (
            None,
            format!(
                "PASSED host ground-truth & module regression tests: {}",
                gt_out.combined_tail(200)
            ),
        ),
        (true, false) => (
            Some("regression_suite_failed".to_string()),
            format!("PASSED ground-truth tests, FAILED module regression suite: {reg_tail}"),
        ),
        (false, true) => (
            Some("ground_truth_tests_failed".to_string()),
            format!(
                "FAILED host ground-truth tests (existing module suite passed): {}",
                gt_out.combined_tail(240)
            ),
        ),
        (false, false) => (
            Some("ground_truth_and_regression_failed".to_string()),
            format!(
                "FAILED host ground-truth tests AND module regression suite: {} | reg: {}",
                gt_out.combined_tail(160),
                truncate_tail(reg_tail, 120)
            ),
        ),
    }
}

async fn ensure_cached_mirror(
    cache_root: &Path,
    repo_url: &str,
    parent_commit: &str,
) -> std::result::Result<(PathBuf, String), (&'static str, String)> {
    let (clone_url, slug_dir) = resolve_clone_target(repo_url);
    let mirror_dir = cache_root.join(&slug_dir);

    if let Err(err) = std::fs::create_dir_all(cache_root) {
        return Err((
            "repo_checkout_failed",
            format!("Failed to create repo cache dir {}: {err}", cache_root.display()),
        ));
    }

    let mirror_lock = repo_mutex_for(&mirror_dir);
    let _guard = mirror_lock.lock().await;

    if !mirror_dir.join(".git").exists() {
        let _ = std::fs::remove_dir_all(&mirror_dir);
        let clone_out = run_git_unscoped(
            &[
                "-c",
                "credential.helper=",
                "-c",
                "core.askPass=true",
                "clone",
                "--no-checkout",
                &clone_url,
                &mirror_dir.to_string_lossy(),
            ],
            GIT_CLONE_TIMEOUT_SECS,
        )
        .await;
        if !clone_out.success() {
            let _ = std::fs::remove_dir_all(&mirror_dir);
            return Err((
                "repo_checkout_failed",
                format!(
                    "git clone failed for {clone_url}: {}",
                    clone_out.combined_tail(240)
                ),
            ));
        }
    }

    let commit_spec = format!("{parent_commit}^{{commit}}");
    let has_commit = run_git(&mirror_dir, &["cat-file", "-e", &commit_spec], 15).await;
    if !has_commit.success() {
        let fetch_commit = run_git(
            &mirror_dir,
            &[
                "-c",
                "credential.helper=",
                "-c",
                "core.askPass=true",
                "fetch",
                "--no-tags",
                "origin",
                parent_commit,
            ],
            GIT_CLONE_TIMEOUT_SECS,
        )
        .await;
        if !fetch_commit.success() {
            let _ = run_git(
                &mirror_dir,
                &[
                    "-c",
                    "credential.helper=",
                    "-c",
                    "core.askPass=true",
                    "fetch",
                    "--all",
                    "--tags",
                ],
                GIT_CLONE_TIMEOUT_SECS,
            )
            .await;
        }
        let recheck = run_git(&mirror_dir, &["cat-file", "-e", &commit_spec], 15).await;
        if !recheck.success() {
            return Err((
                "parent_commit_not_found",
                format!(
                    "Parent commit '{parent_commit}' not found in repository mirror for {clone_url}"
                ),
            ));
        }
    }

    Ok((mirror_dir, slug_dir))
}

async fn cleanup_worktree(mirror_dir: &Path, worktree_path: &Path) {
    let mirror_lock = repo_mutex_for(mirror_dir);
    let _guard = mirror_lock.lock().await;
    let _ = run_git(
        mirror_dir,
        &[
            "worktree",
            "remove",
            "--force",
            &worktree_path.to_string_lossy(),
        ],
        30,
    )
    .await;
    let _ = std::fs::remove_dir_all(worktree_path);
    let _ = run_git(mirror_dir, &["worktree", "prune"], 15).await;
}

async fn ensure_python_venv(
    cache_root: &Path,
    slug_dir: &str,
    worktree_path: &Path,
) -> std::result::Result<PathBuf, String> {
    let has_pkg_manifest = worktree_path.join("pyproject.toml").exists()
        || worktree_path.join("setup.py").exists()
        || worktree_path.join("setup.cfg").exists()
        || worktree_path.join("requirements.txt").exists();

    let venvs_root = cache_root
        .parent()
        .unwrap_or(cache_root)
        .join("venvs");
    let venv_dir = if has_pkg_manifest {
        venvs_root.join(slug_dir)
    } else {
        venvs_root.join("_pytest_base")
    };

    let venv_lock = repo_mutex_for(&venv_dir);
    let _guard = venv_lock.lock().await;

    std::fs::create_dir_all(&venvs_root)
        .map_err(|e| format!("failed to create venvs root {}: {e}", venvs_root.display()))?;

    let uv_bin = if Path::new("/opt/homebrew/bin/uv").exists() {
        "/opt/homebrew/bin/uv"
    } else {
        "uv"
    };
    let venv_python = venv_dir.join("bin/python");
    let marker = venv_dir.join(".epicenter_deps_ready");

    if !venv_python.exists() {
        let mut venv_out = run_cmd(
            uv_bin,
            worktree_path,
            &["venv", "--python", "3.12", &venv_dir.to_string_lossy()],
            &[],
            ENV_SETUP_TIMEOUT_SECS,
        )
        .await;
        if !venv_out.success() {
            venv_out = run_cmd(
                uv_bin,
                worktree_path,
                &["venv", &venv_dir.to_string_lossy()],
                &[],
                ENV_SETUP_TIMEOUT_SECS,
            )
            .await;
        }
        if !venv_out.success() {
            return Err(format!("uv venv failed: {}", venv_out.combined_tail(200)));
        }
    }

    if !marker.exists() {
        let py_str = venv_python.to_string_lossy().to_string();
        let pytest_out = run_cmd(
            uv_bin,
            worktree_path,
            &["pip", "install", "--python", &py_str, "pytest", "pyyaml"],
            &[],
            ENV_SETUP_TIMEOUT_SECS,
        )
        .await;
        if !pytest_out.success() {
            return Err(format!(
                "uv pip install pytest failed: {}",
                pytest_out.combined_tail(200)
            ));
        }

        if worktree_path.join("pyproject.toml").exists()
            || worktree_path.join("setup.py").exists()
            || worktree_path.join("setup.cfg").exists()
        {
            let wt_str = worktree_path.to_string_lossy().to_string();
            let editable_out = run_cmd(
                uv_bin,
                worktree_path,
                &["pip", "install", "--python", &py_str, "-e", &wt_str],
                &[],
                ENV_SETUP_TIMEOUT_SECS,
            )
            .await;
            if !editable_out.success() {
                warn!(
                    slug = %slug_dir,
                    detail = %editable_out.combined_tail(200),
                    "best-effort editable install of repo dependencies failed; proceeding with PYTHONPATH"
                );
            }
        }

        for req_rel in ["requirements/tests.txt", "requirements-test.txt", "requirements.txt"] {
            let req_path = worktree_path.join(req_rel);
            if req_path.is_file() {
                let req_str = req_path.to_string_lossy().to_string();
                let _ = run_cmd(
                    uv_bin,
                    worktree_path,
                    &["pip", "install", "--python", &py_str, "-r", &req_str],
                    &[],
                    ENV_SETUP_TIMEOUT_SECS,
                )
                .await;
            }
        }

        let _ = std::fs::write(&marker, b"ready\n");
    }

    // CRITICAL: Always strip mirror/worktree editable `.pth` and `__editable__*` hooks so
    // `PYTHONPATH=<worktree>/src:<worktree>` ALWAYS wins over any cached editable install!
    strip_editable_pth_files(&venv_dir);

    Ok(venv_python)
}

/// Removes editable `.pth`, `__editable__*`, and `.egg-link` files from a cached Python venv's
/// `site-packages/` so isolated worktree edits via `PYTHONPATH` can never be shadowed by a
/// stale mirror or prior worktree editable install.
pub fn strip_editable_pth_files(venv_dir: &Path) {
    let lib_dir = venv_dir.join("lib");
    let Ok(py_dirs) = std::fs::read_dir(&lib_dir) else {
        return;
    };

    for entry in py_dirs.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("python") {
            continue;
        }
        let site_packages = entry.path().join("site-packages");
        let Ok(sp_entries) = std::fs::read_dir(&site_packages) else {
            continue;
        };

        for sp_entry in sp_entries.flatten() {
            let path = sp_entry.path();
            let Some(fname) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            let lower = fname.to_ascii_lowercase();

            if lower.starts_with("__editable__")
                || lower.starts_with("_editable_")
                || lower.ends_with(".egg-link")
            {
                let _ = std::fs::remove_file(&path);
                continue;
            }

            if lower.ends_with(".pth")
                && lower != "_virtualenv.pth"
                && lower != "distutils-precedence.pth"
            {
                if let Ok(contents) = std::fs::read_to_string(&path) {
                    let points_to_checkout = contents.lines().any(|line| {
                        let trimmed = line.trim();
                        !trimmed.is_empty()
                            && !trimmed.starts_with('#')
                            && (trimmed.contains("epicenter-")
                                || trimmed.contains("/tmp/")
                                || trimmed.contains("/private/tmp/")
                                || trimmed.contains("/var/folders/")
                                || trimmed.starts_with("import __editable__")
                                || (Path::new(trimmed).is_absolute()
                                    && !Path::new(trimmed).starts_with(venv_dir)))
                    });
                    if points_to_checkout {
                        let _ = std::fs::remove_file(&path);
                    }
                }
            }
        }
    }
}

fn build_worktree_pythonpath(worktree_path: &Path) -> String {
    let src_dir = worktree_path.join("src");
    if src_dir.is_dir() {
        format!("{}:{}", src_dir.display(), worktree_path.display())
    } else {
        worktree_path.display().to_string()
    }
}

async fn ensure_node_modules_linked(
    cache_root: &Path,
    slug_dir: &str,
    worktree_path: &Path,
) -> bool {
    let pkg_json = worktree_path.join("package.json");
    if !pkg_json.is_file() || worktree_path.join("node_modules").exists() {
        return false;
    }

    let node_cache_dir = cache_root
        .parent()
        .unwrap_or(cache_root)
        .join("node")
        .join(slug_dir);
    let node_lock = repo_mutex_for(&node_cache_dir);
    let _guard = node_lock.lock().await;

    let cached_nm = node_cache_dir.join("node_modules");
    if !cached_nm.is_dir() {
        if std::fs::create_dir_all(&node_cache_dir).is_err() {
            return false;
        }
        let _ = std::fs::copy(&pkg_json, node_cache_dir.join("package.json"));
        for lockfile in ["package-lock.json", "npm-shrinkwrap.json"] {
            let src = worktree_path.join(lockfile);
            if src.is_file() {
                let _ = std::fs::copy(&src, node_cache_dir.join(lockfile));
            }
        }
        let install_out = run_cmd(
            "npm",
            &node_cache_dir,
            &["install", "--no-audit", "--no-fund", "--ignore-scripts"],
            &[],
            ENV_SETUP_TIMEOUT_SECS,
        )
        .await;
        if !install_out.success() || !cached_nm.is_dir() {
            warn!(
                slug = %slug_dir,
                detail = %install_out.combined_tail(200),
                "npm install failed for cached node_modules"
            );
            return false;
        }
    }

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&cached_nm, worktree_path.join("node_modules")).is_ok()
    }
    #[cfg(not(unix))]
    {
        false
    }
}

async fn revert_ground_truth_test_patch(
    worktree_path: &Path,
    gt_patch_path: &Path,
    gt_test_files: &[String],
) {
    let rev = run_git(
        worktree_path,
        &[
            "apply",
            "-R",
            "--whitespace=nowarn",
            &gt_patch_path.to_string_lossy(),
        ],
        20,
    )
    .await;
    if !rev.success() {
        for rel in gt_test_files {
            let co = run_git(worktree_path, &["checkout", "--", rel], 10).await;
            if !co.success() {
                let _ = std::fs::remove_file(worktree_path.join(rel));
            }
        }
    }
}

fn collect_python_regression_targets(
    worktree_path: &Path,
    gt_test_files: &[String],
    candidate_files: &BTreeSet<String>,
) -> Vec<String> {
    let mut parent_dirs = BTreeSet::new();
    for rel in gt_test_files {
        if let Some(parent) = Path::new(rel).parent() {
            let p_str = normalize_rel_path(&parent.to_string_lossy());
            if !p_str.is_empty() && worktree_path.join(&p_str).is_dir() {
                parent_dirs.insert(p_str);
            }
        }
    }
    if parent_dirs.is_empty() {
        for fallback in ["tests", "test"] {
            if worktree_path.join(fallback).is_dir() {
                parent_dirs.insert(fallback.to_string());
                break;
            }
        }
    }

    let mut all_py_tests: Vec<String> = Vec::new();
    for dir_rel in &parent_dirs {
        collect_py_test_files_recursive(worktree_path, &worktree_path.join(dir_rel), &mut all_py_tests);
    }
    all_py_tests.sort();
    all_py_tests.dedup();

    if all_py_tests.is_empty() {
        return Vec::new();
    }
    if all_py_tests.len() <= 40 {
        return all_py_tests;
    }

    // For large repositories (>40 test files in tests/), prioritize tests matching modified
    // module stems plus existing ground-truth test files so regression completes well within 60s.
    let stems: HashSet<String> = candidate_files
        .iter()
        .filter_map(|p| {
            Path::new(p)
                .file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.to_ascii_lowercase())
        })
        .filter(|s| s.len() >= 3 && s != "__init__")
        .collect();

    let mut prioritized = Vec::new();
    let mut seen = HashSet::new();

    for gt in gt_test_files {
        if worktree_path.join(gt).is_file() && seen.insert(gt.clone()) {
            prioritized.push(gt.clone());
        }
    }
    for test_file in &all_py_tests {
        let lower = test_file.to_ascii_lowercase();
        if stems.iter().any(|stem| lower.contains(stem)) && seen.insert(test_file.clone()) {
            prioritized.push(test_file.clone());
        }
    }
    for test_file in &all_py_tests {
        if prioritized.len() >= 20 {
            break;
        }
        if seen.insert(test_file.clone()) {
            prioritized.push(test_file.clone());
        }
    }

    prioritized
}

fn collect_py_test_files_recursive(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name != "__pycache__" && !name.starts_with('.') {
                collect_py_test_files_recursive(root, &path, out);
            }
        } else if path.is_file() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.ends_with(".py") && (name.starts_with("test_") || name.ends_with("_test.py")) {
                if let Ok(rel) = path.strip_prefix(root) {
                    out.push(normalize_rel_path(&rel.to_string_lossy()));
                }
            }
        }
    }
}

fn collect_parent_dirs(worktree_path: &Path, files: &[String]) -> Vec<String> {
    let mut dirs = BTreeSet::new();
    for rel in files {
        if let Some(parent) = Path::new(rel).parent() {
            let p_str = normalize_rel_path(&parent.to_string_lossy());
            if !p_str.is_empty() && worktree_path.join(&p_str).is_dir() {
                dirs.insert(p_str);
            }
        }
    }
    dirs.into_iter().collect()
}

fn collect_js_regression_targets(worktree_path: &Path, gt_test_files: &[String]) -> Vec<String> {
    let dirs = collect_parent_dirs(worktree_path, gt_test_files);
    let mut targets = Vec::new();
    for dir_rel in dirs {
        if let Ok(entries) = std::fs::read_dir(worktree_path.join(&dir_rel)) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
                    if name.ends_with(".test.js")
                        || name.ends_with(".test.mjs")
                        || name.ends_with(".spec.js")
                    {
                        if let Ok(rel) = path.strip_prefix(worktree_path) {
                            targets.push(normalize_rel_path(&rel.to_string_lossy()));
                        }
                    }
                }
            }
        }
    }
    targets.sort();
    targets.dedup();
    targets
}

fn detect_repo_language(worktree_path: &Path, gt_test_files: &[String]) -> RepoLanguage {
    if gt_test_files.iter().any(|f| f.ends_with(".py")) {
        return RepoLanguage::Python;
    }
    if gt_test_files.iter().any(|f| {
        f.ends_with(".ts")
            || f.ends_with(".tsx")
            || f.ends_with(".js")
            || f.ends_with(".jsx")
            || f.ends_with(".mjs")
            || f.ends_with(".cjs")
    }) {
        return RepoLanguage::TypeScriptJavaScript;
    }
    if worktree_path.join("pyproject.toml").exists()
        || worktree_path.join("setup.py").exists()
        || worktree_path.join("setup.cfg").exists()
        || worktree_path.join("pytest.ini").exists()
    {
        return RepoLanguage::Python;
    }
    if worktree_path.join("package.json").exists() {
        return RepoLanguage::TypeScriptJavaScript;
    }
    RepoLanguage::Unknown
}

/// Computes a real `BlastRadiusReport` directly from the checked-out repository files on disk.
fn compute_blast_radius_from_worktree(
    worktree_path: &Path,
    candidate_diff: &str,
) -> Option<BlastRadiusReport> {
    let graph = match RepoCallGraph::from_directory(worktree_path) {
        Ok(g) => g,
        Err(_) => {
            // Tolerant fallback walker in case a repository contains a non-UTF-8 fixture file.
            let sources = collect_utf8_sources_tolerant(worktree_path);
            if sources.is_empty() {
                return None;
            }
            RepoCallGraph::from_sources(&sources)
        }
    };
    Some(graph.analyze_diff(candidate_diff, None, &BlastAnalysisConfig::default()))
}

fn collect_utf8_sources_tolerant(root: &Path) -> BTreeMap<String, String> {
    const IGNORED_DIRS: &[&str] = &[
        ".git",
        "node_modules",
        "target",
        ".venv",
        "venv",
        "__pycache__",
        "dist",
        "build",
        ".next",
        ".mypy_cache",
        ".pytest_cache",
    ];
    const EXTENSIONS: &[&str] = &[
        "py", "ts", "tsx", "js", "jsx", "mjs", "cjs", "mts", "cts", "rs", "go", "java", "kt",
        "rb", "cs", "cpp", "c", "h", "hpp",
    ];
    const MAX_FILE_BYTES: u64 = 256 * 1024;
    const MAX_FILES: usize = 600;

    let mut sources = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        if sources.len() >= MAX_FILES {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let name = entry.file_name().to_string_lossy().to_string();
                if !IGNORED_DIRS.contains(&name.as_str()) {
                    stack.push(path);
                }
            } else if path.is_file() {
                let Some(ext) = path.extension().and_then(|s| s.to_str()) else {
                    continue;
                };
                if !EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()) {
                    continue;
                }
                if entry.metadata().map(|m| m.len()).unwrap_or(u64::MAX) > MAX_FILE_BYTES {
                    continue;
                }
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Ok(rel) = path.strip_prefix(root) {
                        sources.insert(normalize_rel_path(&rel.to_string_lossy()), content);
                    }
                }
            }
        }
    }

    sources
}

/// Extracts all normalized relative file paths touched by a unified diff.
pub fn extract_touched_paths(diff: &str) -> BTreeSet<String> {
    let mut paths = BTreeSet::new();
    for file_diff in parse_graph_diff(diff) {
        let norm = normalize_rel_path(&file_diff.path);
        if !norm.is_empty() && norm != "/dev/null" {
            paths.insert(norm);
        }
    }
    if let Ok(gh_patches) = parse_github_diff(diff) {
        for patch in gh_patches {
            if let Some(p) = patch.target_path() {
                let norm = normalize_rel_path(p);
                if !norm.is_empty() && norm != "/dev/null" {
                    paths.insert(norm);
                }
            }
        }
    }
    paths
}

fn resolve_clone_target(repo_url: &str) -> (String, String) {
    if let Ok(slug) = parse_github_repo_url(repo_url) {
        let clone_url = format!("https://github.com/{}/{}.git", slug.owner, slug.repo);
        let dir_slug = format!(
            "{}__{}",
            sanitize_path_segment(&slug.owner),
            sanitize_path_segment(&slug.repo)
        );
        return (clone_url, dir_slug);
    }

    let trimmed = repo_url.trim();
    let basename = Path::new(trimmed.trim_start_matches("file://"))
        .file_name()
        .and_then(|s| s.to_str())
        .map(sanitize_path_segment)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "repo".to_string());
    let hash = fnv1a_64(trimmed.as_bytes());
    (trimmed.to_string(), format!("local__{basename}_{hash:016x}"))
}

fn sanitize_path_segment(seg: &str) -> String {
    seg.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn repo_mutex_for(path: &Path) -> Arc<AsyncMutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<AsyncMutex<()>>>>> = OnceLock::new();
    let map = LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = map.lock().expect("repo cache lock map poisoned");
    guard
        .entry(path.to_path_buf())
        .or_insert_with(|| Arc::new(AsyncMutex::new(())))
        .clone()
}

async fn run_git(cwd: &Path, args: &[&str], timeout_secs: u64) -> CmdOutput {
    run_cmd(
        "git",
        cwd,
        args,
        &[("GIT_TERMINAL_PROMPT", "0")],
        timeout_secs,
    )
    .await
}

async fn run_git_unscoped(args: &[&str], timeout_secs: u64) -> CmdOutput {
    run_cmd(
        "git",
        &std::env::temp_dir(),
        args,
        &[("GIT_TERMINAL_PROMPT", "0")],
        timeout_secs,
    )
    .await
}

async fn run_python_cmd(
    venv_python: &Path,
    cwd: &Path,
    pythonpath: &str,
    args: &[String],
    timeout_secs: u64,
) -> CmdOutput {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    run_cmd(
        &venv_python.to_string_lossy(),
        cwd,
        &refs,
        &[
            ("PYTHONPATH", pythonpath),
            ("PYTHONDONTWRITEBYTECODE", "1"),
        ],
        timeout_secs,
    )
    .await
}

async fn run_cmd(
    program: &str,
    cwd: &Path,
    args: &[&str],
    envs: &[(&str, &str)],
    timeout_secs: u64,
) -> CmdOutput {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(cwd)
        .kill_on_drop(true);
    for &(k, v) in envs {
        cmd.env(k, v);
    }

    match tokio::time::timeout(Duration::from_secs(timeout_secs), cmd.output()).await {
        Ok(Ok(out)) => CmdOutput {
            exit_code: out.status.code(),
            timed_out: false,
            stdout: String::from_utf8_lossy(&out.stdout).to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).to_string(),
        },
        Ok(Err(err)) => CmdOutput {
            exit_code: None,
            timed_out: false,
            stdout: String::new(),
            stderr: format!("failed to spawn '{program}': {err}"),
        },
        Err(_) => CmdOutput {
            exit_code: None,
            timed_out: true,
            stdout: String::new(),
            stderr: format!("command '{program}' timed out after {timeout_secs}s"),
        },
    }
}

/// Downloads reachable PR media URLs (15s timeout, max 15MB each) so Arm 3's Tournament Engine
/// receives real multimodal visual evidence instead of an empty attachment list.
pub async fn download_pr_media_attachments(media_urls: &[String]) -> Vec<UploadedAttachmentBytes> {
    if media_urls.is_empty() {
        return Vec::new();
    }

    let Ok(client) = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(MEDIA_DOWNLOAD_TIMEOUT_SECS))
        .user_agent("epicenter-server/0.1.0")
        .build()
    else {
        return Vec::new();
    };

    let mut attachments = Vec::new();

    for (idx, url) in media_urls.iter().take(MAX_MEDIA_ATTACHMENTS).enumerate() {
        let resp = match client.get(url).send().await {
            Ok(r) if r.status().is_success() => r,
            Ok(r) => {
                warn!(%url, status = %r.status(), "skipping unreachable PR media URL");
                continue;
            }
            Err(err) => {
                warn!(%url, %err, "failed downloading PR media URL");
                continue;
            }
        };

        if let Some(len) = resp.content_length() {
            if len as usize > MAX_MEDIA_DOWNLOAD_BYTES {
                warn!(%url, content_length = len, "skipping PR media URL exceeding 15MB limit");
                continue;
            }
        }

        let header_mime = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.split(';').next())
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty());

        if header_mime
            .as_deref()
            .is_some_and(|m| m.starts_with("text/html"))
        {
            warn!(%url, "skipping PR media URL that returned text/html");
            continue;
        }

        let mut bytes = Vec::new();
        let mut stream = resp.bytes_stream();
        let mut exceeded = false;
        while let Some(chunk_res) = stream.next().await {
            match chunk_res {
                Ok(chunk) => {
                    if bytes.len() + chunk.len() > MAX_MEDIA_DOWNLOAD_BYTES {
                        exceeded = true;
                        break;
                    }
                    bytes.extend_from_slice(&chunk);
                }
                Err(err) => {
                    warn!(%url, %err, "stream error while reading PR media URL");
                    exceeded = true;
                    break;
                }
            }
        }

        if exceeded || bytes.is_empty() {
            continue;
        }

        let filename = extract_filename_from_url(url, idx);
        let mime_type = header_mime
            .unwrap_or_else(|| crate::routes::infer_mime_from_filename(&filename).to_string());
        let media_kind = infer_media_kind(&mime_type);
        let meta = AttachmentMeta {
            id: Uuid::new_v4(),
            filename,
            mime_type,
            media_kind,
            size_bytes: bytes.len() as u64,
            file_uri: None,
        };
        info!(%url, filename = %meta.filename, bytes = meta.size_bytes, "downloaded PR media attachment for Arm 3");
        attachments.push(UploadedAttachmentBytes { meta, bytes });
    }

    attachments
}

fn extract_filename_from_url(url: &str, idx: usize) -> String {
    let without_query = url.split(['?', '#']).next().unwrap_or(url);
    without_query
        .rsplit('/')
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty() && s.len() <= 128)
        .map(str::to_string)
        .unwrap_or_else(|| format!("pr-media-{idx}.bin"))
}

fn ensure_trailing_newline(s: &str) -> String {
    if s.ends_with('\n') {
        s.to_string()
    } else {
        format!("{s}\n")
    }
}

fn truncate_tail(s: &str, max_chars: usize) -> String {
    let trimmed = s.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let tail: String = trimmed.chars().rev().take(max_chars).collect();
    format!("...{}", tail.chars().rev().collect::<String>())
}

fn build_problem_statement(ctx: &FetchedPrContext) -> String {
    let mut prompt = format!(
        "Resolve issue described in Pull Request #{}: {}\n\n{}",
        ctx.slug.number,
        ctx.title,
        if ctx.body.trim().is_empty() {
            "(No PR description body provided.)"
        } else {
            ctx.body.trim()
        }
    );
    if !ctx.media_urls.is_empty() {
        prompt.push_str("\n\nAttached visual evidence URLs:\n");
        for url in &ctx.media_urls {
            prompt.push_str(&format!("- {url}\n"));
        }
    }
    prompt
}

fn truncate_head(s: &str, max_chars: usize) -> String {
    let trimmed = s.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let head: String = trimmed.chars().take(max_chars).collect();
    format!("{head}...")
}

fn summarize_blast_nodes(nodes: &[GraphNode]) -> String {
    const MAX_SUMMARY_NODES: usize = 12;
    if nodes.is_empty() {
        return "(none)".to_string();
    }
    let mut summary = nodes
        .iter()
        .take(MAX_SUMMARY_NODES)
        .map(|n| {
            let dz = if n.is_untested_dark_zone {
                " [DARK ZONE]"
            } else {
                ""
            };
            format!("{} ({}:{}{})", n.symbol, n.file_path, n.line, dz)
        })
        .collect::<Vec<_>>()
        .join(", ");
    if nodes.len() > MAX_SUMMARY_NODES {
        let extra = nodes.len() - MAX_SUMMARY_NODES;
        summary.push_str(&format!(", ... (+{extra} more)"));
    }
    summary
}

/// Builds a `gemini-decision-1` pre-flight PR spec clarity triage request over PR title, body,
/// and modified source file paths.
pub fn build_pr_spec_clarity_request(
    title: &str,
    body: &str,
    modified_source_files: &[String],
) -> CreateDecisionRequest {
    let files_list = if modified_source_files.is_empty() {
        "(none listed)".to_string()
    } else {
        modified_source_files
            .iter()
            .take(25)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    };
    let body_block = if body.trim().is_empty() {
        "(No PR description body provided.)".to_string()
    } else {
        truncate_head(body, 4_000)
    };
    let input = format!(
        "PR Title: {title}\n\nPR Description:\n{body_block}\n\nModified Source Files: {files_list}"
    );
    CreateDecisionRequest::new(input)
        .with_question(
            "has_actionable_spec",
            DecisionQuestion::noul(
                "Does this PR title and description provide enough concrete behavioral detail for an engineer to implement the expected fix without seeing the code diff?",
            ),
        )
        .with_question(
            "is_self_contained",
            DecisionQuestion::noul(
                "Can this fix likely be verified by standalone unit tests without external databases, Docker services, or live browsers?",
            ),
        )
}

/// Parses a `gemini-decision-1` pre-flight spec clarity response into a `PrSpecClaritySignal`.
pub fn parse_pr_spec_clarity_response(resp: &DecisionResponse) -> Option<PrSpecClaritySignal> {
    let has_actionable_spec = resp.noul_prob("has_actionable_spec")?;
    let is_self_contained = resp.noul_prob("is_self_contained").unwrap_or(0.5);
    let confs: Vec<f64> = ["has_actionable_spec", "is_self_contained"]
        .into_iter()
        .filter_map(|k| resp.confidence(k))
        .collect();
    let confidence = if confs.is_empty() {
        None
    } else {
        Some(confs.iter().sum::<f64>() / confs.len() as f64)
    };
    Some(PrSpecClaritySignal {
        has_actionable_spec,
        is_self_contained,
        confidence,
    })
}

/// Appends a concise spec-clarity caveat to failed arm outcomes when `has_actionable_spec < 0.30`.
pub fn apply_spec_clarity_note_to_failed_arms(
    outcomes: &mut [ArmOutcome],
    clarity: &PrSpecClaritySignal,
) {
    if clarity.has_actionable_spec >= LOW_SPEC_CLARITY_THRESHOLD {
        return;
    }
    let caveat = format!(
        "Low PR spec clarity ({:.0}%)",
        (clarity.has_actionable_spec * 100.0).clamp(0.0, 100.0)
    );
    for outcome in outcomes.iter_mut() {
        if !outcome.resolved_ground_truth || !outcome.regression_free {
            match &mut outcome.notes {
                Some(existing) if !existing.trim().is_empty() => {
                    existing.push_str(" | ");
                    existing.push_str(&caveat);
                }
                slot => {
                    *slot = Some(caveat.clone());
                }
            }
        }
    }
}

async fn evaluate_pr_spec_clarity(
    gemini: Option<&GeminiClient>,
    ctx: &FetchedPrContext,
) -> Option<PrSpecClaritySignal> {
    let client = gemini?;
    let req = build_pr_spec_clarity_request(&ctx.title, &ctx.body, &ctx.modified_source_files);
    match tokio::time::timeout(
        Duration::from_secs(DECISIONS_PREFLIGHT_TIMEOUT_SECS),
        client.evaluate_decisions(&req),
    )
    .await
    {
        Ok(Ok(resp)) => {
            let signal = parse_pr_spec_clarity_response(&resp);
            if let Some(sig) = signal {
                info!(
                    pr = %format!("{}/{}#{}", ctx.slug.owner, ctx.slug.repo, ctx.slug.number),
                    has_actionable_spec = sig.has_actionable_spec,
                    is_self_contained = sig.is_self_contained,
                    confidence = ?sig.confidence,
                    "pre-flight PR spec clarity triage completed via gemini-decision-1"
                );
            }
            signal
        }
        Ok(Err(err)) => {
            warn!(%err, "pre-flight PR spec clarity triage failed (non-fatal fallback)");
            None
        }
        Err(_) => {
            warn!(
                timeout_secs = DECISIONS_PREFLIGHT_TIMEOUT_SECS,
                "pre-flight PR spec clarity triage timed out (non-fatal fallback)"
            );
            None
        }
    }
}

/// Builds the calibrated `gemini-decision-1` 5-level root-cause & regression hazard request
/// over the problem statement, candidate diff, and structural Ring 0/1/2 blast-radius summary.
pub fn build_blast_calibration_request(
    problem_statement: &str,
    candidate_diff: &str,
    blast: &BlastRadiusReport,
) -> CreateDecisionRequest {
    let ring0 = summarize_blast_nodes(&blast.ring0_mutated);
    let ring1 = summarize_blast_nodes(&blast.ring1_direct_callers);
    let ring2 = summarize_blast_nodes(&blast.ring2_transitive_entrypoints);
    let input = format!(
        "PROBLEM STATEMENT:\n{}\n\n\
         STRUCTURAL BLAST RADIUS:\n\
         - Ring 0 (Mutated): {ring0}\n\
         - Ring 1 (Direct Callers): {ring1}\n\
         - Ring 2 (Transitive Entrypoints): {ring2}\n\
         - Untested Dark Zones: {}\n\
         - Base Structural Risk Score: {:.1}\n\n\
         CANDIDATE DIFF:\n```diff\n{}\n```",
        truncate_head(problem_statement, 2_500),
        blast.dark_zone_count(),
        blast.composite_risk_score,
        truncate_head(candidate_diff, 8_000),
    );

    CreateDecisionRequest::new(input)
        .with_question(
            "root_cause_quality",
            DecisionQuestion::score(
                "Rate how deeply and cleanly this candidate diff cures the underlying root cause of the reported issue rather than masking symptoms.",
                [
                    "Incorrect or superficial patch that fails to address the defect or introduces clear breakage",
                    "Symptom-only bandaid or narrow downstream guard that leaves the underlying root-cause invariant broken",
                    "Partial fix that handles the reported case but misses related edge cases or caller assumptions",
                    "Solid targeted fix that corrects the core defect logic cleanly without disturbing caller contracts",
                    "Comprehensive architectural root-cause cure that restores the upstream invariant cleanly across callers and edge cases",
                ],
            ),
        )
        .with_question(
            "is_symptom_bandaid",
            DecisionQuestion::noul(
                "Does this candidate diff merely mask symptoms with a narrow downstream guard or bandaid instead of curing the underlying root-cause invariant?",
            ),
        )
        .with_question(
            "regression_hazard",
            DecisionQuestion::noul(
                "Does this candidate diff pose a meaningful risk of causing behavioral regressions, contract breakages, or subtle side effects for Ring 1 or Ring 2 callers?",
            ),
        )
}

/// Pure calibration mapping from a `gemini-decision-1` `DecisionResponse` onto a `BlastRadiusReport`.
///
/// Updates `root_cause_depth_score`, `bandaid_probability`, `decision_confidence`, appends a
/// calibrated `SemanticHazard` when `regression_hazard >= 0.58` (`High` if `>= 0.78`, else `Medium`),
/// and recalculates `composite_risk_score`. Returns `true` if any calibration field was applied.
pub fn apply_decision_calibration_to_blast_radius(
    resp: &DecisionResponse,
    blast: &mut BlastRadiusReport,
) -> bool {
    let existing_hazard_penalty: f32 = blast
        .semantic_hazards
        .iter()
        .map(|h| h.severity.risk_weight())
        .sum();
    let base_score = (blast.composite_risk_score - existing_hazard_penalty).clamp(0.0, 100.0);

    let mut applied = false;

    if let Some(depth_score) = resp.score_normalized_100("root_cause_quality", 5) {
        blast.root_cause_depth_score = depth_score;
        applied = true;
    }

    if let Some(bandaid_prob) = resp.noul_prob("is_symptom_bandaid") {
        let clamped = (bandaid_prob as f32).clamp(0.0, 1.0);
        blast.bandaid_probability = Some(clamped);
        if bandaid_prob >= BANDAID_WARNING_THRESHOLD && blast.bandaid_debt_warning.is_none() {
            blast.bandaid_debt_warning = Some(format!(
                "Calibrated Decisions audit flagged likely symptom-only bandaid ({:.0}% probability).",
                (bandaid_prob * 100.0).clamp(0.0, 100.0)
            ));
        }
        applied = true;
    }

    if let Some(reg_prob) = resp.noul_prob("regression_hazard") {
        applied = true;
        let severity = if reg_prob >= HIGH_REGRESSION_HAZARD_THRESHOLD {
            Some(HazardSeverity::High)
        } else if reg_prob >= MEDIUM_REGRESSION_HAZARD_THRESHOLD {
            Some(HazardSeverity::Medium)
        } else {
            None
        };
        if let Some(severity) = severity {
            let symbol = blast
                .ring0_mutated
                .first()
                .map(|n| n.symbol.clone())
                .unwrap_or_else(|| "diff".to_string());
            blast.semantic_hazards.push(SemanticHazard {
                severity,
                category: "calibrated_regression_hazard".to_string(),
                symbol,
                explanation: format!(
                    "gemini-decision-1 estimated {:.0}% regression hazard across impacted callers (Ring 1: {}, Ring 2: {}).",
                    (reg_prob * 100.0).clamp(0.0, 100.0),
                    blast.ring1_direct_callers.len(),
                    blast.ring2_transitive_entrypoints.len(),
                ),
                mitigation: "Add targeted caller regression tests covering Ring 1/Ring 2 entrypoints and verify contract invariants.".to_string(),
            });
        }
    }

    let confidences: Vec<f64> = ["root_cause_quality", "is_symptom_bandaid", "regression_hazard"]
        .into_iter()
        .filter_map(|k| resp.confidence(k))
        .collect();
    if !confidences.is_empty() {
        let mean_conf = confidences.iter().sum::<f64>() / confidences.len() as f64;
        blast.decision_confidence = Some((mean_conf as f32).clamp(0.0, 1.0));
        applied = true;
    }

    if applied {
        blast.recalculate_composite_risk(base_score);
    }

    applied
}

/// Calibrates a structural `BlastRadiusReport` using `gemini-decision-1` (bounded by a 12s timeout).
///
/// Graceful no-op returning `false` if `gemini` is `None` or the Decisions API call fails/times out.
pub async fn calibrate_blast_radius_with_decisions(
    gemini: Option<&GeminiClient>,
    problem_statement: &str,
    candidate_diff: &str,
    blast: &mut BlastRadiusReport,
) -> bool {
    let Some(client) = gemini else {
        return false;
    };
    let req = build_blast_calibration_request(problem_statement, candidate_diff, blast);
    match tokio::time::timeout(
        Duration::from_secs(DECISIONS_CALIBRATION_TIMEOUT_SECS),
        client.evaluate_decisions(&req),
    )
    .await
    {
        Ok(Ok(resp)) => apply_decision_calibration_to_blast_radius(&resp, blast),
        Ok(Err(err)) => {
            warn!(%err, "gemini-decision-1 blast radius calibration failed (graceful no-op)");
            false
        }
        Err(_) => {
            warn!(
                timeout_secs = DECISIONS_CALIBRATION_TIMEOUT_SECS,
                "gemini-decision-1 blast radius calibration timed out (graceful no-op)"
            );
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use epicenter_common::ImpactRing;
    use epicenter_gemini::DecisionAnswer;

    fn sample_blast(base_risk: f32) -> BlastRadiusReport {
        BlastRadiusReport {
            ring0_mutated: vec![GraphNode {
                id: "n0".to_string(),
                symbol: "parse_header".to_string(),
                file_path: "src/parser.py".to_string(),
                line: 42,
                ring: ImpactRing::Ring0,
                test_coverage_pct: Some(85.0),
                is_untested_dark_zone: false,
                radial_x: 0.0,
                radial_y: 0.0,
            }],
            ring1_direct_callers: vec![GraphNode {
                id: "n1".to_string(),
                symbol: "handle_request".to_string(),
                file_path: "src/server.py".to_string(),
                line: 118,
                ring: ImpactRing::Ring1,
                test_coverage_pct: None,
                is_untested_dark_zone: true,
                radial_x: 1.0,
                radial_y: 0.0,
            }],
            ring2_transitive_entrypoints: Vec::new(),
            edges: Vec::new(),
            semantic_hazards: Vec::new(),
            composite_risk_score: base_risk,
            root_cause_depth_score: 50.0,
            bandaid_debt_warning: None,
            bandaid_probability: None,
            decision_confidence: None,
        }
    }

    fn decision_resp(
        root_cause_score: f64,
        bandaid_prob: f64,
        regression_prob: f64,
        confs: (f64, f64, f64),
    ) -> DecisionResponse {
        let mut answers = BTreeMap::new();
        answers.insert(
            "root_cause_quality".to_string(),
            DecisionAnswer::Score {
                score: root_cause_score,
                legend: BTreeMap::new(),
                probabilities: BTreeMap::new(),
                confidence: confs.0,
            },
        );
        answers.insert(
            "is_symptom_bandaid".to_string(),
            DecisionAnswer::Noul {
                noul: bandaid_prob,
                confidence: confs.1,
            },
        );
        answers.insert(
            "regression_hazard".to_string(),
            DecisionAnswer::Noul {
                noul: regression_prob,
                confidence: confs.2,
            },
        );
        DecisionResponse {
            answers,
            usage: None,
        }
    }

    #[test]
    fn calibrates_high_root_cause_clean_patch_without_regression_hazard() {
        let mut blast = sample_blast(18.0);
        let resp = decision_resp(3.6, 0.08, 0.22, (0.90, 0.92, 0.88));

        assert!(apply_decision_calibration_to_blast_radius(&resp, &mut blast));
        assert!((blast.root_cause_depth_score - 90.0).abs() < 0.01);
        assert!((blast.bandaid_probability.unwrap() - 0.08).abs() < 0.001);
        assert!((blast.decision_confidence.unwrap() - 0.90).abs() < 0.001);
        assert!(blast.bandaid_debt_warning.is_none());
        assert!(blast.semantic_hazards.is_empty());
        assert!((blast.composite_risk_score - 18.0).abs() < 0.01);
    }

    #[test]
    fn calibrates_bandaid_patch_with_high_regression_hazard_and_recalculates_risk() {
        let mut blast = sample_blast(22.5);
        let resp = decision_resp(0.8, 0.87, 0.84, (0.85, 0.91, 0.82));

        assert!(apply_decision_calibration_to_blast_radius(&resp, &mut blast));
        assert!((blast.root_cause_depth_score - 20.0).abs() < 0.01);
        assert!((blast.bandaid_probability.unwrap() - 0.87).abs() < 0.001);
        assert!((blast.decision_confidence.unwrap() - 0.86).abs() < 0.001);
        assert!(blast.bandaid_debt_warning.is_some());
        assert_eq!(blast.semantic_hazards.len(), 1);
        assert_eq!(blast.semantic_hazards[0].severity, HazardSeverity::High);
        assert_eq!(blast.semantic_hazards[0].symbol, "parse_header");
        // Base 22.5 + High hazard weight (20.0) = 42.5
        assert!((blast.composite_risk_score - 42.5).abs() < 0.01);
    }

    #[test]
    fn calibrates_medium_regression_hazard_threshold_accurately() {
        let mut blast = sample_blast(15.0);
        let resp = decision_resp(2.0, 0.45, 0.65, (0.75, 0.80, 0.70));

        assert!(apply_decision_calibration_to_blast_radius(&resp, &mut blast));
        assert!((blast.root_cause_depth_score - 50.0).abs() < 0.01);
        assert_eq!(blast.semantic_hazards.len(), 1);
        assert_eq!(blast.semantic_hazards[0].severity, HazardSeverity::Medium);
        // Base 15.0 + Medium hazard weight (10.0) = 25.0
        assert!((blast.composite_risk_score - 25.0).abs() < 0.01);
    }

    #[test]
    fn noop_calibration_when_decision_response_is_empty() {
        let mut blast = sample_blast(14.0);
        let empty = DecisionResponse::default();
        assert!(!apply_decision_calibration_to_blast_radius(&empty, &mut blast));
        assert!((blast.root_cause_depth_score - 50.0).abs() < 0.01);
        assert!(blast.bandaid_probability.is_none());
        assert!(blast.decision_confidence.is_none());
        assert!(blast.semantic_hazards.is_empty());
        assert!((blast.composite_risk_score - 14.0).abs() < 0.01);
    }

    #[test]
    fn preflight_spec_clarity_appends_note_only_to_failed_arms_when_below_threshold() {
        let req = build_pr_spec_clarity_request(
            "Fix bug",
            "",
            &["src/parser.py".to_string()],
        );
        assert_eq!(req.model, epicenter_common::GEMINI_DECISION_MODEL);
        assert!(req.questions.contains_key("has_actionable_spec"));
        assert!(req.questions.contains_key("is_self_contained"));

        let mut answers = BTreeMap::new();
        answers.insert(
            "has_actionable_spec".to_string(),
            DecisionAnswer::Noul {
                noul: 0.18,
                confidence: 0.88,
            },
        );
        answers.insert(
            "is_self_contained".to_string(),
            DecisionAnswer::Noul {
                noul: 0.82,
                confidence: 0.80,
            },
        );
        let clarity = parse_pr_spec_clarity_response(&DecisionResponse {
            answers,
            usage: None,
        })
        .expect("should parse spec clarity signal");
        assert!((clarity.has_actionable_spec - 0.18).abs() < 0.001);
        assert!((clarity.is_self_contained - 0.82).abs() < 0.001);

        let mut outcomes = vec![
            ArmOutcome {
                case_id: "c1".to_string(),
                arm: EvalArm::SingleModelCall,
                resolved_ground_truth: false,
                regression_free: true,
                speedup_pct: None,
                blast_risk_score: None,
                root_cause_depth_score: None,
                dark_zone_touches: None,
                duration_ms: 100,
                notes: Some("FAILED host ground-truth tests".to_string()),
                failure_category: Some("ground_truth_tests_failed".to_string()),
            },
            ArmOutcome {
                case_id: "c1".to_string(),
                arm: EvalArm::SingleManagedAgent,
                resolved_ground_truth: false,
                regression_free: false,
                speedup_pct: None,
                blast_risk_score: None,
                root_cause_depth_score: None,
                dark_zone_touches: None,
                duration_ms: 200,
                notes: None,
                failure_category: Some("patch_apply_conflict".to_string()),
            },
            ArmOutcome {
                case_id: "c1".to_string(),
                arm: EvalArm::EpicenterTournament,
                resolved_ground_truth: true,
                regression_free: true,
                speedup_pct: None,
                blast_risk_score: Some(12.0),
                root_cause_depth_score: Some(88.0),
                dark_zone_touches: Some(0),
                duration_ms: 300,
                notes: Some("Winner 's1'".to_string()),
                failure_category: None,
            },
        ];

        apply_spec_clarity_note_to_failed_arms(&mut outcomes, &clarity);
        assert_eq!(
            outcomes[0].notes.as_deref(),
            Some("FAILED host ground-truth tests | Low PR spec clarity (18%)")
        );
        assert_eq!(
            outcomes[1].notes.as_deref(),
            Some("Low PR spec clarity (18%)")
        );
        assert_eq!(outcomes[2].notes.as_deref(), Some("Winner 's1'"));

        // High clarity (>= 0.30) must not modify failed arms.
        let high_clarity = PrSpecClaritySignal {
            has_actionable_spec: 0.75,
            is_self_contained: 0.90,
            confidence: Some(0.91),
        };
        let mut failed_only = vec![ArmOutcome {
            case_id: "c2".to_string(),
            arm: EvalArm::SingleModelCall,
            resolved_ground_truth: false,
            regression_free: true,
            speedup_pct: None,
            blast_risk_score: None,
            root_cause_depth_score: None,
            dark_zone_touches: None,
            duration_ms: 100,
            notes: Some("original".to_string()),
            failure_category: Some("ground_truth_tests_failed".to_string()),
        }];
        apply_spec_clarity_note_to_failed_arms(&mut failed_only, &high_clarity);
        assert_eq!(failed_only[0].notes.as_deref(), Some("original"));
    }

    #[tokio::test]
    async fn calibrate_blast_radius_with_decisions_is_noop_when_gemini_is_none() {
        let mut blast = sample_blast(19.5);
        let calibrated = calibrate_blast_radius_with_decisions(
            None,
            "Fix empty header crash",
            "diff --git a/src/parser.py b/src/parser.py",
            &mut blast,
        )
        .await;
        assert!(!calibrated);
        assert!((blast.root_cause_depth_score - 50.0).abs() < 0.01);
        assert!((blast.composite_risk_score - 19.5).abs() < 0.01);
    }
}
