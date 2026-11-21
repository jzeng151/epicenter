use crate::repo::{
    default_sandbox_allowlist, diff_adds_caller_tests, emit_event, emit_log,
    extract_json_from_text, extract_unified_diff_from_text, normalize_clone_url,
    strip_locked_test_from_diff, truncate_str, RepoSnapshot,
};
use epicenter_common::{
    CandidateResult, CandidateStatus, Gemini4Model, LiveEvent, LogLevel, RedGateSpec,
    StrategyDepthTier, StrategyPlan, StrategySpec, TournamentSession, TournamentStage,
};
use epicenter_gemini::{
    CreateInteractionRequest, GeminiClient, InteractionEnvironment, ResponseFormat,
};
use serde::Deserialize;
use serde_json::json;
use std::collections::BTreeMap;
use std::time::Duration;
use tokio::sync::broadcast;
use tracing::warn;

const WORKER_POLL_INTERVAL: Duration = Duration::from_secs(3);
const WORKER_TIMEOUT: Duration = Duration::from_secs(720);

#[derive(Debug, Deserialize)]
struct WorkerOutput {
    #[serde(default)]
    git_diff: String,
    #[serde(default)]
    repro_passed: bool,
    #[serde(default)]
    regression_suite_passed: bool,
    #[serde(default)]
    test_output_tail: String,
    #[serde(default)]
    added_caller_tests: bool,
    #[serde(default)]
    coverage_report: Option<String>,
}

/// Output of Stage 4 including optional per-candidate raw coverage payloads captured in worker sandboxes.
#[derive(Debug, Default)]
pub struct WorkersStageResult {
    pub worker_coverage_raw: BTreeMap<String, String>,
}

/// Stage 4: Parallel `Neon` Managed Agent Workers + `0/K` Carbon Rescue Loop.
///
/// Spawns all `K` strategies concurrently (`futures::future::join_all`) with up to 4 internal
/// edit-and-test loops inside each sandbox, strips/rejects any diff hunks touching
/// `red_gate.locked_test_path()`, preserves failed candidates in `session.candidates`, and triggers
/// a single Carbon-designed rescue strategy + Neon sandbox if `0` of `K` candidates pass.
pub async fn run_workers_stage(
    gemini: &GeminiClient,
    session: &mut TournamentSession,
    worker_agent_id: &str,
    red_gate: &RedGateSpec,
    plan: &mut StrategyPlan,
    repo: &RepoSnapshot,
    event_tx: &Option<broadcast::Sender<LiveEvent>>,
) -> anyhow::Result<WorkersStageResult> {
    session.stage = TournamentStage::Executing;
    session.updated_at = chrono::Utc::now();
    emit_event(
        event_tx,
        LiveEvent::StageChanged {
            session_id: session.id,
            stage: TournamentStage::Executing,
            message: format!(
                "Stage 4 — Spawning {} parallel Neon sandboxes (4 internal edit-test loops each)",
                plan.strategies.len()
            ),
        },
    );

    // Pre-populate pending candidates so the UI shows all K lanes immediately.
    session.candidates = plan
        .strategies
        .iter()
        .map(|s| CandidateResult::pending(s.id.clone()))
        .collect();

    for strat in &plan.strategies {
        emit_event(
            event_tx,
            LiveEvent::CandidateStatusChanged {
                session_id: session.id,
                strategy_id: strat.id.clone(),
                status: CandidateStatus::Running,
                detail: format!("Launching Neon sandbox for '{}'", strat.title),
            },
        );
    }

    let futures_vec = plan.strategies.iter().map(|strat| {
        execute_single_worker(
            gemini,
            session.id,
            &session.intake.repo_url,
            session.intake.commit_sha.as_deref(),
            session.intake.git_ref.as_deref(),
            &session.intake.title,
            &session.intake.problem_statement,
            worker_agent_id,
            red_gate,
            strat,
            false,
            event_tx,
        )
    });

    let results = futures::future::join_all(futures_vec).await;
    let mut coverage_raw = BTreeMap::new();
    let mut updated_candidates = Vec::with_capacity(results.len());

    for (cand, cov_opt) in results {
        if let Some(cov) = cov_opt {
            if !cov.trim().is_empty() {
                coverage_raw.insert(cand.strategy_id.clone(), cov);
            }
        }
        updated_candidates.push(cand);
    }

    session.candidates = updated_candidates;
    session.updated_at = chrono::Utc::now();

    // 0/K Rescue: if zero strategies passed both repro & regression tests, ask Carbon for 1 rescue strategy
    let green_count = session.candidates.iter().filter(|c| c.is_green()).count();
    if green_count == 0 && !session.candidates.is_empty() {
        emit_log(
            event_tx,
            session.id,
            LogLevel::Warn,
            "workers",
            format!(
                "0/{} candidates passed tests; invoking Gemini 4 Carbon for 1 rescue strategy",
                session.candidates.len()
            ),
        );

        let rescue_spec = design_carbon_rescue_strategy(
            gemini,
            session,
            red_gate,
            repo,
            &session.candidates,
        )
        .await;

        plan.strategies.push(rescue_spec.clone());
        session.strategy_plan = Some(plan.clone());
        emit_event(
            event_tx,
            LiveEvent::StrategyPlanReady {
                session_id: session.id,
                plan: Box::new(plan.clone()),
            },
        );
        emit_event(
            event_tx,
            LiveEvent::CandidateStatusChanged {
                session_id: session.id,
                strategy_id: rescue_spec.id.clone(),
                status: CandidateStatus::Running,
                detail: format!("Launching 0/K Rescue Neon sandbox for '{}'", rescue_spec.title),
            },
        );

        let (rescue_cand, rescue_cov) = execute_single_worker(
            gemini,
            session.id,
            &session.intake.repo_url,
            session.intake.commit_sha.as_deref(),
            session.intake.git_ref.as_deref(),
            &session.intake.title,
            &session.intake.problem_statement,
            worker_agent_id,
            red_gate,
            &rescue_spec,
            true,
            event_tx,
        )
        .await;

        if let Some(cov) = rescue_cov {
            if !cov.trim().is_empty() {
                coverage_raw.insert(rescue_cand.strategy_id.clone(), cov);
            }
        }
        session.candidates.push(rescue_cand);
        session.updated_at = chrono::Utc::now();
    }

    Ok(WorkersStageResult {
        worker_coverage_raw: coverage_raw,
    })
}

#[allow(clippy::too_many_arguments)]
async fn execute_single_worker(
    gemini: &GeminiClient,
    session_id: uuid::Uuid,
    repo_url: &str,
    commit_sha: Option<&str>,
    git_ref: Option<&str>,
    issue_title: &str,
    problem_statement: &str,
    worker_agent_id: &str,
    red_gate: &RedGateSpec,
    strategy: &StrategySpec,
    is_rescue_candidate: bool,
    event_tx: &Option<broadcast::Sender<LiveEvent>>,
) -> (CandidateResult, Option<String>) {
    let clone_url = normalize_clone_url(repo_url);
    let locked_path = red_gate.locked_test_path();

    let checkout_step = match (commit_sha, git_ref) {
        (Some(sha), _) if !sha.trim().is_empty() => format!("Run `git checkout {}` first.", sha.trim()),
        (_, Some(r)) if !r.trim().is_empty() => format!("Run `git checkout {}` first.", r.trim()),
        _ => "Start from the checked-out HEAD in /workspace/repo.".to_string(),
    };

    let caller_test_rule = if strategy.allow_caller_tests {
        "CALLER TESTS ALLOWED: You are encouraged to add targeted regression tests for Ring 1 callers (in a separate test file, NEVER modifying the locked repro test) to eliminate untested dark zones."
    } else {
        "Keep tests minimal: do NOT modify unrelated test suites."
    };

    let locked_rule = match locked_path {
        Some(path) => format!(
            "IMMUTABLE LOCKED RED-GATE FILE: `{path}`. Apply the Red-Gate patch below first, then NEVER edit, weaken, skip, or delete `{path}`! Any diff touching `{path}` will be automatically stripped and rejected."
        ),
        None => "Apply the Red-Gate acceptance patch below first and NEVER weaken its assertions.".to_string(),
    };

    let prompt = format!(
        "You are an autonomous Gemini 4 Neon worker inside /workspace/repo.\n\
         {checkout_step}\n\
         {locked_rule}\n\
         {caller_test_rule}\n\n\
         STRATEGY ASSIGNMENT:\n\
         - Strategy ID: {id}\n\
         - Title: {title}\n\
         - Depth Tier: {tier:?}\n\
         - Thesis: {thesis}\n\
         - Tradeoff Axis: {axis}\n\
         - Target Files Hint: {targets:?}\n\
         - Anti-Overlap Rules: {anti:?}\n\n\
         ISSUE:\n\
         Title: {issue_title}\n\
         Problem Statement:\n{problem_statement}\n\n\
         LOCKED RED-GATE TEST COMMAND: {test_cmd}\n\
         LOCKED RED-GATE PATCH:\n```diff\n{red_patch}\n```\n\n\
         EXECUTION PROTOCOL (up to 4 internal edit-and-test loops):\n\
         1. Apply the locked Red-Gate patch (if non-empty) and confirm baseline status.\n\
         2. Implement ONLY your assigned strategy cleanly.\n\
         3. Run `{test_cmd}` AND the repository's existing regression test suite.\n\
         4. If tests fail, diagnose and iterate up to 4 edit-and-test loops inside this sandbox.\n\
         5. Before exporting `git_diff`, exclude the locked Red-Gate test path (`git diff -- . ':(exclude){exclude}'` if applicable) so `git_diff` contains ONLY your fix (and optional caller tests if allowed).\n\
         6. Optionally run coverage (`pytest --cov` / `coverage json` / `lcov`) if fast.\n\n\
         Return STRICT JSON ONLY:\n\
         {{\n\
           \"git_diff\": \"diff --git a/... b/...\\n...\",\n\
           \"repro_passed\": true,\n\
           \"regression_suite_passed\": true,\n\
           \"test_output_tail\": \"last ~40 lines of test output\",\n\
           \"added_caller_tests\": false,\n\
           \"coverage_report\": null\n\
         }}",
        id = strategy.id,
        title = strategy.title,
        tier = strategy.depth_tier,
        thesis = strategy.thesis,
        axis = strategy.tradeoff_axis,
        targets = strategy.target_files_hint,
        anti = strategy.anti_overlap_rules,
        test_cmd = red_gate.test_command(),
        red_patch = truncate_str(red_gate.patch(), 8_000),
        exclude = locked_path.unwrap_or(""),
    );

    let mut req = CreateInteractionRequest::for_agent(worker_agent_id, prompt);
    req.environment = Some(InteractionEnvironment::remote_repo(
        clone_url,
        git_ref.map(String::from),
        commit_sha.map(String::from),
        default_sandbox_allowlist(),
    ));
    req.background = Some(true);

    let mut cand = CandidateResult::pending(strategy.id.clone());
    cand.is_rescue_candidate = is_rescue_candidate;

    let mut cov_raw: Option<String> = None;

    match gemini
        .run_background_interaction(req, WORKER_POLL_INTERVAL, WORKER_TIMEOUT)
        .await
    {
        Ok(resp) => {
            let text = resp.extract_text();
            match extract_json_from_text::<WorkerOutput>(&text) {
                Ok(out) => {
                    let mut diff = if out.git_diff.trim().is_empty() {
                        extract_unified_diff_from_text(&text)
                    } else {
                        out.git_diff
                    };

                    let mut tampered = false;
                    if let Some(lp) = locked_path {
                        let (cleaned, stripped) = strip_locked_test_from_diff(&diff, lp);
                        if stripped > 0 {
                            tampered = true;
                            diff = cleaned;
                            emit_log(
                                event_tx,
                                session_id,
                                LogLevel::Warn,
                                format!("worker:{}", strategy.id),
                                format!(
                                    "Stripped {stripped} diff hunk(s) touching locked Red-Gate test '{lp}'"
                                ),
                            );
                        }
                    }

                    cand.git_diff = diff;
                    let has_patch = !cand.git_diff.trim().is_empty();
                    cand.repro_passed = out.repro_passed && has_patch && !tampered;
                    cand.regression_suite_passed =
                        out.regression_suite_passed && has_patch && !tampered;
                    cand.test_output_tail = if tampered {
                        format!(
                            "[REJECTED: attempted edit to locked Red-Gate test]\n{}",
                            truncate_str(&out.test_output_tail, 1500)
                        )
                    } else {
                        truncate_str(&out.test_output_tail, 2000)
                    };
                    cand.added_caller_tests =
                        out.added_caller_tests || diff_adds_caller_tests(&cand.git_diff, locked_path);
                    cand.status = if cand.is_green() {
                        CandidateStatus::PassedTests
                    } else {
                        CandidateStatus::FailedTests
                    };
                    cov_raw = out.coverage_report;
                }
                Err(err) => {
                    warn!(strategy = %strategy.id, error = %err, "worker response JSON parse failed");
                    let recovered_diff = extract_unified_diff_from_text(&text);
                    if let Some(lp) = locked_path {
                        let (cleaned, _) = strip_locked_test_from_diff(&recovered_diff, lp);
                        cand.git_diff = cleaned;
                    } else {
                        cand.git_diff = recovered_diff;
                    }
                    cand.status = CandidateStatus::FailedTests;
                    cand.test_output_tail = format!(
                        "Worker output parse error ({err}): {}",
                        truncate_str(&text, 1000)
                    );
                }
            }
        }
        Err(err) => {
            warn!(strategy = %strategy.id, error = %err, "Neon worker sandbox failed");
            cand.status = CandidateStatus::FailedTests;
            cand.test_output_tail = format!("Sandbox execution error: {err}");
        }
    }

    let detail = if cand.is_green() {
        format!(
            "Passed locked Red-Gate + regression suite (caller_tests={})",
            cand.added_caller_tests
        )
    } else {
        format!(
            "Failed verification (repro={}, regression={})",
            cand.repro_passed, cand.regression_suite_passed
        )
    };

    emit_event(
        event_tx,
        LiveEvent::CandidateStatusChanged {
            session_id,
            strategy_id: cand.strategy_id.clone(),
            status: cand.status,
            detail,
        },
    );
    emit_event(
        event_tx,
        LiveEvent::CandidateUpdated {
            session_id,
            candidate: Box::new(cand.clone()),
        },
    );

    (cand, cov_raw)
}

async fn design_carbon_rescue_strategy(
    gemini: &GeminiClient,
    session: &TournamentSession,
    red_gate: &RedGateSpec,
    repo: &RepoSnapshot,
    failed_candidates: &[CandidateResult],
) -> StrategySpec {
    let failure_digest = failed_candidates
        .iter()
        .map(|c| {
            format!(
                "Strategy `{}` (repro={}, regression={}):\nTail:\n{}\nDiff snippet:\n{}\n",
                c.strategy_id,
                c.repro_passed,
                c.regression_suite_passed,
                truncate_str(&c.test_output_tail, 700),
                truncate_str(&c.git_diff, 700),
            )
        })
        .collect::<Vec<_>>()
        .join("\n---\n");

    let prompt = format!(
        "All {} initial tournament strategies failed verification on '{}'.\n\
         Diagnose WHY every attempt failed from the test logs below and design ONE surgical,\n\
         high-conviction rescue StrategySpec (`id: \"s-rescue-carbon\"`) that directly fixes the\n\
         root cause without repeating the failed mistakes.\n\n\
         Problem Statement:\n{}\n\
         Red-Gate Command: {}\n\n\
         Failed Candidate Logs:\n{}\n\n\
         Repository Sources:\n{}\n\n\
         Return STRICT JSON matching the StrategySpec schema.",
        failed_candidates.len(),
        session.intake.title,
        session.intake.problem_statement,
        red_gate.test_command(),
        failure_digest,
        repo.prompt_source_context(&session.intake.suspect_files, 24_000),
    );

    let mut req = CreateInteractionRequest::for_model(Gemini4Model::Carbon, prompt);
    req.system_instruction = Some(
        "You are Gemini 4 Carbon performing a 0/K rescue post-mortem and authoring 1 rescue StrategySpec. Return strict JSON."
            .to_string(),
    );
    req.response_format = Some(ResponseFormat::json_schema(rescue_strategy_schema()));

    if let Ok(resp) = gemini.create_interaction(&req).await {
        if let Ok(mut spec) = extract_json_from_text::<StrategySpec>(&resp.extract_text()) {
            if spec.id.trim().is_empty() {
                spec.id = "s-rescue-carbon".to_string();
            }
            spec.allow_caller_tests = true;
            return spec;
        }
    }

    StrategySpec {
        id: "s-rescue-carbon".to_string(),
        title: "Carbon Rescue Root-Cause Fix".to_string(),
        thesis: "Synthesize failure logs from prior candidates to fix the exact failing assertion cleanly without breaking existing callers."
            .to_string(),
        tradeoff_axis: "Verified green pass reliability over speculative optimizations".to_string(),
        target_files_hint: session.intake.suspect_files.clone(),
        risk_hypothesis: "Directly targets root test failure observed in initial wave.".to_string(),
        depth_tier: StrategyDepthTier::ArchitecturalRootCause,
        anti_overlap_rules: vec![
            "Avoid repeating failing patches from initial K candidates.".to_string(),
        ],
        allow_caller_tests: true,
    }
}

fn rescue_strategy_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "id": { "type": "string" },
            "title": { "type": "string" },
            "thesis": { "type": "string" },
            "tradeoff_axis": { "type": "string" },
            "target_files_hint": {
                "type": "array",
                "items": { "type": "string" }
            },
            "risk_hypothesis": { "type": "string" },
            "depth_tier": {
                "type": "string",
                "enum": [
                    "surgical_hotfix",
                    "targeted_refactor",
                    "architectural_root_cause",
                    "algorithmic_optimization"
                ]
            },
            "anti_overlap_rules": {
                "type": "array",
                "items": { "type": "string" }
            },
            "allow_caller_tests": { "type": "boolean" }
        },
        "required": [
            "id",
            "title",
            "thesis",
            "tradeoff_axis",
            "target_files_hint",
            "risk_hypothesis",
            "depth_tier",
            "anti_overlap_rules",
            "allow_caller_tests"
        ]
    })
}
