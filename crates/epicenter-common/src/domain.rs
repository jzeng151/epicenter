use crate::blast::BlastRadiusReport;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

pub const MIN_STRATEGIES: usize = 1;
pub const MAX_STRATEGIES: usize = 5;

#[derive(Debug, Error)]
pub enum CommonError {
    #[error("strategy plan must contain between {MIN_STRATEGIES} and {MAX_STRATEGIES} strategies, got {0}")]
    InvalidStrategyCount(usize),
    #[error("duplicate strategy id detected: '{0}'")]
    DuplicateStrategyId(String),
}

/// High-level engineering task classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    Bug,
    Feature,
}

/// Red-gate specification for a bugfix task (failing reproduction test verified on HEAD).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BugRedGate {
    pub repro_test_path: String,
    pub repro_test_patch: String,
    pub test_command: String,
    pub verified_fails_on_head: bool,
    pub failure_excerpt: String,
    pub base_environment_id: Option<String>,
}

/// Red-gate specification for a feature/optimization task (acceptance test + benchmark contract).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeatureRedGate {
    pub acceptance_test_patch: String,
    pub benchmark_entrypoint_contract: String,
    pub test_command: String,
    pub base_environment_id: Option<String>,
}

/// Unified Red-Gate contract ensuring all tournament candidates solve the exact same failing test.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RedGateSpec {
    Bug(BugRedGate),
    Feature(FeatureRedGate),
}

impl RedGateSpec {
    pub const fn task_kind(&self) -> TaskKind {
        match self {
            Self::Bug(_) => TaskKind::Bug,
            Self::Feature(_) => TaskKind::Feature,
        }
    }

    pub fn test_command(&self) -> &str {
        match self {
            Self::Bug(b) => &b.test_command,
            Self::Feature(f) => &f.test_command,
        }
    }

    pub fn base_environment_id(&self) -> Option<&str> {
        match self {
            Self::Bug(b) => b.base_environment_id.as_deref(),
            Self::Feature(f) => f.base_environment_id.as_deref(),
        }
    }

    pub fn patch(&self) -> &str {
        match self {
            Self::Bug(b) => &b.repro_test_patch,
            Self::Feature(f) => &f.acceptance_test_patch,
        }
    }
}

/// Supported multimodal input attachment categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    ScreenRecording,
    Screenshot,
    Pdf,
    VoiceNote,
    Log,
    Other,
}

/// Metadata for an uploaded multimodal attachment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentMeta {
    pub filename: String,
    pub mime_type: String,
    pub media_kind: MediaKind,
    pub size_bytes: u64,
    pub file_uri: Option<String>,
}

/// Structured observation extracted from a screen recording, screenshot, PDF, or voice note.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultimodalFinding {
    pub source_name: String,
    pub media_kind: MediaKind,
    pub timestamp_hint: Option<String>,
    pub summary: String,
    pub extracted_symbols: Vec<String>,
}

/// Structured intake brief synthesized from user prompt + multimodal evidence + target repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntakeBrief {
    pub id: Uuid,
    pub task_kind: TaskKind,
    pub repo_url: String,
    pub commit_sha: Option<String>,
    pub git_ref: Option<String>,
    pub title: String,
    pub problem_statement: String,
    pub repro_steps: Vec<String>,
    pub multimodal_findings: Vec<MultimodalFinding>,
    pub attachments: Vec<AttachmentMeta>,
    pub created_at: DateTime<Utc>,
}

/// Shared benchmark harness specification executed across every passing candidate in the Arena.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BenchmarkHarnessSpec {
    pub name: String,
    pub command: String,
    pub setup_command: Option<String>,
    pub trials: u32,
    pub warmup_trials: u32,
    pub metric_unit: String,
    pub entrypoint_contract: Option<String>,
    pub script_content: Option<String>,
}

/// An individual architectural approach competing in the tournament.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StrategySpec {
    pub id: String,
    pub title: String,
    pub thesis: String,
    pub tradeoff_axis: String,
    pub target_files_hint: Vec<String>,
    pub risk_hypothesis: String,
}

/// Dynamic K (`1..=5`) strategy tournament plan produced before spawning Managed Agent workers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StrategyPlan {
    pub degree_of_freedom_rationale: String,
    pub strategies: Vec<StrategySpec>,
    pub shared_benchmark_spec: BenchmarkHarnessSpec,
}

impl StrategyPlan {
    /// Validates that `strategies.len()` is within `1..=5` and that all strategy IDs are unique.
    pub fn validate(&self) -> Result<(), CommonError> {
        let len = self.strategies.len();
        if !(MIN_STRATEGIES..=MAX_STRATEGIES).contains(&len) {
            return Err(CommonError::InvalidStrategyCount(len));
        }
        let mut seen = std::collections::HashSet::with_capacity(len);
        for strategy in &self.strategies {
            if !seen.insert(strategy.id.as_str()) {
                return Err(CommonError::DuplicateStrategyId(strategy.id.clone()));
            }
        }
        Ok(())
    }

    /// Truncates the strategy list to at most `MAX_STRATEGIES` (5).
    pub fn clamped(mut self) -> Self {
        if self.strategies.len() > MAX_STRATEGIES {
            self.strategies.truncate(MAX_STRATEGIES);
        }
        self
    }
}

/// Execution lifecycle state of a candidate strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateStatus {
    Pending,
    Running,
    PassedTests,
    FailedTests,
    BenchmarkDone,
    Audited,
}

/// Interleaved benchmark timing and resource measurements for one candidate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CandidateArenaMetrics {
    pub baseline_samples_ms: Vec<f64>,
    pub candidate_samples_ms: Vec<f64>,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub speedup_pct: f64,
    pub peak_memory_kb: Option<u64>,
    pub call_or_query_count: Option<u64>,
}

impl CandidateArenaMetrics {
    /// Computes p50, p95, and speedup percentage from interleaved baseline & candidate trial timings.
    pub fn from_interleaved_samples(
        baseline_samples_ms: Vec<f64>,
        candidate_samples_ms: Vec<f64>,
        peak_memory_kb: Option<u64>,
        call_or_query_count: Option<u64>,
    ) -> Self {
        let baseline_p50 = percentile_ms(&baseline_samples_ms, 50.0);
        let p50_ms = percentile_ms(&candidate_samples_ms, 50.0);
        let p95_ms = percentile_ms(&candidate_samples_ms, 95.0);
        let speedup_pct = if baseline_p50 > 0.0 {
            ((baseline_p50 - p50_ms) / baseline_p50) * 100.0
        } else {
            0.0
        };
        Self {
            baseline_samples_ms,
            candidate_samples_ms,
            p50_ms,
            p95_ms,
            speedup_pct,
            peak_memory_kb,
            call_or_query_count,
        }
    }
}

/// Tournament-wide Arena benchmark summary across all candidates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArenaSummary {
    pub benchmark_name: String,
    pub baseline_p50_ms: f64,
    pub baseline_p95_ms: f64,
    pub trials_per_candidate: u32,
    pub pareto_frontier_ids: Vec<String>,
    pub recommended_strategy_id: Option<String>,
    pub summary_notes: String,
}

/// Full execution, verification, benchmark, and blast-radius result for a single strategy candidate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CandidateResult {
    pub strategy_id: String,
    pub status: CandidateStatus,
    pub git_diff: String,
    pub repro_passed: bool,
    pub regression_suite_passed: bool,
    pub test_output_tail: String,
    pub arena_metrics: Option<CandidateArenaMetrics>,
    pub blast_radius: Option<BlastRadiusReport>,
    pub pareto_optimal: bool,
}

impl CandidateResult {
    pub fn pending(strategy_id: impl Into<String>) -> Self {
        Self {
            strategy_id: strategy_id.into(),
            status: CandidateStatus::Pending,
            git_diff: String::new(),
            repro_passed: false,
            regression_suite_passed: false,
            test_output_tail: String::new(),
            arena_metrics: None,
            blast_radius: None,
            pareto_optimal: false,
        }
    }

    pub fn is_green(&self) -> bool {
        self.repro_passed && self.regression_suite_passed
    }
}

/// High-level pipeline stages for a tournament session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TournamentStage {
    Intake,
    RedGate,
    Planning,
    Executing,
    Benchmarking,
    Auditing,
    Completed,
    Failed,
}

/// Top-level persisted tournament session state shared between server and Leptos UI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TournamentSession {
    pub id: Uuid,
    pub stage: TournamentStage,
    pub intake: IntakeBrief,
    pub red_gate: Option<RedGateSpec>,
    pub strategy_plan: Option<StrategyPlan>,
    pub candidates: Vec<CandidateResult>,
    pub arena_summary: Option<ArenaSummary>,
    pub audio_briefing_url: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Computes an interpolated percentile (e.g. `50.0` for p50, `95.0` for p95) over millisecond samples.
pub fn percentile_ms(samples: &[f64], pct: f64) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let mut sorted: Vec<f64> = samples.iter().copied().filter(|v| v.is_finite()).collect();
    if sorted.is_empty() {
        return 0.0;
    }
    sorted.sort_by(|a, b| a.total_cmp(b));
    let clamped = pct.clamp(0.0, 100.0);
    let rank = (clamped / 100.0) * ((sorted.len() - 1) as f64);
    let lower = rank.floor() as usize;
    let upper = rank.ceil() as usize;
    if lower == upper {
        sorted[lower]
    } else {
        let weight = rank - (lower as f64);
        sorted[lower] * (1.0 - weight) + sorted[upper] * weight
    }
}

/// Marks Pareto-optimal candidates in-place (higher `speedup_pct` and lower `composite_risk_score`
/// among test-passing candidates) and returns their strategy IDs.
pub fn compute_pareto_frontier(candidates: &mut [CandidateResult]) -> Vec<String> {
    for c in candidates.iter_mut() {
        c.pareto_optimal = false;
    }

    let eligible: Vec<(usize, f64, f32)> = candidates
        .iter()
        .enumerate()
        .filter(|(_, c)| c.is_green())
        .map(|(idx, c)| {
            let speedup = c
                .arena_metrics
                .as_ref()
                .map(|m| m.speedup_pct)
                .unwrap_or(0.0);
            let risk = c
                .blast_radius
                .as_ref()
                .map(|b| b.composite_risk_score)
                .unwrap_or(50.0);
            (idx, speedup, risk)
        })
        .collect();

    let mut frontier_ids = Vec::new();
    for &(i, sp_i, risk_i) in &eligible {
        let dominated = eligible.iter().any(|&(j, sp_j, risk_j)| {
            if i == j {
                return false;
            }
            let at_least_as_good = sp_j >= sp_i && risk_j <= risk_i;
            let strictly_better = sp_j > sp_i || risk_j < risk_i;
            at_least_as_good && strictly_better
        });
        if !dominated {
            candidates[i].pareto_optimal = true;
            frontier_ids.push(candidates[i].strategy_id.clone());
        }
    }

    frontier_ids
}
