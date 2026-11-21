use crate::repo::{
    default_sandbox_allowlist, emit_event, emit_log, extract_json_from_text, normalize_clone_url,
    truncate_str,
};
use epicenter_common::{
    percentile_ms, ArenaSummary, BenchmarkHarnessSpec, CandidateArenaMetrics, CandidateStatus,
    LiveEvent, LogLevel, RedGateSpec, TournamentSession, TournamentStage,
};
use epicenter_gemini::{CreateInteractionRequest, GeminiClient, InteractionEnvironment};
use epicenter_graph::CoverageMap;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::time::Duration;
use tokio::sync::broadcast;
use tracing::warn;

const ARENA_POLL_INTERVAL: Duration = Duration::from_secs(3);
const ARENA_TIMEOUT: Duration = Duration::from_secs(360);

#[derive(Debug, Deserialize)]
struct ArenaSandboxOutput {
    #[serde(default)]
    baseline_samples_ms: Vec<f64>,
    #[serde(default)]
    candidates: Vec<ArenaCandidateOutput>,
    #[serde(default)]
    summary_notes: String,
}

#[derive(Debug, Deserialize)]
struct ArenaCandidateOutput {
    strategy_id: String,
    #[serde(default)]
    baseline_samples_ms: Vec<f64>,
    #[serde(default)]
    candidate_samples_ms: Vec<f64>,
    #[serde(default)]
    peak_memory_kb: Option<u64>,
    #[serde(default)]
    call_or_query_count: Option<u64>,
    #[serde(default)]
    coverage_json: Option<String>,
    #[serde(default)]
    lcov_info: Option<String>,
}

/// Output of Stage 5 Arena: aggregate `ArenaSummary` and per-candidate parsed `CoverageMap`s.
#[derive(Debug)]
pub struct ArenaStageResult {
    pub summary: ArenaSummary,
    pub coverage_maps: BTreeMap<String, CoverageMap>,
}

/// Stage 5: Single-VM Interleaved Benchmark Arena.
///
/// Launches ONE single remote Managed Agent sandbox running interleaved trials
/// (`Baseline -> Candidate_1 -> ... -> Candidate_M` repeated `trials` times) on the exact same VM,
/// eliminating cross-container CPU jitter, while collecting `baseline_samples_ms`,
/// `candidate_samples_ms`, `peak_memory_kb`, `call_or_query_count`, and per-candidate coverage
/// (`coverage.json` or `lcov.info`).
pub async fn run_arena_stage(
    gemini: &GeminiClient,
    session: &mut TournamentSession,
    worker_agent_id: &str,
    red_gate: &RedGateSpec,
    bench_spec: &BenchmarkHarnessSpec,
    worker_coverage_raw: &BTreeMap<String, String>,
    event_tx: &Option<broadcast::Sender<LiveEvent>>,
) -> anyhow::Result<ArenaStageResult> {
    session.stage = TournamentStage::Benchmarking;
    session.updated_at = chrono::Utc::now();

    let green_indices: Vec<usize> = session
        .candidates
        .iter()
        .enumerate()
        .filter(|(_, c)| c.is_green())
        .map(|(i, _)| i)
        .collect();

    emit_event(
        event_tx,
        LiveEvent::StageChanged {
            session_id: session.id,
            stage: TournamentStage::Benchmarking,
            message: format!(
                "Stage 5 — Single-VM Interleaved Arena ({} passing candidates × {} trials)",
                green_indices.len(),
                bench_spec.trials
            ),
        },
    );

    // Seed coverage maps from any worker-provided coverage reports first
    let mut coverage_maps = parse_raw_coverage_map(worker_coverage_raw);

    if green_indices.is_empty() {
        let summary = ArenaSummary {
            benchmark_name: bench_spec.name.clone(),
            baseline_p50_ms: 0.0,
            baseline_p95_ms: 0.0,
            trials_per_candidate: bench_spec.trials,
            pareto_frontier_ids: Vec::new(),
            recommended_strategy_id: None,
            summary_notes: "No candidates passed Red-Gate + regression verification; Arena skipped."
                .to_string(),
        };
        session.arena_summary = Some(summary.clone());
        session.updated_at = chrono::Utc::now();
        emit_event(
            event_tx,
            LiveEvent::ArenaCompleted {
                session_id: session.id,
                summary: summary.clone(),
            },
        );
        return Ok(ArenaStageResult {
            summary,
            coverage_maps,
        });
    }

    let clone_url = normalize_clone_url(&session.intake.repo_url);
    let trials = bench_spec.trials.clamp(3, 15);
    let warmup = bench_spec.warmup_trials.min(3);

    let candidate_blocks = green_indices
        .iter()
        .map(|&idx| {
            let c = &session.candidates[idx];
            format!(
                "### CANDIDATE `{}`\n```diff\n{}\n```",
                c.strategy_id,
                truncate_str(&c.git_diff, 14_000)
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    let checkout_step = match (&session.intake.commit_sha, &session.intake.git_ref) {
        (Some(sha), _) if !sha.trim().is_empty() => format!("Run `git checkout {}` first.", sha.trim()),
        (_, Some(r)) if !r.trim().is_empty() => format!("Run `git checkout {}` first.", r.trim()),
        _ => "Use the checked-out HEAD in /workspace/repo.".to_string(),
    };

    let prompt = format!(
        "You are Stage 5 (Single-VM Interleaved Benchmark Arena) of the Epicenter tournament.\n\
         {checkout_step}\n\
         You are running inside ONE dedicated VM so every measurement shares identical CPU/memory hardware.\n\n\
         BENCHMARK HARNESS:\n\
         - Name: {name}\n\
         - Setup Command: {setup}\n\
         - Benchmark Command: {cmd}\n\
         - Warmup Trials: {warmup}\n\
         - Measured Interleaved Trials: {trials}\n\
         - Metric Unit: {unit}\n\
         - Optional Script:\n{script}\n\n\
         RED-GATE TEST COMMAND (for per-candidate coverage capture): {test_cmd}\n\
         LOCKED RED-GATE REPRO PATCH (save to `/tmp/repro_gate.patch` and apply alongside each candidate during step 4 coverage capture if non-empty):\n\
         ```diff\n{repro_patch}\n```\n\n\
         PASSING CANDIDATE PATCHES:\n\
         {candidate_blocks}\n\n\
         STRICT INTERLEAVED PROTOCOL:\n\
         1. Save each candidate's unified diff to `/tmp/<strategy_id>.patch` (and save the locked Red-Gate repro patch to `/tmp/repro_gate.patch` if non-empty).\n\
         2. Run `{warmup}` warmup iterations.\n\
         3. For trial `t = 0 .. {trials}`:\n\
            - Ensure clean baseline (`git checkout -- . && git clean -fd`), time the benchmark command in milliseconds -> append to `baseline_samples_ms`.\n\
            - For EACH candidate in order: `git apply /tmp/<strategy_id>.patch`, time the benchmark command in milliseconds (and capture `peak_memory_kb` and optional `call_or_query_count`), then revert (`git checkout -- . && git clean -fd`).\n\
         4. After timing trials, for each candidate apply `/tmp/repro_gate.patch` (if non-empty) + `/tmp/<strategy_id>.patch`, run `{test_cmd}` with coverage (`python3 -m coverage run -m pytest ... && python3 -m coverage json -o -` or `lcov.info` if available) and embed the coverage string (or null if unavailable), then revert.\n\n\
         Return STRICT JSON ONLY:\n\
         {{\n\
           \"baseline_samples_ms\": [100.2, 99.8, ...],\n\
           \"candidates\": [\n\
             {{\n\
               \"strategy_id\": \"...\",\n\
               \"baseline_samples_ms\": [100.2, 99.8, ...],\n\
               \"candidate_samples_ms\": [54.1, 53.7, ...],\n\
               \"peak_memory_kb\": 18400,\n\
               \"call_or_query_count\": 4,\n\
               \"coverage_json\": null,\n\
               \"lcov_info\": null\n\
             }}\n\
           ],\n\
           \"summary_notes\": \"concise single-VM benchmark summary\"\n\
         }}",
        name = bench_spec.name,
        setup = bench_spec.setup_command.as_deref().unwrap_or("(none)"),
        cmd = bench_spec.command,
        unit = bench_spec.metric_unit,
        script = bench_spec
            .script_content
            .as_deref()
            .unwrap_or("(none — run benchmark command directly)"),
        test_cmd = red_gate.test_command(),
        repro_patch = truncate_str(red_gate.patch(), 8_000),
    );

    let mut req = CreateInteractionRequest::for_agent(worker_agent_id, prompt);
    req.environment = Some(InteractionEnvironment::remote_repo(
        clone_url,
        session.intake.git_ref.clone(),
        session.intake.commit_sha.clone(),
        default_sandbox_allowlist(),
    ));
    req.background = Some(true);

    let mut baseline_all: Vec<f64> = Vec::new();
    let mut summary_notes;

    match gemini
        .run_background_interaction(req, ARENA_POLL_INTERVAL, ARENA_TIMEOUT)
        .await
    {
        Ok(resp) => {
            let text = resp.extract_text();
            match extract_json_from_text::<ArenaSandboxOutput>(&text) {
                Ok(parsed) => {
                    baseline_all = parsed.baseline_samples_ms.clone();
                    summary_notes = parsed.summary_notes;

                    for entry in parsed.candidates {
                        if let Some(cov_json) = entry.coverage_json.as_deref() {
                            if let Ok(cm) = CoverageMap::from_coverage_json(cov_json) {
                                coverage_maps.insert(entry.strategy_id.clone(), cm);
                            }
                        } else if let Some(lcov) = entry.lcov_info.as_deref() {
                            if !lcov.trim().is_empty() {
                                coverage_maps
                                    .insert(entry.strategy_id.clone(), CoverageMap::from_lcov(lcov));
                            }
                        }

                        let base_samples = if entry.baseline_samples_ms.is_empty() {
                            baseline_all.clone()
                        } else {
                            entry.baseline_samples_ms
                        };
                        if baseline_all.is_empty() && !base_samples.is_empty() {
                            baseline_all = base_samples.clone();
                        }

                        if let Some(cand) = session
                            .candidates
                            .iter_mut()
                            .find(|c| c.strategy_id == entry.strategy_id)
                        {
                            for (trial_idx, &cand_ms) in
                                entry.candidate_samples_ms.iter().enumerate()
                            {
                                let base_ms = base_samples
                                    .get(trial_idx)
                                    .copied()
                                    .or_else(|| base_samples.last().copied())
                                    .unwrap_or(cand_ms);
                                emit_event(
                                    event_tx,
                                    LiveEvent::ArenaTrialSample {
                                        session_id: session.id,
                                        strategy_id: cand.strategy_id.clone(),
                                        trial_index: trial_idx as u32,
                                        baseline_ms: base_ms,
                                        candidate_ms: cand_ms,
                                    },
                                );
                            }

                            let metrics = CandidateArenaMetrics::from_interleaved_samples(
                                base_samples,
                                entry.candidate_samples_ms,
                                entry.peak_memory_kb,
                                entry.call_or_query_count,
                            );
                            cand.arena_metrics = Some(metrics.clone());
                            cand.status = CandidateStatus::BenchmarkDone;

                            emit_event(
                                event_tx,
                                LiveEvent::ArenaCandidateDone {
                                    session_id: session.id,
                                    strategy_id: cand.strategy_id.clone(),
                                    metrics,
                                },
                            );
                            emit_event(
                                event_tx,
                                LiveEvent::CandidateUpdated {
                                    session_id: session.id,
                                    candidate: Box::new(cand.clone()),
                                },
                            );
                        }
                    }
                }
                Err(err) => {
                    warn!(error = %err, "Arena sandbox JSON parse failed");
                    emit_log(
                        event_tx,
                        session.id,
                        LogLevel::Warn,
                        "arena",
                        format!("Arena output parse warning ({err}); continuing to Stage 6 audit"),
                    );
                    summary_notes = format!("Arena completed with parse fallback: {err}");
                }
            }
        }
        Err(err) => {
            warn!(error = %err, "Arena single-VM sandbox failed");
            emit_log(
                event_tx,
                session.id,
                LogLevel::Warn,
                "arena",
                format!("Arena sandbox error ({err}); continuing to Stage 6 blast-radius audit"),
            );
            summary_notes = format!("Arena sandbox encountered error: {err}");
        }
    }

    // Ensure every green candidate has a BenchmarkDone status even if Arena missed its entry
    for &idx in &green_indices {
        let cand = &mut session.candidates[idx];
        if cand.status == CandidateStatus::PassedTests {
            cand.status = CandidateStatus::BenchmarkDone;
        }
    }

    let baseline_p50_ms = percentile_ms(&baseline_all, 50.0);
    let baseline_p95_ms = percentile_ms(&baseline_all, 95.0);
    if summary_notes.trim().is_empty() {
        summary_notes = format!(
            "Single-VM interleaved Arena completed across {} passing candidates ({trials} trials, baseline p50={baseline_p50_ms:.2}ms).",
            green_indices.len()
        );
    }

    let summary = ArenaSummary {
        benchmark_name: bench_spec.name.clone(),
        baseline_p50_ms,
        baseline_p95_ms,
        trials_per_candidate: trials,
        pareto_frontier_ids: Vec::new(),
        recommended_strategy_id: None,
        summary_notes,
    };

    session.arena_summary = Some(summary.clone());
    session.updated_at = chrono::Utc::now();
    emit_event(
        event_tx,
        LiveEvent::ArenaCompleted {
            session_id: session.id,
            summary: summary.clone(),
        },
    );

    Ok(ArenaStageResult {
        summary,
        coverage_maps,
    })
}

fn parse_raw_coverage_map(raw_map: &BTreeMap<String, String>) -> BTreeMap<String, CoverageMap> {
    let mut parsed = BTreeMap::new();
    for (strategy_id, raw) in raw_map {
        let trimmed = raw.trim_start();
        if trimmed.starts_with('{') {
            if let Ok(cm) = CoverageMap::from_coverage_json(trimmed) {
                parsed.insert(strategy_id.clone(), cm);
            }
        } else if !trimmed.is_empty() {
            let cm = CoverageMap::from_lcov(trimmed);
            if !cm.is_empty() {
                parsed.insert(strategy_id.clone(), cm);
            }
        }
    }
    parsed
}
