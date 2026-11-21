use crate::intake::format_multimodal_findings;
use crate::repo::{
    emit_event, emit_log, extract_json_from_text, extract_unified_diff_from_text, truncate_str,
    RepoSnapshot,
};
use crate::verifier::{PreparedRepo, RepoRuntime};
use epicenter_common::{
    BugRedGate, FeatureRedGate, Gemini4Model, LiveEvent, LogLevel, RedGateResolution, RedGateSpec,
    TaskKind, TournamentSession, TournamentStage,
};
use epicenter_gemini::{CreateInteractionRequest, GeminiClient, GenerationConfig, ResponseFormat};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::broadcast;
use tracing::warn;

#[derive(Debug, Deserialize)]
struct BugRedGateOutput {
    #[serde(default)]
    repro_test_path: String,
    #[serde(default)]
    repro_test_content: String,
    #[serde(default)]
    repro_test_patch: String,
    #[serde(default)]
    test_command: String,
    #[serde(default)]
    failure_excerpt: String,
}

#[derive(Debug, Deserialize)]
struct FeatureRedGateOutput {
    #[serde(default)]
    acceptance_test_path: String,
    #[serde(default)]
    acceptance_test_content: String,
    #[serde(default)]
    acceptance_test_patch: String,
    #[serde(default)]
    benchmark_entrypoint_contract: String,
    #[serde(default)]
    test_command: String,
}

/// Stage 2: Locked Red-Gate Verification powered by direct `Neon`/`Carbon` + independent
/// host-side verification (`PreparedRepo::verify_repro_fails_on_head`).
///
/// - For `TaskKind::Bug`: runs up to 2 direct `Gemini4Model::Neon` attempts + 1 `Gemini4Model::Carbon`
///   escalation. Every candidate test is independently verified by the host sandbox on unmodified
///   base HEAD. If no attempt yields a genuinely failing reproduction test, fails fast honestly
///   with exact host diagnostics (no synthetic fallback).
/// - For `TaskKind::Feature`: uses `Gemini4Model::Carbon` with language-aware runtime detection to
///   produce a `FeatureRedGate`.
pub async fn run_red_gate_stage(
    gemini: &GeminiClient,
    session: &mut TournamentSession,
    prepared: &PreparedRepo,
    repo: &RepoSnapshot,
    event_tx: &Option<broadcast::Sender<LiveEvent>>,
) -> anyhow::Result<RedGateSpec> {
    session.stage = TournamentStage::RedGate;
    session.updated_at = chrono::Utc::now();
    emit_event(
        event_tx,
        LiveEvent::StageChanged {
            session_id: session.id,
            stage: TournamentStage::RedGate,
            message: format!(
                "Stage 2 — Locked Red-Gate ({:?}, {} host sandbox)",
                session.intake.task_kind,
                prepared.runtime.label()
            ),
        },
    );

    let red_gate = match session.intake.task_kind {
        TaskKind::Bug => run_bug_red_gate(gemini, session, prepared, repo, event_tx).await?,
        TaskKind::Feature => run_feature_red_gate(gemini, session, prepared, repo, event_tx).await,
    };

    session.red_gate = Some(red_gate.clone());
    session.updated_at = chrono::Utc::now();
    emit_event(
        event_tx,
        LiveEvent::RedGateVerified {
            session_id: session.id,
            red_gate: Box::new(red_gate.clone()),
        },
    );

    Ok(red_gate)
}

async fn run_bug_red_gate(
    gemini: &GeminiClient,
    session: &TournamentSession,
    prepared: &PreparedRepo,
    repo: &RepoSnapshot,
    event_tx: &Option<broadcast::Sender<LiveEvent>>,
) -> anyhow::Result<RedGateSpec> {
    let multimodal_block = format_multimodal_findings(&session.intake.multimodal_findings);
    let focus_text = format!(
        "{}\n{}\n{}\n{}",
        session.intake.title,
        session.intake.problem_statement,
        session.intake.repro_steps.join("\n"),
        multimodal_block
    );
    let source_context =
        repo.prompt_focused_source_context(&session.intake.suspect_files, &focus_text, 36_000);
    let test_examples = repo.prompt_test_examples(&session.intake.suspect_files, 8_000);
    let suggested_path = suggested_repro_path(prepared.runtime, &repo.file_tree);
    let default_cmd = prepared.default_test_command(Some(&suggested_path));

    let attempts: [(u8, Gemini4Model, RedGateResolution, &str); 3] = [
        (1, Gemini4Model::Neon, RedGateResolution::VerifiedNeon, "Neon 1/2"),
        (2, Gemini4Model::Neon, RedGateResolution::VerifiedNeon, "Neon 2/2"),
        (
            3,
            Gemini4Model::Carbon,
            RedGateResolution::EscalatedCarbon,
            "Carbon escalation",
        ),
    ];

    let mut last_diagnostic = String::from("no attempts executed");
    let mut last_output_tail = String::new();

    for (attempt_num, model, resolution, label) in attempts {
        emit_log(
            event_tx,
            session.id,
            if model == Gemini4Model::Carbon {
                LogLevel::Warn
            } else {
                LogLevel::Info
            },
            "red_gate",
            format!("Bug Red-Gate attempt {attempt_num}/3 ({label}) + independent host verification"),
        );

        let retry_block = if attempt_num == 1 {
            String::new()
        } else {
            format!(
                "\nPREVIOUS ATTEMPT FAILED INDEPENDENT HOST VERIFICATION:\n\
                 Diagnostic: {}\n\
                 Real Host Test Output:\n{}\n\
                 Fix the exact issue above! Ensure your test:\n\
                 - Has ZERO syntax/import/fixture errors.\n\
                 - Touches ONLY the new reproduction test file (never production code).\n\
                 - Genuinely FAILS an assertion on unmodified HEAD demonstrating the exact bug.\n",
                truncate_str(&last_diagnostic, 600),
                truncate_str(&last_output_tail, 1200),
            )
        };

        let prompt = format!(
            "You are Stage 2 (Locked Red-Gate Author) of the Epicenter engineering tournament.\n\
             Your SOLE objective is to write a minimal, standalone reproduction test file for the bug below\n\
             WITHOUT modifying any production source files.\n\n\
             HOST VERIFICATION CONTRACT (enforced independently by the server on unmodified HEAD):\n\
             1. Write a standalone test file at a clean relative path (suggested: `{suggested_path}`).\n\
             2. Prefer returning the full file text in `repro_test_content` (the host builds a guaranteed-clean `git diff` deterministically).\n\
             3. The host will apply ONLY your new test file on unmodified HEAD and run `test_command` (default: `{default_cmd}`).\n\
             4. The test MUST genuinely FAIL an assertion (exit code 1 for pytest, non-zero assertion failure for Node/Rust/Go) exposing the exact bug.\n\
             5. Tests that pass on HEAD (exit 0) or fail due to SyntaxError / ImportError / ModuleNotFoundError / collection errors are AUTOMATICALLY REJECTED.\n\
             6. Follow the repository's real imports, test runner, and fixtures shown below.\n\n\
             Repository Runtime: {}\n\
             Default Test Command: {default_cmd}\n\
             Issue Title: {}\n\
             Problem Statement:\n{}\n\
             Repro Steps: {:?}\n\
             Suspect Files: {:?}\n\n\
             MULTIMODAL FINDINGS (screenshots / recordings / audio / logs):\n{multimodal_block}\n\
             {retry_block}\n\
             EXISTING REPOSITORY TEST EXAMPLES (follow these conventions & imports):\n{test_examples}\n\n\
             RELEVANT REPOSITORY SOURCES:\n{source_context}\n\n\
             Return STRICT JSON matching the schema.",
            prepared.runtime.label(),
            session.intake.title,
            session.intake.problem_statement,
            session.intake.repro_steps,
            session.intake.suspect_files,
        );

        let mut req = CreateInteractionRequest::for_model(model, prompt);
        req.system_instruction = Some(
            "You are authoring a standalone failing reproduction test for Stage 2 Red-Gate. \
             Return strict JSON with `repro_test_path`, full `repro_test_content`, and `test_command`. \
             Never edit production source files."
                .to_string(),
        );
        req.response_format = Some(ResponseFormat::json_schema(bug_red_gate_schema()));
        req.generation_config = bug_red_gate_generation_config(attempt_num);

        let resp = match gemini.create_interaction(&req).await {
            Ok(r) => r,
            Err(err) => {
                last_diagnostic = format!("{label} API error: {err}");
                last_output_tail.clear();
                warn!(attempt = attempt_num, error = %err, "Red-Gate model call failed");
                continue;
            }
        };

        let raw = resp.extract_text();
        let out = match extract_json_from_text::<BugRedGateOutput>(&raw) {
            Ok(parsed) => parsed,
            Err(err) => {
                last_diagnostic = format!("{label} returned invalid JSON: {err}");
                last_output_tail = truncate_str(&raw, 600);
                warn!(attempt = attempt_num, error = %err, "Red-Gate JSON parse failed");
                continue;
            }
        };

        let repro_path = if out.repro_test_path.trim().is_empty() {
            suggested_path.clone()
        } else {
            out.repro_test_path.trim().to_string()
        };

        // Deterministically construct a clean `git diff` from `repro_test_content` whenever provided.
        let repro_patch = if !out.repro_test_content.trim().is_empty() {
            match prepared
                .generate_new_file_patch(&repro_path, &out.repro_test_content)
                .await
            {
                Ok(diff) if !diff.trim().is_empty() => diff,
                Ok(_) => {
                    last_diagnostic = format!(
                        "Generated empty diff for '{repro_path}' (content identical to HEAD)"
                    );
                    last_output_tail.clear();
                    continue;
                }
                Err(err) => {
                    last_diagnostic = format!("Failed generating diff for '{repro_path}': {err}");
                    last_output_tail.clear();
                    continue;
                }
            }
        } else {
            let extracted = extract_unified_diff_from_text(&out.repro_test_patch);
            if extracted.is_empty() {
                out.repro_test_patch.clone()
            } else {
                extracted
            }
        };

        if repro_patch.trim().is_empty() {
            last_diagnostic = format!("{label} returned neither repro_test_content nor repro_test_patch");
            last_output_tail.clear();
            continue;
        }

        let test_cmd = prepared.sanitize_test_command(&out.test_command, Some(&repro_path));

        // INDEPENDENT HOST VERIFICATION — never trust LLM self-report!
        match prepared
            .verify_repro_fails_on_head(&repro_path, &repro_patch, &test_cmd)
            .await
        {
            Ok(verdict) if verdict.verified_fails_on_head => {
                emit_log(
                    event_tx,
                    session.id,
                    LogLevel::Info,
                    "red_gate",
                    format!(
                        "HostVerifier independently locked failing repro '{}' via {label} (exit {:?})",
                        repro_path, verdict.exit_code
                    ),
                );
                let excerpt = if verdict.output_tail.trim().is_empty() {
                    truncate_str(&out.failure_excerpt, 1500)
                } else {
                    truncate_str(&verdict.output_tail, 1500)
                };
                return Ok(RedGateSpec::Bug(BugRedGate {
                    repro_test_path: repro_path,
                    repro_test_patch: repro_patch,
                    test_command: verdict.command_used,
                    verified_fails_on_head: true,
                    failure_excerpt: excerpt,
                    base_environment_id: None,
                    resolution,
                }));
            }
            Ok(verdict) => {
                last_diagnostic = verdict.diagnostic.clone();
                last_output_tail = verdict.output_tail.clone();
                warn!(
                    attempt = attempt_num,
                    diagnostic = %verdict.diagnostic,
                    "HostVerifier rejected Red-Gate attempt"
                );
                emit_log(
                    event_tx,
                    session.id,
                    LogLevel::Warn,
                    "red_gate",
                    format!(
                        "Attempt {attempt_num}/3 ({label}) rejected by HostVerifier: {}",
                        truncate_str(&verdict.diagnostic, 240)
                    ),
                );
            }
            Err(err) => {
                last_diagnostic = format!("HostVerifier execution error: {err}");
                last_output_tail.clear();
                warn!(attempt = attempt_num, error = %err, "HostVerifier errored on Red-Gate");
            }
        }
    }

    // Honest fail-fast: never synthesize a fake CharacterizationFallback Red-Gate!
    anyhow::bail!(
        "Stage 2 Red-Gate failed: HostVerifier could not independently verify a failing reproduction test on unmodified HEAD after 3 attempts (2x Neon + 1x Carbon). Last diagnostic: {} | Output tail: {}",
        truncate_str(&last_diagnostic, 400),
        truncate_str(&last_output_tail, 500)
    )
}

async fn run_feature_red_gate(
    gemini: &GeminiClient,
    session: &TournamentSession,
    prepared: &PreparedRepo,
    repo: &RepoSnapshot,
    event_tx: &Option<broadcast::Sender<LiveEvent>>,
) -> RedGateSpec {
    emit_log(
        event_tx,
        session.id,
        LogLevel::Info,
        "red_gate",
        format!(
            "Synthesizing FeatureRedGate acceptance contract via Gemini 4 Carbon ({})",
            prepared.runtime.label()
        ),
    );

    let multimodal_block = format_multimodal_findings(&session.intake.multimodal_findings);
    let suggested_path = suggested_repro_path(prepared.runtime, &repo.file_tree);
    let default_cmd = prepared.default_test_command(Some(&suggested_path));

    let prompt = format!(
        "You are Stage 2 (Feature Acceptance Red-Gate) of the Epicenter engineering tournament.\n\
         Design a crisp acceptance test and benchmark entrypoint contract so all competing\n\
         strategies implement the exact same public API and pass the exact same acceptance criteria.\n\
         Repository Runtime: {}\n\
         Suggested Acceptance Test Path: {suggested_path}\n\
         Default Test Command: {default_cmd}\n\n\
         Feature Title: {}\n\
         Problem Statement:\n{}\n\
         Requirements / Steps: {:?}\n\
         Suspect Files: {:?}\n\n\
         MULTIMODAL FINDINGS:\n{multimodal_block}\n\n\
         EXISTING TEST EXAMPLES:\n{}\n\n\
         Repository Sources:\n{}\n\n\
         Return STRICT JSON matching the schema. Prefer `acceptance_test_path` + `acceptance_test_content`.",
        prepared.runtime.label(),
        session.intake.title,
        session.intake.problem_statement,
        session.intake.repro_steps,
        session.intake.suspect_files,
        repo.prompt_test_examples(&session.intake.suspect_files, 6_000),
        repo.prompt_source_context(&session.intake.suspect_files, 30_000),
    );

    let mut req = CreateInteractionRequest::for_model(Gemini4Model::Carbon, prompt);
    req.system_instruction = Some(
        "You are Gemini 4 Carbon authoring a strict FeatureRedGate contract. Always return valid JSON."
            .to_string(),
    );
    req.response_format = Some(ResponseFormat::json_schema(feature_red_gate_schema()));

    if let Ok(resp) = gemini.create_interaction(&req).await {
        if let Ok(out) = extract_json_from_text::<FeatureRedGateOutput>(&resp.extract_text()) {
            let acc_path = if out.acceptance_test_path.trim().is_empty() {
                suggested_path.clone()
            } else {
                out.acceptance_test_path.trim().to_string()
            };

            let patch = if !out.acceptance_test_content.trim().is_empty() {
                prepared
                    .generate_new_file_patch(&acc_path, &out.acceptance_test_content)
                    .await
                    .unwrap_or_else(|_| out.acceptance_test_patch.clone())
            } else {
                out.acceptance_test_patch
            };

            let target_opt = if patch.trim().is_empty() {
                None
            } else {
                Some(acc_path.as_str())
            };
            let cmd = prepared.sanitize_test_command(&out.test_command, target_opt);

            return RedGateSpec::Feature(FeatureRedGate {
                acceptance_test_patch: patch,
                benchmark_entrypoint_contract: if out.benchmark_entrypoint_contract.trim().is_empty()
                {
                    format!("Benchmark public entrypoint for '{}'", session.intake.title)
                } else {
                    out.benchmark_entrypoint_contract
                },
                test_command: cmd,
                base_environment_id: None,
            });
        }
    }

    RedGateSpec::Feature(FeatureRedGate {
        acceptance_test_patch: String::new(),
        benchmark_entrypoint_contract: format!(
            "Verify functional acceptance and hot-path throughput for '{}'",
            session.intake.title
        ),
        test_command: prepared.default_test_command(None),
        base_environment_id: None,
    })
}

/// Adaptive reasoning depth for Stage 2 Bug Red-Gate:
/// - Attempt 1 (`Neon 1/2`): `"low"` thinking for fast first-pass repro authoring.
/// - Attempt 2 (`Neon 2/2`) & Attempt 3 (`Carbon` escalation): `"medium"` thinking so harder
///   bugs escalate reasoning depth cleanly without hitting 90s+ default-high stalls.
pub(crate) fn bug_red_gate_generation_config(attempt_num: u8) -> Option<GenerationConfig> {
    let level = if attempt_num == 1 { "low" } else { "medium" };
    Some(GenerationConfig {
        thinking_level: Some(level.to_string()),
        ..Default::default()
    })
}

fn suggested_repro_path(runtime: RepoRuntime, file_tree: &[String]) -> String {
    let test_prefix = if file_tree.iter().any(|p| p.starts_with("tests/")) {
        "tests"
    } else if file_tree.iter().any(|p| p.starts_with("test/")) {
        "test"
    } else {
        "tests"
    };

    match runtime {
        RepoRuntime::Python => format!("{test_prefix}/test_epicenter_repro.py"),
        RepoRuntime::NodeTs => {
            let uses_ts = file_tree
                .iter()
                .any(|p| p.ends_with(".ts") || p.ends_with(".tsx"));
            if uses_ts {
                format!("{test_prefix}/epicenter_repro.test.ts")
            } else {
                format!("{test_prefix}/epicenter_repro.test.js")
            }
        }
        RepoRuntime::Rust => "tests/epicenter_repro.rs".to_string(),
        RepoRuntime::Go => "epicenter_repro_test.go".to_string(),
        RepoRuntime::Unknown => format!("{test_prefix}/test_epicenter_repro.txt"),
    }
}

fn bug_red_gate_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "repro_test_path": { "type": "string" },
            "repro_test_content": { "type": "string" },
            "repro_test_patch": { "type": "string" },
            "test_command": { "type": "string" },
            "failure_excerpt": { "type": "string" }
        },
        "required": ["repro_test_path", "repro_test_content", "test_command"]
    })
}

fn feature_red_gate_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "acceptance_test_path": { "type": "string" },
            "acceptance_test_content": { "type": "string" },
            "acceptance_test_patch": { "type": "string" },
            "benchmark_entrypoint_contract": { "type": "string" },
            "test_command": { "type": "string" }
        },
        "required": ["benchmark_entrypoint_contract", "test_command"]
    })
}
