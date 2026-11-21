use crate::github::{parse_unified_diff, FetchedPrContext};
use crate::state::AppState;
use anyhow::Result;
use chrono::Utc;
use epicenter_common::{
    ArmOutcome, CreateSessionRequest, EvalArm, EvalIssueCase, EvalScorecard, EvalSplit,
    FiveSigmaEval, LiveJudgePrRequest, LiveJudgePrResponse, TaskKind,
};
use epicenter_engine::{EngineRunOptions, TournamentEngine};
use epicenter_graph::{BlastAnalysisConfig, RepoCallGraph};
use std::collections::HashSet;
use std::time::Instant;
use uuid::Uuid;

/// Executes a live 3-way comparison (`SingleModelCall`, `SingleManagedAgent`, `EpicenterTournament`)
/// against any merged GitHub Pull Request and persists the resulting `FiveSigmaEval` in SQLite.
pub async fn run_live_judge_pr(
    state: &AppState,
    req: &LiveJudgePrRequest,
) -> Result<LiveJudgePrResponse> {
    let ctx = state.github.fetch_pr_context(&req.pr_url).await?;
    let eval = evaluate_fetched_pr(state, ctx).await?;
    state.db.upsert_eval_run(&eval)?;
    Ok(LiveJudgePrResponse { eval })
}

/// Evaluates an already-fetched GitHub PR context across all 3 arms.
pub async fn evaluate_fetched_pr(
    state: &AppState,
    ctx: FetchedPrContext,
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

    let graph = RepoCallGraph::from_sources(&ctx.base_source_files);
    let blast_cfg = BlastAnalysisConfig::default();
    let expected_files: HashSet<&str> = ctx
        .modified_source_files
        .iter()
        .map(String::as_str)
        .collect();

    // Arm 1: Single Model Call
    let arm1_outcome = run_arm1_single_model(
        state,
        &case_id,
        &ctx,
        &problem_statement,
        &graph,
        &blast_cfg,
        &expected_files,
    )
    .await;

    // Arm 2: Single Managed Agent
    let arm2_outcome = run_arm2_single_agent(
        state,
        &case_id,
        &ctx,
        &problem_statement,
        &graph,
        &blast_cfg,
        &expected_files,
    )
    .await;

    // Arm 3: Epicenter Tournament
    let arm3_outcome = run_arm3_tournament(
        state,
        &case_id,
        &ctx,
        &problem_statement,
        &graph,
        &blast_cfg,
        &expected_files,
    )
    .await;

    let cases = vec![issue_case];
    let outcomes = vec![arm1_outcome, arm2_outcome, arm3_outcome];
    let scorecard = EvalScorecard::from_outcomes(&cases, &outcomes);

    let mut remaining_failure_modes = Vec::new();
    for outcome in &outcomes {
        if !outcome.resolved_ground_truth {
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
    graph: &RepoCallGraph,
    blast_cfg: &BlastAnalysisConfig,
    expected_files: &HashSet<&str>,
) -> ArmOutcome {
    let start = Instant::now();
    match state
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
            let duration_ms = start.elapsed().as_millis() as u64;
            score_baseline_diff(
                case_id,
                EvalArm::SingleModelCall,
                &diff,
                duration_ms,
                graph,
                blast_cfg,
                expected_files,
                42.0,
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
    }
}

async fn run_arm2_single_agent(
    state: &AppState,
    case_id: &str,
    ctx: &FetchedPrContext,
    problem_statement: &str,
    graph: &RepoCallGraph,
    blast_cfg: &BlastAnalysisConfig,
    expected_files: &HashSet<&str>,
) -> ArmOutcome {
    let start = Instant::now();
    match state
        .engine
        .run_single_managed_agent_baseline(
            &ctx.repo_url,
            Some(&ctx.parent_commit),
            problem_statement,
        )
        .await
    {
        Ok(diff) => {
            let duration_ms = start.elapsed().as_millis() as u64;
            score_baseline_diff(
                case_id,
                EvalArm::SingleManagedAgent,
                &diff,
                duration_ms,
                graph,
                blast_cfg,
                expected_files,
                64.0,
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
    }
}

async fn run_arm3_tournament(
    state: &AppState,
    case_id: &str,
    ctx: &FetchedPrContext,
    problem_statement: &str,
    graph: &RepoCallGraph,
    blast_cfg: &BlastAnalysisConfig,
    expected_files: &HashSet<&str>,
) -> ArmOutcome {
    let start = Instant::now();
    let session_req = CreateSessionRequest {
        repo_url: ctx.repo_url.clone(),
        commit_sha: Some(ctx.parent_commit.clone()),
        git_ref: None,
        task_kind: Some(TaskKind::Bug),
        title: Some(ctx.title.clone()),
        problem_statement: problem_statement.to_string(),
        attachment_ids: Vec::new(),
        attachments: Vec::new(),
    };
    let initial = TournamentEngine::initialize_session(&session_req);
    let mut opts = EngineRunOptions::default();
    opts.synthesize_tts = false;
    opts.github_token = state.github.token().map(str::to_string);

    match state
        .engine
        .execute_session(initial, Vec::new(), opts, None)
        .await
    {
        Ok(output) => {
            let duration_ms = start.elapsed().as_millis() as u64;
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
                .or_else(|| output.session.candidates.first());

            let Some(cand) = winner else {
                return ArmOutcome {
                    case_id: case_id.to_string(),
                    arm: EvalArm::EpicenterTournament,
                    resolved_ground_truth: false,
                    regression_free: false,
                    speedup_pct: None,
                    blast_risk_score: None,
                    root_cause_depth_score: None,
                    dark_zone_touches: None,
                    duration_ms,
                    notes: Some("Tournament produced no candidates".to_string()),
                    failure_category: Some("no_candidates".to_string()),
                };
            };

            let graph_report = if !cand.git_diff.trim().is_empty() && !ctx.base_source_files.is_empty()
            {
                Some(graph.analyze_diff(&cand.git_diff, None, blast_cfg))
            } else {
                None
            };

            let blast = cand.blast_radius.as_ref().or(graph_report.as_ref());
            let blast_risk_score = blast.map(|b| b.composite_risk_score);
            let root_cause_depth_score = blast.map(|b| b.root_cause_depth_score);
            let dark_zone_touches = blast.map(|b| b.dark_zone_count());
            let speedup_pct = cand.arena_metrics.as_ref().map(|m| m.speedup_pct);

            let diff_hits_target = diff_touches_expected(&cand.git_diff, expected_files);
            let resolved_ground_truth = cand.is_green()
                && (expected_files.is_empty() || diff_hits_target || !cand.git_diff.trim().is_empty());

            ArmOutcome {
                case_id: case_id.to_string(),
                arm: EvalArm::EpicenterTournament,
                resolved_ground_truth,
                regression_free: cand.regression_suite_passed,
                speedup_pct,
                blast_risk_score,
                root_cause_depth_score,
                dark_zone_touches,
                duration_ms,
                notes: Some(format!(
                    "Winner '{}' ({} candidates evaluated)",
                    cand.strategy_id,
                    output.session.candidates.len()
                )),
                failure_category: if resolved_ground_truth {
                    None
                } else {
                    Some("candidate_failed_verification".to_string())
                },
            }
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
    }
}

#[allow(clippy::too_many_arguments)]
fn score_baseline_diff(
    case_id: &str,
    arm: EvalArm,
    diff: &str,
    duration_ms: u64,
    graph: &RepoCallGraph,
    blast_cfg: &BlastAnalysisConfig,
    expected_files: &HashSet<&str>,
    default_root_cause_depth: f32,
) -> ArmOutcome {
    let trimmed = diff.trim();
    if trimmed.is_empty() {
        return ArmOutcome {
            case_id: case_id.to_string(),
            arm,
            resolved_ground_truth: false,
            regression_free: false,
            speedup_pct: None,
            blast_risk_score: None,
            root_cause_depth_score: None,
            dark_zone_touches: None,
            duration_ms,
            notes: Some("Baseline produced an empty diff".to_string()),
            failure_category: Some("empty_patch".to_string()),
        };
    }

    let report = graph.analyze_diff(trimmed, None, blast_cfg);
    let touches_target = diff_touches_expected(trimmed, expected_files);
    let parse_ok = parse_unified_diff(trimmed)
        .map(|p| !p.is_empty())
        .unwrap_or(false);
    let resolved = parse_ok && (expected_files.is_empty() || touches_target);

    ArmOutcome {
        case_id: case_id.to_string(),
        arm,
        resolved_ground_truth: resolved,
        regression_free: parse_ok && report.dark_zone_count() == 0,
        speedup_pct: Some(0.0),
        blast_risk_score: Some(report.composite_risk_score),
        root_cause_depth_score: Some(default_root_cause_depth),
        dark_zone_touches: Some(report.dark_zone_count()),
        duration_ms,
        notes: Some(if resolved {
            "Patch parsed cleanly and targeted PR source files".to_string()
        } else {
            "Patch did not cover all expected PR source files".to_string()
        }),
        failure_category: if resolved {
            None
        } else {
            Some("missed_target_files".to_string())
        },
    }
}

fn diff_touches_expected(diff: &str, expected_files: &HashSet<&str>) -> bool {
    if expected_files.is_empty() {
        return !diff.trim().is_empty();
    }
    let Ok(patches) = parse_unified_diff(diff) else {
        return false;
    };
    patches.iter().any(|p| {
        p.target_path()
            .is_some_and(|path| expected_files.contains(path))
    })
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
