use crate::repo::{
    default_sandbox_allowlist, emit_event, emit_log, extract_json_from_text, normalize_clone_url,
    truncate_str, RepoSnapshot,
};
use epicenter_common::{
    BugRedGate, FeatureRedGate, Gemini4Model, LiveEvent, LogLevel, RedGateResolution, RedGateSpec,
    TaskKind, TournamentSession, TournamentStage,
};
use epicenter_gemini::{
    CreateInteractionRequest, GeminiClient, InteractionEnvironment, ResponseFormat,
};
use serde::Deserialize;
use serde_json::json;
use std::time::Duration;
use tokio::sync::broadcast;
use tracing::warn;

const RED_GATE_POLL_INTERVAL: Duration = Duration::from_secs(3);
const RED_GATE_TIMEOUT: Duration = Duration::from_secs(240);

#[derive(Debug, Deserialize)]
struct BugRedGateOutput {
    #[serde(default)]
    repro_test_path: String,
    #[serde(default)]
    repro_test_patch: String,
    #[serde(default)]
    test_command: String,
    #[serde(default)]
    verified_fails_on_head: bool,
    #[serde(default)]
    failure_excerpt: String,
}

#[derive(Debug, Deserialize)]
struct FeatureRedGateOutput {
    #[serde(default)]
    acceptance_test_patch: String,
    #[serde(default)]
    benchmark_entrypoint_contract: String,
    #[serde(default)]
    test_command: String,
}

/// Output from Stage 2 Red-Gate including the resolved Neon Managed Agent worker ID.
#[derive(Debug, Clone)]
pub struct RedGateStageResult {
    pub worker_agent_id: String,
    pub red_gate: RedGateSpec,
}

/// Stage 2: Locked Red-Gate Verification.
///
/// Ensures the Neon Managed Agent worker is registered, then either:
/// - For `TaskKind::Bug`: runs up to 2 Neon Managed Agent attempts to author a standalone
///   reproduction test verified to FAIL on unmodified `HEAD` (`VerifiedNeon`), escalating to
///   1 deep `Gemini4Model::Carbon` attempt (`EscalatedCarbon`), and falling back gracefully
///   to `CharacterizationFallback` (`verified_fails_on_head: false`) rather than hard-crashing.
/// - For `TaskKind::Feature`: uses `Gemini4Model::Carbon` to produce a `FeatureRedGate`.
pub async fn run_red_gate_stage(
    gemini: &GeminiClient,
    session: &mut TournamentSession,
    repo: &RepoSnapshot,
    event_tx: &Option<broadcast::Sender<LiveEvent>>,
) -> anyhow::Result<RedGateStageResult> {
    session.stage = TournamentStage::RedGate;
    session.updated_at = chrono::Utc::now();
    emit_event(
        event_tx,
        LiveEvent::StageChanged {
            session_id: session.id,
            stage: TournamentStage::RedGate,
            message: format!(
                "Stage 2 — Locked Red-Gate ({:?})",
                session.intake.task_kind
            ),
        },
    );

    let worker_agent_id = gemini.ensure_neon_worker_agent().await;
    emit_log(
        event_tx,
        session.id,
        LogLevel::Info,
        "red_gate",
        format!("Using Managed Agent worker '{worker_agent_id}'"),
    );

    let red_gate = match session.intake.task_kind {
        TaskKind::Bug => run_bug_red_gate(gemini, session, repo, &worker_agent_id, event_tx).await,
        TaskKind::Feature => run_feature_red_gate(gemini, session, repo, event_tx).await,
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

    Ok(RedGateStageResult {
        worker_agent_id,
        red_gate,
    })
}

async fn run_bug_red_gate(
    gemini: &GeminiClient,
    session: &TournamentSession,
    repo: &RepoSnapshot,
    worker_agent_id: &str,
    event_tx: &Option<broadcast::Sender<LiveEvent>>,
) -> RedGateSpec {
    let clone_url = normalize_clone_url(&session.intake.repo_url);
    let mut last_failure_context = String::new();
    let mut last_env_id: Option<String> = None;

    // Attempts 1 & 2: Neon Managed Agent in remote sandbox
    for attempt in 1..=2u8 {
        emit_log(
            event_tx,
            session.id,
            LogLevel::Info,
            "red_gate",
            format!("Bug Red-Gate Neon attempt {attempt}/2 in remote sandbox"),
        );

        let checkout_instruction = match (&session.intake.commit_sha, &session.intake.git_ref) {
            (Some(sha), _) if !sha.trim().is_empty() => {
                format!("First run `git checkout {}` inside /workspace/repo.", sha.trim())
            }
            (_, Some(r)) if !r.trim().is_empty() => {
                format!("First run `git checkout {}` inside /workspace/repo.", r.trim())
            }
            _ => "Work on the checked-out default HEAD inside /workspace/repo.".to_string(),
        };

        let retry_hint = if last_failure_context.is_empty() {
            String::new()
        } else {
            format!(
                "\nPrevious attempt note (fix this issue and try a cleaner standalone test):\n{}\n",
                truncate_str(&last_failure_context, 800)
            )
        };

        let prompt = format!(
            "You are Stage 2 (Locked Red-Gate) of the Epicenter engineering tournament.\n\
             {checkout_instruction}\n\
             Your SOLE objective is to write a standalone reproduction test file for the bug below\n\
             WITHOUT modifying any production source code, and verify that:\n\
             1. Existing baseline tests still pass.\n\
             2. Your new reproduction test FAILS on unmodified HEAD, reproducing the exact bug.\n\
             3. Generate `git diff` (staged/tracked via `git add -N <path> && git diff`) for ONLY the new reproduction test file.\n\n\
             Issue Title: {}\n\
             Problem Statement:\n{}\n\
             Repro Steps: {:?}\n\
             Suspect Files: {:?}\n\
             Multimodal Findings: {:?}\n\
             {retry_hint}\n\
             Respond with STRICT JSON ONLY:\n\
             {{\n\
               \"repro_test_path\": \"relative/path/to/test_repro...\",\n\
               \"repro_test_patch\": \"diff --git a/... b/...\\n...\",\n\
               \"test_command\": \"exact command to run the repro test\",\n\
               \"verified_fails_on_head\": true,\n\
               \"failure_excerpt\": \"concise failing assertion/traceback excerpt\"\n\
             }}",
            session.intake.title,
            session.intake.problem_statement,
            session.intake.repro_steps,
            session.intake.suspect_files,
            session.intake.multimodal_findings,
        );

        let mut req = CreateInteractionRequest::for_agent(worker_agent_id, prompt);
        req.environment = Some(InteractionEnvironment::remote_repo(
            clone_url.clone(),
            session.intake.git_ref.clone(),
            session.intake.commit_sha.clone(),
            default_sandbox_allowlist(),
        ));
        req.background = Some(true);

        match gemini
            .run_background_interaction(req, RED_GATE_POLL_INTERVAL, RED_GATE_TIMEOUT)
            .await
        {
            Ok(resp) => {
                if let Some(eid) = resp.resolved_environment_id() {
                    last_env_id = Some(eid.to_string());
                }
                let raw = resp.extract_text();
                match extract_json_from_text::<BugRedGateOutput>(&raw) {
                    Ok(out)
                        if out.verified_fails_on_head
                            && !out.repro_test_path.trim().is_empty()
                            && !out.repro_test_patch.trim().is_empty() =>
                    {
                        emit_log(
                            event_tx,
                            session.id,
                            LogLevel::Info,
                            "red_gate",
                            format!(
                                "Neon verified failing reproduction test '{}' on attempt {attempt}",
                                out.repro_test_path
                            ),
                        );
                        return RedGateSpec::Bug(BugRedGate {
                            repro_test_path: out.repro_test_path.trim().to_string(),
                            repro_test_patch: out.repro_test_patch,
                            test_command: default_test_cmd(&out.test_command, &out.repro_test_path),
                            verified_fails_on_head: true,
                            failure_excerpt: truncate_str(&out.failure_excerpt, 1500),
                            base_environment_id: last_env_id,
                            resolution: RedGateResolution::VerifiedNeon,
                        });
                    }
                    Ok(out) => {
                        last_failure_context = format!(
                            "verified_fails_on_head={}, path='{}', excerpt='{}'",
                            out.verified_fails_on_head,
                            out.repro_test_path,
                            truncate_str(&out.failure_excerpt, 400)
                        );
                        warn!(attempt, context = %last_failure_context, "Neon red-gate attempt did not verify failure on HEAD");
                    }
                    Err(err) => {
                        last_failure_context = format!("JSON parse error: {err}; tail: {}", truncate_str(&raw, 400));
                        warn!(attempt, error = %err, "Neon red-gate attempt returned non-JSON");
                    }
                }
            }
            Err(err) => {
                last_failure_context = format!("Sandbox execution error: {err}");
                warn!(attempt, error = %err, "Neon red-gate attempt failed");
            }
        }
    }

    // Attempt 3: Escalate 1 deep attempt to Gemini 4 Carbon
    emit_log(
        event_tx,
        session.id,
        LogLevel::Warn,
        "red_gate",
        "Neon failed twice to lock reproduction test; escalating 1 deep attempt to Gemini 4 Carbon",
    );

    let carbon_prompt = format!(
        "You are Gemini 4 Carbon acting as Stage 2 Red-Gate Escalation Architect.\n\
         Two automated sandbox attempts failed to lock a reproduction test for the bug below.\n\
         Prior attempt context:\n{}\n\n\
         Inspect the issue description and source files carefully. Craft a precise standalone\n\
         reproduction test patch that exposes the exact defect on unmodified HEAD.\n\n\
         Issue Title: {}\n\
         Problem Statement:\n{}\n\
         Repro Steps: {:?}\n\
         Suspect Files: {:?}\n\n\
         Repository Sources:\n{}\n\n\
         Return STRICT JSON matching the schema.",
        truncate_str(&last_failure_context, 1000),
        session.intake.title,
        session.intake.problem_statement,
        session.intake.repro_steps,
        session.intake.suspect_files,
        repo.prompt_source_context(&session.intake.suspect_files, 32_000),
    );

    let mut carbon_req = CreateInteractionRequest::for_model(Gemini4Model::Carbon, carbon_prompt);
    carbon_req.system_instruction = Some(
        "You are Gemini 4 Carbon synthesizing an exact, minimal failing reproduction test patch. Return strict JSON."
            .to_string(),
    );
    carbon_req.response_format = Some(ResponseFormat::json_schema(bug_red_gate_schema()));

    if let Ok(resp) = gemini.create_interaction(&carbon_req).await {
        let raw = resp.extract_text();
        if let Ok(out) = extract_json_from_text::<BugRedGateOutput>(&raw) {
            if !out.repro_test_path.trim().is_empty() && !out.repro_test_patch.trim().is_empty() {
                // Verify Carbon's patch once inside the remote sandbox when possible; if sandbox verification
                // cannot execute, preserve Carbon's escalated red-gate honestly.
                let (verified, excerpt, env_id) = verify_carbon_repro_in_sandbox(
                    gemini,
                    worker_agent_id,
                    &clone_url,
                    session,
                    &out,
                    last_env_id.clone(),
                )
                .await;

                if verified {
                    emit_log(
                        event_tx,
                        session.id,
                        LogLevel::Info,
                        "red_gate",
                        format!(
                            "Carbon escalation verified failing reproduction test '{}'",
                            out.repro_test_path
                        ),
                    );
                    return RedGateSpec::Bug(BugRedGate {
                        repro_test_path: out.repro_test_path.trim().to_string(),
                        repro_test_patch: out.repro_test_patch,
                        test_command: default_test_cmd(&out.test_command, &out.repro_test_path),
                        verified_fails_on_head: true,
                        failure_excerpt: truncate_str(&excerpt, 1500),
                        base_environment_id: env_id.or(last_env_id),
                        resolution: RedGateResolution::EscalatedCarbon,
                    });
                }

                // Graceful CharacterizationFallback with Carbon's characterization patch
                emit_log(
                    event_tx,
                    session.id,
                    LogLevel::Warn,
                    "red_gate",
                    format!(
                        "Could not CLI-verify failure on HEAD for '{}'; falling back to CharacterizationFallback",
                        out.repro_test_path
                    ),
                );
                return RedGateSpec::Bug(BugRedGate {
                    repro_test_path: out.repro_test_path.trim().to_string(),
                    repro_test_patch: out.repro_test_patch,
                    test_command: default_test_cmd(&out.test_command, &out.repro_test_path),
                    verified_fails_on_head: false,
                    failure_excerpt: if excerpt.is_empty() {
                        format!(
                            "Characterization fallback (non-CLI-reproducible on HEAD): {}",
                            truncate_str(&last_failure_context, 400)
                        )
                    } else {
                        truncate_str(&excerpt, 1500)
                    },
                    base_environment_id: env_id.or(last_env_id),
                    resolution: RedGateResolution::CharacterizationFallback,
                });
            }
        }
    }

    // Final guaranteed graceful CharacterizationFallback so runs never hard-crash
    emit_log(
        event_tx,
        session.id,
        LogLevel::Warn,
        "red_gate",
        "Falling back to synthetic CharacterizationFallback Red-Gate so tournament proceeds",
    );
    RedGateSpec::Bug(BugRedGate {
        repro_test_path: "tests/test_epicenter_characterization.py".to_string(),
        repro_test_patch: String::new(),
        test_command: "pytest -q".to_string(),
        verified_fails_on_head: false,
        failure_excerpt: format!(
            "Characterization fallback: CLI reproduction unverified on HEAD ({})",
            truncate_str(&last_failure_context, 400)
        ),
        base_environment_id: last_env_id,
        resolution: RedGateResolution::CharacterizationFallback,
    })
}

async fn verify_carbon_repro_in_sandbox(
    gemini: &GeminiClient,
    worker_agent_id: &str,
    clone_url: &str,
    session: &TournamentSession,
    candidate: &BugRedGateOutput,
    existing_env_id: Option<String>,
) -> (bool, String, Option<String>) {
    let prompt = format!(
        "Apply the following reproduction test patch inside /workspace/repo without changing production code,\n\
         run `{}`, and confirm whether the reproduction test FAILS on unmodified HEAD.\n\n\
         Patch:\n```diff\n{}\n```\n\n\
         Return STRICT JSON ONLY:\n\
         {{\"verified_fails_on_head\": true/false, \"failure_excerpt\": \"...\"}}",
        default_test_cmd(&candidate.test_command, &candidate.repro_test_path),
        candidate.repro_test_patch,
    );

    let mut req = CreateInteractionRequest::for_agent(worker_agent_id, prompt);
    req.environment = Some(if let Some(eid) = existing_env_id.clone() {
        InteractionEnvironment::existing_remote(eid)
    } else {
        InteractionEnvironment::remote_repo(
            clone_url.to_string(),
            session.intake.git_ref.clone(),
            session.intake.commit_sha.clone(),
            default_sandbox_allowlist(),
        )
    });
    req.background = Some(true);

    #[derive(Deserialize)]
    struct VerifyOut {
        #[serde(default)]
        verified_fails_on_head: bool,
        #[serde(default)]
        failure_excerpt: String,
    }

    match gemini
        .run_background_interaction(req, RED_GATE_POLL_INTERVAL, Duration::from_secs(180))
        .await
    {
        Ok(resp) => {
            let env_id = resp.resolved_environment_id().map(String::from).or(existing_env_id);
            if let Ok(v) = extract_json_from_text::<VerifyOut>(&resp.extract_text()) {
                let excerpt = if v.failure_excerpt.is_empty() {
                    candidate.failure_excerpt.clone()
                } else {
                    v.failure_excerpt
                };
                return (v.verified_fails_on_head, excerpt, env_id);
            }
            (false, candidate.failure_excerpt.clone(), env_id)
        }
        Err(err) => {
            warn!(error = %err, "Carbon red-gate sandbox verification failed");
            (false, candidate.failure_excerpt.clone(), existing_env_id)
        }
    }
}

async fn run_feature_red_gate(
    gemini: &GeminiClient,
    session: &TournamentSession,
    repo: &RepoSnapshot,
    event_tx: &Option<broadcast::Sender<LiveEvent>>,
) -> RedGateSpec {
    emit_log(
        event_tx,
        session.id,
        LogLevel::Info,
        "red_gate",
        "Synthesizing FeatureRedGate acceptance contract via Gemini 4 Carbon",
    );

    let prompt = format!(
        "You are Stage 2 (Feature Acceptance Red-Gate) of the Epicenter engineering tournament.\n\
         Design a crisp acceptance test patch and benchmark entrypoint contract so all competing\n\
         strategies implement the exact same public API and pass the exact same acceptance criteria.\n\n\
         Feature Title: {}\n\
         Problem Statement:\n{}\n\
         Requirements / Steps: {:?}\n\
         Suspect Files: {:?}\n\n\
         Repository Sources:\n{}\n\n\
         Return STRICT JSON matching the schema.",
        session.intake.title,
        session.intake.problem_statement,
        session.intake.repro_steps,
        session.intake.suspect_files,
        repo.prompt_source_context(&session.intake.suspect_files, 32_000),
    );

    let mut req = CreateInteractionRequest::for_model(Gemini4Model::Carbon, prompt);
    req.system_instruction = Some(
        "You are Gemini 4 Carbon authoring a strict FeatureRedGate contract. Always return valid JSON."
            .to_string(),
    );
    req.response_format = Some(ResponseFormat::json_schema(feature_red_gate_schema()));

    if let Ok(resp) = gemini.create_interaction(&req).await {
        if let Ok(out) = extract_json_from_text::<FeatureRedGateOutput>(&resp.extract_text()) {
            return RedGateSpec::Feature(FeatureRedGate {
                acceptance_test_patch: out.acceptance_test_patch,
                benchmark_entrypoint_contract: if out.benchmark_entrypoint_contract.trim().is_empty() {
                    format!("Benchmark public entrypoint for '{}'", session.intake.title)
                } else {
                    out.benchmark_entrypoint_contract
                },
                test_command: if out.test_command.trim().is_empty() {
                    "pytest -q".to_string()
                } else {
                    out.test_command
                },
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
        test_command: "pytest -q".to_string(),
        base_environment_id: None,
    })
}

fn default_test_cmd(cmd: &str, repro_path: &str) -> String {
    let trimmed = cmd.trim();
    if !trimmed.is_empty() {
        return trimmed.to_string();
    }
    if repro_path.ends_with(".py") {
        format!("pytest -q {repro_path}")
    } else if repro_path.ends_with(".ts") || repro_path.ends_with(".js") {
        format!("npx vitest run {repro_path}")
    } else if repro_path.ends_with(".rs") {
        "cargo test".to_string()
    } else if repro_path.ends_with(".go") {
        "go test ./...".to_string()
    } else {
        "pytest -q".to_string()
    }
}

fn bug_red_gate_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "repro_test_path": { "type": "string" },
            "repro_test_patch": { "type": "string" },
            "test_command": { "type": "string" },
            "verified_fails_on_head": { "type": "boolean" },
            "failure_excerpt": { "type": "string" }
        },
        "required": ["repro_test_path", "repro_test_patch", "test_command", "failure_excerpt"]
    })
}

fn feature_red_gate_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "acceptance_test_patch": { "type": "string" },
            "benchmark_entrypoint_contract": { "type": "string" },
            "test_command": { "type": "string" }
        },
        "required": ["acceptance_test_patch", "benchmark_entrypoint_contract", "test_command"]
    })
}
