use crate::blast::{BlastRadiusReport, SemanticHazard};
use crate::domain::{
    ArenaSummary, CandidateArenaMetrics, CandidateResult, CandidateStatus, IntakeBrief,
    RedGateSpec, StrategyPlan, TournamentStage,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

/// Real-time Server-Sent Events (SSE) pushed from `epicenter-server` to `epicenter-web`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", content = "data", rename_all = "snake_case")]
pub enum LiveEvent {
    SessionCreated {
        session_id: Uuid,
        timestamp: DateTime<Utc>,
    },
    StageChanged {
        session_id: Uuid,
        stage: TournamentStage,
        message: String,
    },
    IntakeReady {
        session_id: Uuid,
        brief: Box<IntakeBrief>,
    },
    RedGateVerified {
        session_id: Uuid,
        red_gate: Box<RedGateSpec>,
    },
    StrategyPlanReady {
        session_id: Uuid,
        plan: Box<StrategyPlan>,
    },
    CandidateStatusChanged {
        session_id: Uuid,
        strategy_id: String,
        status: CandidateStatus,
        detail: String,
    },
    CandidateUpdated {
        session_id: Uuid,
        candidate: Box<CandidateResult>,
    },
    ArenaTrialSample {
        session_id: Uuid,
        strategy_id: String,
        trial_index: u32,
        baseline_ms: f64,
        candidate_ms: f64,
    },
    ArenaCandidateDone {
        session_id: Uuid,
        strategy_id: String,
        metrics: CandidateArenaMetrics,
    },
    ArenaCompleted {
        session_id: Uuid,
        summary: ArenaSummary,
    },
    BlastRadiusReady {
        session_id: Uuid,
        strategy_id: String,
        report: Box<BlastRadiusReport>,
    },
    HazardAuditReady {
        session_id: Uuid,
        strategy_id: String,
        hazards: Vec<SemanticHazard>,
        composite_risk_score: f32,
    },
    AudioBriefingReady {
        session_id: Uuid,
        audio_url: String,
    },
    TournamentCompleted {
        session_id: Uuid,
        recommended_strategy_id: Option<String>,
    },
    Log {
        session_id: Uuid,
        level: LogLevel,
        source: String,
        message: String,
        timestamp: DateTime<Utc>,
    },
    Error {
        session_id: Option<Uuid>,
        message: String,
    },
}
