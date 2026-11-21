use crate::repo::{emit_event, emit_log};
use crate::verifier::{ArenaCandidateInput, PreparedRepo};
use epicenter_common::{
    percentile_ms, ArenaSummary, BenchmarkHarnessSpec, CandidateArenaMetrics, CandidateStatus,
    LiveEvent, LogLevel, RedGateSpec, TournamentSession, TournamentStage,
};
use epicenter_graph::CoverageMap;
use std::collections::BTreeMap;
use tokio::sync::broadcast;
use tracing::warn;

/// Output of Stage 5 Arena: aggregate `ArenaSummary` and per-candidate parsed `CoverageMap`s.
#[derive(Debug)]
pub struct ArenaStageResult {
    pub summary: ArenaSummary,
    pub coverage_maps: BTreeMap<String, CoverageMap>,
}

/// Stage 5: Single-Host Interleaved Benchmark Arena + Per-Candidate Coverage Capture.
///
/// Runs inside ONE clean worktree on the host CPU via `PreparedRepo::run_interleaved_arena`,
/// interleaving `Baseline -> Candidate_1 -> ... -> Candidate_M` timed directly with
/// `std::time::Instant::now()` (zero self-reported LLM timings), then collects genuine line
/// coverage (`CoverageMap`) with the immutable locked Red-Gate patch applied.
pub async fn run_arena_stage(
    session: &mut TournamentSession,
    prepared: &PreparedRepo,
    red_gate: &RedGateSpec,
    bench_spec: &BenchmarkHarnessSpec,
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

    let clamped_trials = if bench_spec.trials == 0 {
        3
    } else {
        bench_spec.trials.clamp(3, 5)
    };

    emit_event(
        event_tx,
        LiveEvent::StageChanged {
            session_id: session.id,
            stage: TournamentStage::Benchmarking,
            message: format!(
                "Stage 5 — Single-Host Interleaved Arena ({} passing candidates × {} trials)",
                green_indices.len(),
                clamped_trials
            ),
        },
    );

    let mut coverage_maps = BTreeMap::new();

    if green_indices.is_empty() {
        let summary = ArenaSummary {
            benchmark_name: bench_spec.name.clone(),
            baseline_p50_ms: 0.0,
            baseline_p95_ms: 0.0,
            trials_per_candidate: clamped_trials,
            pareto_frontier_ids: Vec::new(),
            recommended_strategy_id: None,
            summary_notes: "No candidates passed independent Red-Gate + regression verification; Arena skipped."
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

    let inputs: Vec<ArenaCandidateInput> = green_indices
        .iter()
        .map(|&idx| ArenaCandidateInput {
            strategy_id: session.candidates[idx].strategy_id.clone(),
            git_diff: session.candidates[idx].git_diff.clone(),
        })
        .collect();

    let mut baseline_all: Vec<f64> = Vec::new();
    let mut trials_run = clamped_trials;
    let summary_notes: String;

    match prepared
        .run_interleaved_arena(
            bench_spec,
            red_gate.locked_test_path(),
            red_gate.patch(),
            red_gate.test_command(),
            &inputs,
            Some(session.id),
            event_tx.as_ref(),
        )
        .await
    {
        Ok(arena_res) => {
            baseline_all = arena_res.baseline_samples_ms;
            trials_run = arena_res.trials_run;
            summary_notes = arena_res.summary_notes;

            for entry in arena_res.candidates {
                if let Some(cm) = entry.coverage_map {
                    if !cm.is_empty() {
                        coverage_maps.insert(entry.strategy_id.clone(), cm);
                    }
                }

                if let Some(cand) = session
                    .candidates
                    .iter_mut()
                    .find(|c| c.strategy_id == entry.strategy_id)
                {
                    let metrics = CandidateArenaMetrics::from_interleaved_samples(
                        entry.baseline_samples_ms,
                        entry.candidate_samples_ms,
                        entry.peak_memory_kb,
                        None,
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

            emit_log(
                event_tx,
                session.id,
                LogLevel::Info,
                "arena",
                format!(
                    "Single-host Arena finished ({trials_run} trials, {} coverage maps captured via {})",
                    coverage_maps.len(),
                    arena_res.benchmark_used
                ),
            );
        }
        Err(err) => {
            warn!(error = %err, "Single-host Arena failed");
            emit_log(
                event_tx,
                session.id,
                LogLevel::Warn,
                "arena",
                format!("Host Arena error ({err}); continuing to Stage 6 blast-radius audit"),
            );
            summary_notes = format!("Host Arena encountered error: {err}");
        }
    }

    // Ensure every green candidate transitions cleanly even if benchmark errored.
    for &idx in &green_indices {
        let cand = &mut session.candidates[idx];
        if cand.status == CandidateStatus::PassedTests {
            cand.status = CandidateStatus::BenchmarkDone;
        }
    }

    let baseline_p50_ms = percentile_ms(&baseline_all, 50.0);
    let baseline_p95_ms = percentile_ms(&baseline_all, 95.0);
    let final_notes = if summary_notes.trim().is_empty() {
        format!(
            "Single-host interleaved Arena completed across {} passing candidates ({trials_run} trials, baseline p50={baseline_p50_ms:.2}ms).",
            green_indices.len()
        )
    } else {
        summary_notes
    };

    let summary = ArenaSummary {
        benchmark_name: bench_spec.name.clone(),
        baseline_p50_ms,
        baseline_p95_ms,
        trials_per_candidate: trials_run,
        pareto_frontier_ids: Vec::new(),
        recommended_strategy_id: None,
        summary_notes: final_notes,
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
