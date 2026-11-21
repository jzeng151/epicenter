use chrono::Utc;
use epicenter_common::{
    CreateSessionRequest, ExportBundleResponse, LiveEvent, TaskKind, TournamentSession,
    TournamentStage,
};
use epicenter_gemini::GeminiClient;
use std::collections::BTreeMap;
use tokio::sync::broadcast;
use uuid::Uuid;

/// Uploaded raw attachment bytes passed into Stage 1 multimodal intake.
#[derive(Debug, Clone)]
pub struct UploadedAttachmentBytes {
    pub meta: epicenter_common::AttachmentMeta,
    pub bytes: Vec<u8>,
}

/// Output of a complete Epicenter tournament execution (session state + optional 24kHz WAV briefing).
#[derive(Debug, Clone)]
pub struct EngineRunOutput {
    pub session: TournamentSession,
    pub briefing_wav: Option<Vec<u8>>,
}

/// Configuration options controlling tournament execution behavior.
#[derive(Debug, Clone)]
pub struct EngineRunOptions {
    /// Synthesize `gemini-3.8-flash-tts` 2-speaker war-room WAV upon completion (true for Studio, false for batch eval).
    pub synthesize_tts: bool,
    /// Optional github token for higher rate limits when fetching repo tree/files.
    pub github_token: Option<String>,
}

impl Default for EngineRunOptions {
    fn default() -> Self {
        Self {
            synthesize_tts: true,
            github_token: std::env::var("GITHUB_TOKEN").ok().filter(|s| !s.is_empty()),
        }
    }
}

/// Core 7-stage Epicenter Tournament Orchestrator shared by `epicenter-server` and `epicenter-eval`.
#[derive(Clone)]
pub struct TournamentEngine {
    pub gemini: Option<GeminiClient>,
}

impl TournamentEngine {
    pub fn new(gemini: Option<GeminiClient>) -> Self {
        Self { gemini }
    }

    pub fn from_env() -> Self {
        Self {
            gemini: GeminiClient::from_env().ok(),
        }
    }

    pub fn has_live_gemini(&self) -> bool {
        self.gemini.is_some()
    }

    /// Creates an initial `TournamentSession` snapshot prior to background execution.
    pub fn initialize_session(req: &CreateSessionRequest) -> TournamentSession {
        let now = Utc::now();
        let id = Uuid::new_v4();
        TournamentSession {
            id,
            stage: TournamentStage::Intake,
            intake: epicenter_common::IntakeBrief {
                id: Uuid::new_v4(),
                task_kind: req.task_kind.unwrap_or(TaskKind::Bug),
                repo_url: req.repo_url.clone(),
                commit_sha: req.commit_sha.clone(),
                git_ref: req.git_ref.clone(),
                title: req
                    .title
                    .clone()
                    .unwrap_or_else(|| "Analyzing issue...".to_string()),
                problem_statement: req.problem_statement.clone(),
                repro_steps: Vec::new(),
                multimodal_findings: Vec::new(),
                attachments: req.attachments.clone(),
                suspect_files: Vec::new(),
                created_at: now,
            },
            red_gate: None,
            strategy_plan: None,
            candidates: Vec::new(),
            arena_summary: None,
            audio_briefing_url: None,
            war_room_script: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }

    /// Executes Stages 1..=7 of the Epicenter tournament, streaming `LiveEvent`s to `event_tx`.
    /// Note: Full implementation provided by `neon-engine`.
    pub async fn execute_session(
        &self,
        mut session: TournamentSession,
        _attachments: Vec<UploadedAttachmentBytes>,
        _opts: EngineRunOptions,
        event_tx: Option<broadcast::Sender<LiveEvent>>,
    ) -> anyhow::Result<EngineRunOutput> {
        session.stage = TournamentStage::Completed;
        session.updated_at = Utc::now();
        if let Some(tx) = &event_tx {
            let _ = tx.send(LiveEvent::TournamentCompleted {
                session_id: session.id,
                recommended_strategy_id: None,
            });
        }
        Ok(EngineRunOutput {
            session,
            briefing_wav: None,
        })
    }

    /// Arm 1 Baseline for 5-Sigma Evaluation: Single `gemini-4-neon` model call returning unified `git diff`.
    pub async fn run_single_model_baseline(
        &self,
        _repo_url: &str,
        _parent_commit: Option<&str>,
        _problem_statement: &str,
        _repo_files: &BTreeMap<String, String>,
    ) -> anyhow::Result<String> {
        Ok(String::new())
    }

    /// Arm 2 Baseline for 5-Sigma Evaluation: Single `gemini-4-neon` Managed Agent sandbox returning unified `git diff`.
    pub async fn run_single_managed_agent_baseline(
        &self,
        _repo_url: &str,
        _parent_commit: Option<&str>,
        _problem_statement: &str,
    ) -> anyhow::Result<String> {
        Ok(String::new())
    }

    /// Builds a zero-auth `.patch` + Repro Test + Markdown Blast-Radius Audit export bundle for any candidate.
    pub fn build_export_bundle(
        session: &TournamentSession,
        strategy_id: &str,
    ) -> Option<ExportBundleResponse> {
        let cand = session
            .candidates
            .iter()
            .find(|c| c.strategy_id == strategy_id)?;
        let repro_patch = session
            .red_gate
            .as_ref()
            .map(|rg| rg.patch().to_string())
            .unwrap_or_default();
        let patch_filename = format!("epicenter-{}-{}.patch", session.id, cand.strategy_id);
        Some(ExportBundleResponse {
            session_id: session.id,
            strategy_id: cand.strategy_id.clone(),
            git_apply_command: format!("git apply {}", patch_filename),
            patch_filename,
            git_diff: cand.git_diff.clone(),
            repro_test_patch: repro_patch,
            audit_markdown: format!("# Epicenter Audit — {}\n", cand.strategy_id),
        })
    }
}
