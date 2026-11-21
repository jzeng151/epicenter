use crate::intake::format_multimodal_findings;
use crate::repo::{
    diff_adds_caller_tests, emit_event, emit_log, extract_json_from_text,
    extract_unified_diff_from_text, truncate_str, RepoSnapshot,
};
use crate::verifier::{FileEdit, NewFileSpec, PreparedRepo};
use epicenter_common::{
    CandidateResult, CandidateStatus, Gemini4Model, LiveEvent, LogLevel, RedGateSpec,
    StrategyDepthTier, StrategyPlan, StrategySpec, TournamentSession, TournamentStage,
};
use epicenter_gemini::{CreateInteractionRequest, GeminiClient, ResponseFormat};
use serde::Deserialize;
use serde_json::json;
use std::time::{Duration, Instant};
use tokio::sync::broadcast;
use tracing::warn;

const MAX_EDIT_TEST_LOOPS: u8 = 3;
const STRATEGY_DEADLINE: Duration = Duration::from_secs(90);

#[derive(Debug, Deserialize)]
struct WorkerEditOutput {
    #[serde(default)]
    edits: Vec<FileEdit>,
    #[serde(default)]
    new_files: Vec<NewFileSpec>,
    #[serde(default)]
    git_diff: String,
    #[serde(default)]
    rationale: String,
}

/// Stage 4: Parallel `Gemini4Model::Neon` Edit-Test Workers + `0/K` Carbon Rescue Loop,
/// 100% independently verified on the host via `PreparedRepo::verify_candidate_patch`.
///
/// Spawns all `K` strategies concurrently (`futures::future::join_all`) with up to 3 fast
/// edit-and-test iterations (bounded by a 90s per-strategy deadline). Zero self-reported
/// test outcomes: every pass/fail, regression verdict, output tail, and anti-tampering check
/// comes strictly from `HostVerifier`.
pub async fn run_workers_stage(
    gemini: &GeminiClient,
    session: &mut TournamentSession,
    prepared: &PreparedRepo,
    red_gate: &RedGateSpec,
    plan: &mut StrategyPlan,
    repo: &RepoSnapshot,
    event_tx: &Option<broadcast::Sender<LiveEvent>>,
) -> anyhow::Result<()> {
    session.stage = TournamentStage::Executing;
    session.updated_at = chrono::Utc::now();
    emit_event(
        event_tx,
        LiveEvent::StageChanged {
            session_id: session.id,
            stage: TournamentStage::Executing,
            message: format!(
                "Stage 4 — Launching {} parallel Neon edit-test workers ({} loops max, host-verified)",
                plan.strategies.len(),
                MAX_EDIT_TEST_LOOPS
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
                detail: format!("Running host-verified Neon loop for '{}'", strat.title),
            },
        );
    }

    let multimodal_block = format_multimodal_findings(&session.intake.multimodal_findings);

    let futures_vec = plan.strategies.iter().map(|strat| {
        execute_single_worker(
            gemini,
            session.id,
            &session.intake.title,
            &session.intake.problem_statement,
            &session.intake.repro_steps,
            &session.intake.suspect_files,
            &multimodal_block,
            prepared,
            repo,
            red_gate,
            strat,
            Gemini4Model::Neon,
            false,
            event_tx,
        )
    });

    let results = futures::future::join_all(futures_vec).await;
    session.candidates = results;
    session.updated_at = chrono::Utc::now();

    // 0/K Rescue: if zero strategies passed independent host verification, invoke Carbon rescue.
    let green_count = session.candidates.iter().filter(|c| c.is_green()).count();
    if green_count == 0 && !session.candidates.is_empty() {
        emit_log(
            event_tx,
            session.id,
            LogLevel::Warn,
            "workers",
            format!(
                "0/{} candidates passed independent host verification; invoking Gemini 4 Carbon for 1 rescue strategy",
                session.candidates.len()
            ),
        );

        let rescue_spec = design_carbon_rescue_strategy(
            gemini,
            session,
            &multimodal_block,
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
                detail: format!(
                    "Running host-verified 0/K Carbon rescue for '{}'",
                    rescue_spec.title
                ),
            },
        );

        let rescue_cand = execute_single_worker(
            gemini,
            session.id,
            &session.intake.title,
            &session.intake.problem_statement,
            &session.intake.repro_steps,
            &session.intake.suspect_files,
            &multimodal_block,
            prepared,
            repo,
            red_gate,
            &rescue_spec,
            Gemini4Model::Carbon,
            true,
            event_tx,
        )
        .await;

        session.candidates.push(rescue_cand);
        session.updated_at = chrono::Utc::now();
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn execute_single_worker(
    gemini: &GeminiClient,
    session_id: uuid::Uuid,
    issue_title: &str,
    problem_statement: &str,
    repro_steps: &[String],
    suspect_files: &[String],
    multimodal_block: &str,
    prepared: &PreparedRepo,
    repo: &RepoSnapshot,
    red_gate: &RedGateSpec,
    strategy: &StrategySpec,
    primary_model: Gemini4Model,
    is_rescue_candidate: bool,
    event_tx: &Option<broadcast::Sender<LiveEvent>>,
) -> CandidateResult {
    let locked_path = red_gate.locked_test_path();
    let deadline = Instant::now() + STRATEGY_DEADLINE;

    let mut combined_hints: Vec<String> = strategy.target_files_hint.clone();
    for s in suspect_files {
        if !combined_hints.contains(s) {
            combined_hints.push(s.clone());
        }
    }
    let source_context = repo.prompt_source_context(&combined_hints, 36_000);

    let caller_test_rule = if strategy.allow_caller_tests {
        "CALLER TESTS ALLOWED: Add targeted regression tests for Ring 1 callers in a separate file via `new_files` (NEVER editing the locked Red-Gate test) to eliminate untested dark zones."
    } else {
        "Keep edits minimal and strictly scoped; do NOT modify unrelated test suites."
    };

    let locked_rule = match locked_path {
        Some(path) => format!(
            "IMMUTABLE LOCKED RED-GATE FILE: `{path}`. NEVER include `{path}` in `edits`, `new_files`, or `git_diff`! The host verifier independently applies `{path}` from server memory and immediately rejects tampering."
        ),
        None => "The host verifier applies the immutable Red-Gate acceptance patch independently; never weaken test assertions.".to_string(),
    };

    let mut cand = CandidateResult::pending(strategy.id.clone());
    cand.is_rescue_candidate = is_rescue_candidate;

    let mut last_feedback = String::new();
    let mut last_diff_snippet = String::new();

    for iter in 1..=MAX_EDIT_TEST_LOOPS {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            emit_log(
                event_tx,
                session_id,
                LogLevel::Warn,
                format!("worker:{}", strategy.id),
                format!("Reached {}s per-strategy deadline before loop {iter}", STRATEGY_DEADLINE.as_secs()),
            );
            break;
        };
        if remaining < Duration::from_secs(3) {
            break;
        }

        // Use primary_model (Neon for regular strategies, Carbon on loop 1 of 0/K rescue then Neon).
        let model = if is_rescue_candidate && iter == 1 {
            primary_model
        } else {
            Gemini4Model::Neon
        };

        let feedback_block = if last_feedback.is_empty() {
            String::new()
        } else {
            format!(
                "\nPREVIOUS ITERATION FAILED INDEPENDENT HOST VERIFICATION (loop {}/{MAX_EDIT_TEST_LOOPS}):\n\
                 Host Failure Diagnostic & Real Test Output:\n{}\n\
                 Previous Patch Snippet:\n```diff\n{}\n```\n\
                 IMPORTANT: Each iteration is applied against CLEAN base HEAD, so return the COMPLETE corrected `edits` (and optional `new_files`) from base HEAD!\n",
                iter - 1,
                truncate_str(&last_feedback, 1500),
                truncate_str(&last_diff_snippet, 1200),
            )
        };

        let prompt = format!(
            "You are a fast Gemini 4 worker implementing ONE assigned strategy in the Epicenter tournament.\n\
             Repository Runtime: {}\n\
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
             Problem Statement:\n{problem_statement}\n\
             Repro Steps: {repro_steps:?}\n\n\
             MULTIMODAL FINDINGS (screenshots / recordings / audio / logs):\n{multimodal_block}\n\n\
             LOCKED RED-GATE TEST COMMAND: {test_cmd}\n\
             LOCKED RED-GATE PATCH (applied automatically by host verifier — DO NOT emit edits to this file):\n\
             ```diff\n{red_patch}\n```\n\
             {feedback_block}\n\
             EXACT BASE-HEAD REPOSITORY SOURCES (copy `old_text` VERBATIM from these files):\n\
             {source_context}\n\n\
             OUTPUT INSTRUCTIONS:\n\
             - Return STRICT JSON matching the schema.\n\
             - Use `edits: [{{ \"path\", \"old_text\", \"new_text\" }}]` for modifying existing files — copy `old_text` verbatim with 2-4 unique surrounding context lines so replacement is exact.\n\
             - Use `new_files: [{{ \"path\", \"content\" }}]` only if creating a brand-new file (e.g., caller regression tests when allowed).\n\
             - Do NOT self-report test results; the host sandbox independently applies your patch and runs all tests.",
            prepared.runtime.label(),
            id = strategy.id,
            title = strategy.title,
            tier = strategy.depth_tier,
            thesis = strategy.thesis,
            axis = strategy.tradeoff_axis,
            targets = strategy.target_files_hint,
            anti = strategy.anti_overlap_rules,
            test_cmd = red_gate.test_command(),
            red_patch = truncate_str(red_gate.patch(), 8_000),
        );

        let mut req = CreateInteractionRequest::for_model(model, prompt);
        req.system_instruction = Some(
            "You are a precision software engineer returning exact search-and-replace `edits` in strict JSON. \
             Copy `old_text` verbatim from the provided source files. Never touch the locked Red-Gate test file."
                .to_string(),
        );
        req.response_format = Some(ResponseFormat::json_schema(worker_edit_schema()));

        let api_res = tokio::time::timeout(remaining, gemini.create_interaction(&req)).await;
        let resp = match api_res {
            Ok(Ok(r)) => r,
            Ok(Err(err)) => {
                warn!(strategy = %strategy.id, iter, error = %err, "worker model call failed");
                last_feedback = format!("Model API error on loop {iter}: {err}");
                if cand.test_output_tail.is_empty() {
                    cand.test_output_tail = last_feedback.clone();
                }
                continue;
            }
            Err(_) => {
                warn!(strategy = %strategy.id, iter, "worker hit 90s per-strategy deadline");
                if cand.test_output_tail.is_empty() {
                    cand.test_output_tail = format!(
                        "Strategy reached {}s per-strategy deadline on loop {iter}",
                        STRATEGY_DEADLINE.as_secs()
                    );
                }
                break;
            }
        };

        let raw_text = resp.extract_text();
        let out = match extract_json_from_text::<WorkerEditOutput>(&raw_text) {
            Ok(parsed) => parsed,
            Err(err) => {
                // Attempt raw unified diff extraction fallback if model output a diff block
                let fallback_diff = extract_unified_diff_from_text(&raw_text);
                if !fallback_diff.trim().is_empty() {
                    WorkerEditOutput {
                        edits: Vec::new(),
                        new_files: Vec::new(),
                        git_diff: fallback_diff,
                        rationale: String::new(),
                    }
                } else {
                    last_feedback = format!(
                        "Invalid JSON response ({err}). Return strict JSON with `edits` array. Snippet: {}",
                        truncate_str(&raw_text, 400)
                    );
                    cand.test_output_tail = last_feedback.clone();
                    continue;
                }
            }
        };

        if !out.rationale.trim().is_empty() {
            emit_log(
                event_tx,
                session_id,
                LogLevel::Info,
                format!("worker:{}", strategy.id),
                format!("Loop {iter}/{MAX_EDIT_TEST_LOOPS}: {}", truncate_str(out.rationale.trim(), 180)),
            );
        }

        // Convert structured `edits` + `new_files` into a canonical `git diff` via host worktree.
        let candidate_diff = if !out.edits.is_empty() || !out.new_files.is_empty() {
            match prepared
                .synthesize_diff_from_edits(&out.edits, &out.new_files, locked_path)
                .await
            {
                Ok(app) if app.tampered => {
                    let msg = app.errors.join("; ");
                    emit_log(
                        event_tx,
                        session_id,
                        LogLevel::Warn,
                        format!("worker:{}", strategy.id),
                        msg.clone(),
                    );
                    cand.repro_passed = false;
                    cand.regression_suite_passed = false;
                    cand.test_output_tail = msg.clone();
                    last_feedback = msg;
                    continue;
                }
                Ok(app) if !app.errors.is_empty() => {
                    let err_summary = format!(
                        "Structured edit application failed:\n- {}\nEnsure `old_text` matches base HEAD verbatim.",
                        app.errors.join("\n- ")
                    );
                    cand.test_output_tail = err_summary.clone();
                    last_feedback = err_summary;
                    continue;
                }
                Ok(app) => app.git_diff,
                Err(err) => {
                    last_feedback = format!("Host edit synthesis error: {err}");
                    cand.test_output_tail = last_feedback.clone();
                    continue;
                }
            }
        } else {
            let extracted = extract_unified_diff_from_text(&out.git_diff);
            if extracted.is_empty() {
                out.git_diff.clone()
            } else {
                extracted
            }
        };

        if candidate_diff.trim().is_empty() {
            last_feedback =
                "Candidate produced an empty diff (no file changes). Provide concrete `edits` solving the issue."
                    .to_string();
            cand.test_output_tail = last_feedback.clone();
            continue;
        }

        // 100% INDEPENDENT HOST VERIFICATION (anti-tampering + git apply + locked repro + regression).
        match prepared
            .verify_candidate_patch(
                &candidate_diff,
                locked_path,
                red_gate.patch(),
                red_gate.test_command(),
            )
            .await
        {
            Ok(verdict) => {
                if verdict.tampered {
                    emit_log(
                        event_tx,
                        session_id,
                        LogLevel::Warn,
                        format!("worker:{}", strategy.id),
                        verdict.test_output_tail.clone(),
                    );
                    cand.git_diff.clear();
                    cand.repro_passed = false;
                    cand.regression_suite_passed = false;
                    cand.test_output_tail = verdict.test_output_tail.clone();
                    last_feedback = verdict.test_output_tail;
                    last_diff_snippet.clear();
                    continue;
                }

                cand.git_diff = candidate_diff.clone();
                cand.repro_passed = verdict.repro_passed;
                cand.regression_suite_passed = verdict.regression_suite_passed;
                cand.test_output_tail = verdict.test_output_tail.clone();
                cand.added_caller_tests = diff_adds_caller_tests(&cand.git_diff, locked_path);

                if verdict.repro_passed && verdict.regression_suite_passed {
                    cand.status = CandidateStatus::PassedTests;
                    emit_log(
                        event_tx,
                        session_id,
                        LogLevel::Info,
                        format!("worker:{}", strategy.id),
                        format!(
                            "HostVerifier PASSED locked repro + module regression on loop {iter}/{MAX_EDIT_TEST_LOOPS} (caller_tests={})",
                            cand.added_caller_tests
                        ),
                    );
                    break;
                }

                let reason = verdict
                    .failure_reason
                    .unwrap_or_else(|| "test verification failed".to_string());
                emit_log(
                    event_tx,
                    session_id,
                    LogLevel::Warn,
                    format!("worker:{}", strategy.id),
                    format!(
                        "Loop {iter}/{MAX_EDIT_TEST_LOOPS} host verification failed: {}",
                        truncate_str(&reason, 200)
                    ),
                );
                last_feedback = format!("{reason}\n\n{}", verdict.test_output_tail);
                last_diff_snippet = candidate_diff;
            }
            Err(err) => {
                warn!(strategy = %strategy.id, iter, error = %err, "HostVerifier error");
                last_feedback = format!("HostVerifier error: {err}");
                cand.test_output_tail = last_feedback.clone();
            }
        }
    }

    cand.status = if cand.is_green() {
        CandidateStatus::PassedTests
    } else {
        CandidateStatus::FailedTests
    };

    let detail = if cand.is_green() {
        format!(
            "Host-verified PASS on locked Red-Gate + regression suite (caller_tests={})",
            cand.added_caller_tests
        )
    } else {
        format!(
            "Failed host verification (repro={}, regression={})",
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

    cand
}

async fn design_carbon_rescue_strategy(
    gemini: &GeminiClient,
    session: &TournamentSession,
    multimodal_block: &str,
    red_gate: &RedGateSpec,
    repo: &RepoSnapshot,
    failed_candidates: &[CandidateResult],
) -> StrategySpec {
    let failure_digest = failed_candidates
        .iter()
        .map(|c| {
            format!(
                "Strategy `{}` (repro={}, regression={}):\nReal Host Test Output:\n{}\nDiff snippet:\n{}\n",
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
        "All {} initial tournament strategies failed independent host verification on '{}'.\n\
         Diagnose WHY every attempt failed from the real host test logs below and design ONE surgical,\n\
         high-conviction rescue StrategySpec (`id: \"s-rescue-carbon\"`) that directly fixes the\n\
         root cause without repeating the failed mistakes.\n\n\
         Problem Statement:\n{}\n\
         MULTIMODAL FINDINGS:\n{multimodal_block}\n\
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
        thesis: "Synthesize real host failure logs from prior candidates to fix the exact failing assertion cleanly without breaking existing callers."
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

fn worker_edit_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "edits": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "old_text": { "type": "string" },
                        "new_text": { "type": "string" }
                    },
                    "required": ["path", "old_text", "new_text"]
                }
            },
            "new_files": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "content": { "type": "string" }
                    },
                    "required": ["path", "content"]
                }
            },
            "git_diff": { "type": "string" },
            "rationale": { "type": "string" }
        },
        "required": ["edits", "rationale"]
    })
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
