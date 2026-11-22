use crate::intake::format_multimodal_findings;
use crate::repo::{emit_event, emit_log, extract_json_from_text, RepoSnapshot};
use crate::verifier::PreparedRepo;
use epicenter_common::{
    BenchmarkHarnessSpec, Gemini4Model, LiveEvent, LogLevel, RedGateSpec, StrategyDepthTier,
    StrategyPlan, StrategySpec, TournamentSession, TournamentStage,
};
use epicenter_gemini::{CreateInteractionRequest, GeminiClient, GenerationConfig, ResponseFormat};
use serde_json::json;
use std::collections::HashSet;
use tokio::sync::broadcast;
use tracing::warn;

/// Stage 3: `Gemini4Model::Carbon` Dynamic-K (`1..=5`) Orthogonal Strategy Planner.
///
/// Generates orthogonal `StrategySpec`s (defaulting to K=3 crisp strategies spanning
/// `SurgicalHotfix`, `AlgorithmicOptimization`/`TargetedRefactor`, and `ArchitecturalRootCause`)
/// with explicit `anti_overlap_rules` and a fast (~15–30ms) self-contained `BenchmarkHarnessSpec`.
pub async fn run_planner_stage(
    gemini: &GeminiClient,
    session: &mut TournamentSession,
    prepared: &PreparedRepo,
    red_gate: &RedGateSpec,
    repo: &RepoSnapshot,
    event_tx: &Option<broadcast::Sender<LiveEvent>>,
) -> anyhow::Result<StrategyPlan> {
    session.stage = TournamentStage::Planning;
    session.updated_at = chrono::Utc::now();
    emit_event(
        event_tx,
        LiveEvent::StageChanged {
            session_id: session.id,
            stage: TournamentStage::Planning,
            message: "Stage 3 — Carbon architecting dynamic-K orthogonal strategies & Arena harness"
                .to_string(),
        },
    );

    let multimodal_block = format_multimodal_findings(&session.intake.multimodal_findings);
    let focus_text = format!(
        "{}\n{}\n{}\n{}\n{}",
        session.intake.title,
        session.intake.problem_statement,
        session.intake.repro_steps.join("\n"),
        multimodal_block,
        red_gate.patch()
    );
    let focused_sources =
        repo.prompt_focused_source_context(&session.intake.suspect_files, &focus_text, 10_000);

    let prompt = build_planner_prompt(
        prepared.runtime.label(),
        session,
        red_gate,
        &multimodal_block,
        &focused_sources,
    );

    let mut req = CreateInteractionRequest::for_model(Gemini4Model::Carbon, prompt);
    req.system_instruction = Some(
        "You are Gemini 4 Carbon, a Principal Systems Architect designing orthogonal tournament strategies and a fast micro-benchmark harness. Return strict JSON."
            .to_string(),
    );
    req.response_format = Some(ResponseFormat::json_schema(strategy_plan_schema()));
    req.generation_config = Some(planner_generation_config());

    let plan = match gemini.create_interaction(&req).await {
        Ok(resp) => {
            let text = resp.extract_text();
            match extract_json_from_text::<StrategyPlan>(&text) {
                Ok(raw_plan) => normalize_and_validate_plan(raw_plan, session, red_gate),
                Err(err) => {
                    warn!(error = %err, "Carbon strategy plan JSON parse failed; using fallback 3-tier plan");
                    emit_log(
                        event_tx,
                        session.id,
                        LogLevel::Warn,
                        "planner",
                        format!("Carbon plan JSON parse fallback ({err}); using 3-tier orthogonal plan"),
                    );
                    fallback_strategy_plan(session, red_gate)
                }
            }
        }
        Err(err) => {
            warn!(error = %err, "Carbon planner API call failed; using fallback 3-tier plan");
            emit_log(
                event_tx,
                session.id,
                LogLevel::Warn,
                "planner",
                format!("Carbon planner API error ({err}); using 3-tier orthogonal plan"),
            );
            fallback_strategy_plan(session, red_gate)
        }
    };

    emit_log(
        event_tx,
        session.id,
        LogLevel::Info,
        "planner",
        format!(
            "Locked K={} orthogonal strategies: [{}]",
            plan.strategies.len(),
            plan.strategies
                .iter()
                .map(|s| format!("{} ({:?})", s.id, s.depth_tier))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    );

    session.strategy_plan = Some(plan.clone());
    session.updated_at = chrono::Utc::now();
    emit_event(
        event_tx,
        LiveEvent::StrategyPlanReady {
            session_id: session.id,
            plan: Box::new(plan.clone()),
        },
    );

    Ok(plan)
}

pub(crate) fn planner_generation_config() -> GenerationConfig {
    GenerationConfig {
        thinking_level: Some("low".to_string()),
        ..Default::default()
    }
}

/// Formats the calibrated target strategy count directive (`1..=5`) for Stage 3 Carbon.
pub(crate) fn format_calibrated_k_directive(suggested_k: Option<u8>) -> String {
    match suggested_k.map(|k| k.clamp(1, 5)) {
        Some(k) => format!(
            "Calibrated Stage 1 triage (`gemini-decision-1`) recommends target strategy count K={k} (valid range 1..=5).\n\
             Design K={k} crisp, genuinely orthogonal engineering strategies (stay within 1..=5 and only\n\
             deviate from K={k} if the concrete source code clearly demands fewer or more degrees of freedom)."
        ),
        None => "Default to K=3 crisp, genuinely orthogonal engineering strategies (range 1..=5 only if\n\
             the problem clearly demands fewer or more degrees of freedom)."
            .to_string(),
    }
}

/// Builds the Stage 3 Carbon strategy planning prompt, incorporating Stage 1's calibrated `suggested_k`
/// when available while preserving `1..=5` tournament invariants.
pub(crate) fn build_planner_prompt(
    runtime_label: &str,
    session: &TournamentSession,
    red_gate: &RedGateSpec,
    multimodal_block: &str,
    focused_sources: &str,
) -> String {
    let k_directive = format_calibrated_k_directive(session.intake.suggested_k);
    let calibrated_k_label = session
        .intake
        .suggested_k
        .map(|k| format!("{} (calibrated by gemini-decision-1)", k.clamp(1, 5)))
        .unwrap_or_else(|| "3 (default)".to_string());

    format!(
        "You are Stage 3 (Dynamic-K Orthogonal Strategy Architect) of the Epicenter tournament.\n\
         {k_directive}\n\n\
         MANDATORY SPECTRUM RULES:\n\
         - Strategy 1 (`surgical_hotfix`): minimal footprint, narrowest diff, lowest immediate collateral risk.\n\
         - Strategy 2 (`algorithmic_optimization` or `targeted_refactor`): optimizes hot-path throughput/memory\n\
           or refactors the local subsystem cleanly.\n\
         - Strategy 3 (`architectural_root_cause`): cures the underlying upstream invariant cleanly.\n\
         - For K < 3, prioritize the highest-signal depth tiers without redundancy; for K > 3 (up to 5),\n\
           add genuinely distinct algorithmic or architectural designs with zero overlap.\n\
         - Set `allow_caller_tests: true` on `architectural_root_cause` strategies so workers can add\n\
           regression tests for untested callers and shrink dark-zone blast-radius risk.\n\
         - Give every strategy explicit `anti_overlap_rules` forbidding it from copying sibling approaches.\n\
         - Strategy IDs must be short lowercase slugs (e.g. \"s1-hotfix\", \"s2-algorithmic\", \"s3-root-cause\").\n\
         - Keep each strategy's thesis, tradeoff_axis, and risk_hypothesis crisp (1-2 concise sentences).\n\n\
         FAST ARENA MICRO-BENCHMARK SPEC:\n\
         - Design `shared_benchmark_spec` with `trials: 3`, `warmup_trials: 1`, `metric_unit: \"ms\"`.\n\
         - Provide a fast, self-contained `script_content` in {runtime_label} that exercises the affected code path\n\
           in ~15–30ms per run (loops over the hot function/API directly, exits 0 on BOTH unmodified\n\
           baseline and patched code so wall-clock timings are cleanly comparable!).\n\
         - STRICT RUNTIME BUDGET: NEVER write heavy 500–1000ms loops (those burn ~15s across warmup+trials!).\n\
           Choose a modest iteration count targeting ~15–30ms per invocation so total Stage 5 benchmark\n\
           runtime stays under ~3 seconds across all candidates.\n\n\
         Repository Runtime: {runtime_label}\n\
         Calibrated Target K: {calibrated_k_label}\n\
         Issue Title: {}\n\
         Task Kind: {:?}\n\
         Problem Statement:\n{}\n\
         Repro Steps: {:?}\n\
         Suspect Files: {:?}\n\
         Red-Gate Test Command: {}\n\
         Red-Gate Locked Path: {:?}\n\n\
         MULTIMODAL FINDINGS (screenshots / recordings / audio / logs):\n{multimodal_block}\n\n\
         Repository Sources:\n{focused_sources}\n\n\
         Return STRICT JSON matching the schema.",
        session.intake.title,
        session.intake.task_kind,
        session.intake.problem_statement,
        session.intake.repro_steps,
        session.intake.suspect_files,
        red_gate.test_command(),
        red_gate.locked_test_path(),
    )
}

/// Normalizes strategy IDs, enforces `1..=5` clamping and uniqueness, ensures `allow_caller_tests`
/// on `ArchitecturalRootCause` strategies, and guarantees a valid benchmark harness.
pub fn normalize_and_validate_plan(
    raw: StrategyPlan,
    session: &TournamentSession,
    red_gate: &RedGateSpec,
) -> StrategyPlan {
    let mut plan = raw.clamped();
    if plan.strategies.is_empty() {
        return fallback_strategy_plan(session, red_gate);
    }

    let mut used_ids = HashSet::new();
    for (idx, strat) in plan.strategies.iter_mut().enumerate() {
        let base = sanitize_strategy_id(&strat.id, idx + 1);
        let mut unique = base.clone();
        let mut suffix = 2usize;
        while !used_ids.insert(unique.clone()) {
            unique = format!("{base}-{suffix}");
            suffix += 1;
        }
        strat.id = unique;

        if strat.depth_tier == StrategyDepthTier::ArchitecturalRootCause {
            strat.allow_caller_tests = true;
        }
        if strat.anti_overlap_rules.is_empty() {
            strat.anti_overlap_rules.push(format!(
                "Stay strictly aligned with {:?} scope and avoid duplicating sibling strategies.",
                strat.depth_tier
            ));
        }
    }

    if plan.shared_benchmark_spec.trials == 0 {
        plan.shared_benchmark_spec.trials = 3;
    } else {
        plan.shared_benchmark_spec.trials = plan.shared_benchmark_spec.trials.clamp(3, 5);
    }
    if plan.shared_benchmark_spec.warmup_trials == 0 {
        plan.shared_benchmark_spec.warmup_trials = 1;
    }
    if plan.shared_benchmark_spec.metric_unit.trim().is_empty() {
        plan.shared_benchmark_spec.metric_unit = "ms".to_string();
    }
    if plan.shared_benchmark_spec.command.trim().is_empty() {
        plan.shared_benchmark_spec.command = red_gate.test_command().to_string();
    }

    // Re-check invariants
    if plan.validate().is_err() {
        return fallback_strategy_plan(session, red_gate);
    }
    plan
}

fn sanitize_strategy_id(raw: &str, ordinal: usize) -> String {
    let cleaned: String = raw
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('-');
    if trimmed.is_empty() {
        format!("strategy-{ordinal}")
    } else {
        trimmed.to_string()
    }
}

fn fallback_strategy_plan(session: &TournamentSession, red_gate: &RedGateSpec) -> StrategyPlan {
    let hints = session.intake.suspect_files.clone();
    StrategyPlan {
        degree_of_freedom_rationale:
            "3-tier orthogonal spectrum balancing immediate hotfix safety, algorithmic hot-path performance, and upstream root-cause invariant repair."
                .to_string(),
        strategies: vec![
            StrategySpec {
                id: "s1-surgical-hotfix".to_string(),
                title: "Surgical Boundary Guard Hotfix".to_string(),
                thesis: "Apply the smallest localized guard at the failing call site to neutralize the immediate symptom with minimal blast radius."
                    .to_string(),
                tradeoff_axis: "Minimal blast-radius footprint vs. localized symptom guard".to_string(),
                target_files_hint: hints.iter().take(2).cloned().collect(),
                risk_hypothesis: "Touches fewest callers (lowest Ring 1/2 count), though upstream invariant remains guarded rather than redesigned."
                    .to_string(),
                depth_tier: StrategyDepthTier::SurgicalHotfix,
                anti_overlap_rules: vec![
                    "Do not refactor upstream interfaces or add caching structures.".to_string(),
                    "Keep changes strictly localized to the failing function.".to_string(),
                ],
                allow_caller_tests: false,
            },
            StrategySpec {
                id: "s2-algorithmic-opt".to_string(),
                title: "Algorithmic Fast-Path Optimization".to_string(),
                thesis: "Eliminate redundant work along the hot path while fixing the defect to maximize throughput and reduce tail latency."
                    .to_string(),
                tradeoff_axis: "Throughput & p95 latency vs. moderate implementation complexity"
                    .to_string(),
                target_files_hint: hints.clone(),
                risk_hypothesis: "Introduces a faster data structure or short-circuit path; requires verifying cache/state invalidation."
                    .to_string(),
                depth_tier: StrategyDepthTier::AlgorithmicOptimization,
                anti_overlap_rules: vec![
                    "Do not rely on a bare symptom guard; optimize the core computational path."
                        .to_string(),
                    "Do not alter unrelated public module signatures.".to_string(),
                ],
                allow_caller_tests: false,
            },
            StrategySpec {
                id: "s3-root-cause-cure".to_string(),
                title: "Architectural Root-Cause Invariant Cure".to_string(),
                thesis: "Enforce the violated invariant at its authoritative source and add caller regression tests to eliminate untested dark zones."
                    .to_string(),
                tradeoff_axis: "Highest root-cause durability & dark-zone elimination vs. broader touch surface"
                    .to_string(),
                target_files_hint: hints,
                risk_hypothesis: "Touches upstream invariant producer and adds caller tests so all Ring 1/2 callers are verified green."
                    .to_string(),
                depth_tier: StrategyDepthTier::ArchitecturalRootCause,
                anti_overlap_rules: vec![
                    "Fix the root invariant at its source rather than masking symptoms downstream."
                        .to_string(),
                    "Add targeted caller regression tests (outside the locked repro test) to cover Ring 1 callers."
                        .to_string(),
                ],
                allow_caller_tests: true,
            },
        ],
        shared_benchmark_spec: BenchmarkHarnessSpec {
            name: format!("{} hot-path latency", session.intake.title),
            command: red_gate.test_command().to_string(),
            setup_command: None,
            trials: 5,
            warmup_trials: 1,
            metric_unit: "ms".to_string(),
            entrypoint_contract: match red_gate {
                RedGateSpec::Feature(f) => Some(f.benchmark_entrypoint_contract.clone()),
                RedGateSpec::Bug(_) => None,
            },
            script_content: None,
        },
    }
}

fn strategy_plan_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "degree_of_freedom_rationale": { "type": "string" },
            "strategies": {
                "type": "array",
                "minItems": 1,
                "maxItems": 5,
                "items": {
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
                }
            },
            "shared_benchmark_spec": {
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "command": { "type": "string" },
                    "setup_command": { "type": "string" },
                    "trials": { "type": "integer" },
                    "warmup_trials": { "type": "integer" },
                    "metric_unit": { "type": "string" },
                    "entrypoint_contract": { "type": "string" },
                    "script_content": { "type": "string" }
                },
                "required": ["name", "command", "trials", "warmup_trials", "metric_unit"]
            }
        },
        "required": ["degree_of_freedom_rationale", "strategies", "shared_benchmark_spec"]
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TournamentEngine;
    use epicenter_common::{BugRedGate, CreateSessionRequest, RedGateResolution, TaskKind};

    fn sample_session_and_gate(suggested_k: Option<u8>) -> (TournamentSession, RedGateSpec) {
        let mut session = TournamentEngine::initialize_session(&CreateSessionRequest {
            repo_url: "https://github.com/acme/pricing".to_string(),
            commit_sha: Some("deadbeef".to_string()),
            git_ref: Some("main".to_string()),
            task_kind: Some(TaskKind::Bug),
            title: Some("Cache collision across tenants".to_string()),
            problem_statement: "Shared pricing cache collides when tax rates differ.".to_string(),
            attachment_ids: Vec::new(),
            attachments: Vec::new(),
        });
        session.intake.suggested_k = suggested_k;
        session.intake.suspect_files = vec!["app/pricing.py".to_string()];

        let gate = RedGateSpec::Bug(BugRedGate {
            repro_test_path: "tests/test_epicenter_repro.py".to_string(),
            repro_test_patch: String::new(),
            test_command: "pytest -q tests/test_epicenter_repro.py".to_string(),
            verified_fails_on_head: true,
            failure_excerpt: "AssertionError".to_string(),
            base_environment_id: None,
            resolution: RedGateResolution::VerifiedNeon,
        });
        (session, gate)
    }

    #[test]
    fn calibrated_k_directive_handles_none_valid_and_out_of_range_values() {
        let default_dir = format_calibrated_k_directive(None);
        assert!(default_dir.contains("Default to K=3"));
        assert!(default_dir.contains("1..=5"));

        let k2 = format_calibrated_k_directive(Some(2));
        assert!(k2.contains("K=2"));
        assert!(k2.contains("gemini-decision-1"));

        let k0_clamped = format_calibrated_k_directive(Some(0));
        assert!(k0_clamped.contains("K=1"));

        let k9_clamped = format_calibrated_k_directive(Some(9));
        assert!(k9_clamped.contains("K=5"));
    }

    #[test]
    fn build_planner_prompt_incorporates_suggested_k_and_preserves_bounds() {
        let (session_cal, gate) = sample_session_and_gate(Some(4));
        let prompt_cal =
            build_planner_prompt("Python", &session_cal, &gate, "None", "def compute_total(): pass");
        assert!(prompt_cal.contains("target strategy count K=4"));
        assert!(prompt_cal.contains("Calibrated Target K: 4 (calibrated by gemini-decision-1)"));
        assert!(prompt_cal.contains("1..=5"));

        let (session_def, _) = sample_session_and_gate(None);
        let prompt_def =
            build_planner_prompt("Python", &session_def, &gate, "None", "def compute_total(): pass");
        assert!(prompt_def.contains("Default to K=3"));
        assert!(prompt_def.contains("Calibrated Target K: 3 (default)"));
    }
}
