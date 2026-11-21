use crate::domain::percentile_ms;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Evaluation comparison arm for the 5-Sigma benchmark harness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvalArm {
    SingleModelCall,
    SingleManagedAgent,
    EpicenterTournament,
}

impl EvalArm {
    pub const fn all() -> &'static [EvalArm] {
        &[
            Self::SingleModelCall,
            Self::SingleManagedAgent,
            Self::EpicenterTournament,
        ]
    }

    pub const fn label(&self) -> &'static str {
        match self {
            Self::SingleModelCall => "Arm 1: Single Model Call",
            Self::SingleManagedAgent => "Arm 2: Single Managed Agent",
            Self::EpicenterTournament => "Arm 3: Epicenter Tournament",
        }
    }
}

/// Dataset split discipline for 5-Sigma scientific rigor (5 calibration + 15 locked unseen + live judge).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EvalSplit {
    Calibration,
    #[default]
    LockedUnseen,
    LiveJudge,
}

/// Real-world issue evaluation case with ground-truth maintainer verification patch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvalIssueCase {
    pub id: String,
    pub repo_url: String,
    pub issue_number: u64,
    #[serde(default)]
    pub pr_number: Option<u64>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub issue_body: String,
    pub parent_commit: String,
    pub ground_truth_test_patch: String,
    pub has_multimodal_media: bool,
    pub media_urls: Vec<String>,
    #[serde(default)]
    pub split: EvalSplit,
}

/// Outcome of running a single `EvalArm` against a single `EvalIssueCase`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArmOutcome {
    pub case_id: String,
    pub arm: EvalArm,
    pub resolved_ground_truth: bool,
    pub regression_free: bool,
    pub speedup_pct: Option<f64>,
    pub blast_risk_score: Option<f32>,
    #[serde(default)]
    pub root_cause_depth_score: Option<f32>,
    #[serde(default)]
    pub dark_zone_touches: Option<usize>,
    pub duration_ms: u64,
    pub notes: Option<String>,
    #[serde(default)]
    pub failure_category: Option<String>,
}

/// Aggregated statistics for a single `EvalArm` across an evaluation suite.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArmAggregateMetrics {
    pub arm: EvalArm,
    pub total_cases: usize,
    pub resolved_count: usize,
    pub resolved_rate_pct: f64,
    pub regression_free_rate_pct: f64,
    pub median_speedup_pct: Option<f64>,
    pub mean_blast_risk_score: Option<f32>,
    #[serde(default)]
    pub mean_root_cause_depth: Option<f32>,
    pub median_duration_ms: f64,
}

/// Documented failure-to-fix iteration captured during the 5-issue Calibration phase.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalibrationFinding {
    pub failure_mode: String,
    pub observed_on_case: String,
    pub system_change_made: String,
    pub impact_summary: String,
}

/// Cross-arm comparison scorecard generated from `ArmOutcome` records.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvalScorecard {
    pub total_cases: usize,
    pub multimodal_cases: usize,
    #[serde(default)]
    pub calibration_cases: usize,
    #[serde(default)]
    pub unseen_cases: usize,
    pub arm_metrics: Vec<ArmAggregateMetrics>,
}

impl EvalScorecard {
    pub fn from_outcomes(cases: &[EvalIssueCase], outcomes: &[ArmOutcome]) -> Self {
        let multimodal_cases = cases.iter().filter(|c| c.has_multimodal_media).count();
        let calibration_cases = cases
            .iter()
            .filter(|c| c.split == EvalSplit::Calibration)
            .count();
        let unseen_cases = cases
            .iter()
            .filter(|c| c.split == EvalSplit::LockedUnseen)
            .count();
        let mut arm_metrics = Vec::new();

        for &arm in EvalArm::all() {
            let arm_outcomes: Vec<&ArmOutcome> =
                outcomes.iter().filter(|o| o.arm == arm).collect();
            let total = arm_outcomes.len();
            if total == 0 {
                continue;
            }
            let resolved_count = arm_outcomes
                .iter()
                .filter(|o| o.resolved_ground_truth)
                .count();
            let regression_free_count =
                arm_outcomes.iter().filter(|o| o.regression_free).count();

            let speedups: Vec<f64> = arm_outcomes.iter().filter_map(|o| o.speedup_pct).collect();
            let median_speedup_pct = if speedups.is_empty() {
                None
            } else {
                Some(percentile_ms(&speedups, 50.0))
            };

            let risks: Vec<f32> = arm_outcomes
                .iter()
                .filter_map(|o| o.blast_risk_score)
                .collect();
            let mean_blast_risk_score = if risks.is_empty() {
                None
            } else {
                Some(risks.iter().sum::<f32>() / (risks.len() as f32))
            };

            let rcs: Vec<f32> = arm_outcomes
                .iter()
                .filter_map(|o| o.root_cause_depth_score)
                .collect();
            let mean_root_cause_depth = if rcs.is_empty() {
                None
            } else {
                Some(rcs.iter().sum::<f32>() / (rcs.len() as f32))
            };

            let durations: Vec<f64> = arm_outcomes.iter().map(|o| o.duration_ms as f64).collect();
            let median_duration_ms = percentile_ms(&durations, 50.0);

            arm_metrics.push(ArmAggregateMetrics {
                arm,
                total_cases: total,
                resolved_count,
                resolved_rate_pct: (resolved_count as f64 / total as f64) * 100.0,
                regression_free_rate_pct: (regression_free_count as f64 / total as f64) * 100.0,
                median_speedup_pct,
                mean_blast_risk_score,
                mean_root_cause_depth,
                median_duration_ms,
            });
        }

        Self {
            total_cases: cases.len(),
            multimodal_cases,
            calibration_cases,
            unseen_cases,
            arm_metrics,
        }
    }
}

/// Complete persisted record of a 5-Sigma evaluation run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FiveSigmaEval {
    pub run_id: Uuid,
    pub label: String,
    pub cases: Vec<EvalIssueCase>,
    pub outcomes: Vec<ArmOutcome>,
    pub scorecard: EvalScorecard,
    #[serde(default)]
    pub calibration_findings: Vec<CalibrationFinding>,
    #[serde(default)]
    pub remaining_failure_modes: Vec<String>,
    pub created_at: DateTime<Utc>,
}
