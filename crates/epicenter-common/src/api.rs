use crate::domain::{AttachmentMeta, TaskKind, TournamentSession, TournamentStage};
use crate::eval::{EvalArm, FiveSigmaEval};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// `GET /api/health` response shared between server and WASM Studio.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerStatusResponse {
    pub status: String,
    pub version: String,
    pub has_gemini_key: bool,
    pub has_github_token: bool,
}

/// `POST /api/sessions` request payload from Leptos Studio.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateSessionRequest {
    pub repo_url: String,
    #[serde(default)]
    pub commit_sha: Option<String>,
    #[serde(default)]
    pub git_ref: Option<String>,
    #[serde(default)]
    pub task_kind: Option<TaskKind>,
    #[serde(default)]
    pub title: Option<String>,
    pub problem_statement: String,
    #[serde(default)]
    pub attachment_ids: Vec<Uuid>,
    #[serde(default)]
    pub attachments: Vec<AttachmentMeta>,
}

/// `POST /api/sessions` response payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateSessionResponse {
    pub session: TournamentSession,
}

/// Lightweight list item for `GET /api/sessions`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionListItem {
    pub id: Uuid,
    pub title: String,
    pub repo_url: String,
    pub task_kind: TaskKind,
    pub stage: TournamentStage,
    pub candidate_count: usize,
    pub created_at: DateTime<Utc>,
}

/// `GET /api/sessions/{id}/candidates/{strategy_id}/export` zero-auth export bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportBundleResponse {
    pub session_id: Uuid,
    pub strategy_id: String,
    pub patch_filename: String,
    pub git_diff: String,
    pub repro_test_patch: String,
    pub audit_markdown: String,
    pub git_apply_command: String,
}

/// `POST /api/sessions/{id}/candidates/{strategy_id}/pr` request payload (uses `GITHUB_TOKEN`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreatePrRequest {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub target_branch: Option<String>,
}

/// `POST /api/sessions/{id}/candidates/{strategy_id}/pr` response payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreatePrResponse {
    pub pr_url: String,
    pub branch_name: String,
    pub number: u64,
}

/// `POST /api/eval/judge-pr` request payload for live hackathon judge testing on any merged GitHub PR.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveJudgePrRequest {
    pub pr_url: String,
}

/// `POST /api/eval/judge-pr` response payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveJudgePrResponse {
    pub eval: FiveSigmaEval,
}

/// Lifecycle state of an asynchronous Live PR Benchmark job (`POST /api/eval/judge-pr/start`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveJudgeJobState {
    Running,
    Completed,
    Failed,
}

/// Per-arm live progress telemetry polled by `GET /api/eval/judge-pr/{job_id}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveJudgeArmProgress {
    pub arm: EvalArm,
    pub state: LiveJudgeJobState,
    pub detail: String,
    pub elapsed_ms: u64,
}

/// Asynchronous status & final payload for `POST /api/eval/judge-pr/start` and `GET /api/eval/judge-pr/{job_id}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveJudgeJobStatusResponse {
    pub job_id: Uuid,
    pub pr_url: String,
    #[serde(default)]
    pub pr_title: Option<String>,
    pub state: LiveJudgeJobState,
    pub elapsed_ms: u64,
    pub arms: Vec<LiveJudgeArmProgress>,
    #[serde(default)]
    pub eval: Option<FiveSigmaEval>,
    #[serde(default)]
    pub error: Option<String>,
}
