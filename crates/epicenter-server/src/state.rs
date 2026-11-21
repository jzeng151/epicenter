use crate::db::DbStore;
use crate::github::GitHubClient;
use epicenter_common::{
    CandidateResult, CandidateStatus, LiveEvent, TournamentSession, TournamentStage,
};
use epicenter_engine::TournamentEngine;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tokio::sync::broadcast;
use uuid::Uuid;

pub const EVENT_CHANNEL_CAPACITY: usize = 256;

/// Shared server application state passed into all Axum route handlers.
#[derive(Clone)]
pub struct AppState {
    pub db: DbStore,
    pub engine: Arc<TournamentEngine>,
    pub github: GitHubClient,
    channels: Arc<RwLock<HashMap<Uuid, broadcast::Sender<LiveEvent>>>>,
}

impl AppState {
    pub fn new(db: DbStore, engine: TournamentEngine, github: GitHubClient) -> Self {
        Self {
            db,
            engine: Arc::new(engine),
            github,
            channels: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn from_env(db: DbStore) -> Self {
        Self::new(db, TournamentEngine::from_env(), GitHubClient::from_env())
    }

    /// Returns the existing broadcast sender for `session_id`, or creates a new one.
    pub fn get_or_create_channel(&self, session_id: Uuid) -> broadcast::Sender<LiveEvent> {
        if let Ok(guard) = self.channels.read() {
            if let Some(tx) = guard.get(&session_id) {
                return tx.clone();
            }
        }
        let mut guard = self
            .channels
            .write()
            .expect("event channels rwlock poisoned");
        guard
            .entry(session_id)
            .or_insert_with(|| broadcast::channel(EVENT_CHANNEL_CAPACITY).0)
            .clone()
    }

    /// Subscribes to live events for `session_id`.
    pub fn subscribe(&self, session_id: Uuid) -> broadcast::Receiver<LiveEvent> {
        self.get_or_create_channel(session_id).subscribe()
    }
}

/// Applies an incremental `LiveEvent` to a working `TournamentSession` snapshot.
/// Returns `true` if the session state mutated and should be persisted to SQLite.
pub fn apply_event_to_session(session: &mut TournamentSession, event: &LiveEvent) -> bool {
    let mut dirty = true;
    match event {
        LiveEvent::StageChanged { stage, .. } => {
            session.stage = *stage;
        }
        LiveEvent::IntakeReady { brief, .. } => {
            session.intake = (**brief).clone();
        }
        LiveEvent::RedGateVerified { red_gate, .. } => {
            session.red_gate = Some((**red_gate).clone());
        }
        LiveEvent::StrategyPlanReady { plan, .. } => {
            for spec in &plan.strategies {
                if !session
                    .candidates
                    .iter()
                    .any(|c| c.strategy_id == spec.id)
                {
                    session.candidates.push(CandidateResult::pending(&spec.id));
                }
            }
            session.strategy_plan = Some((**plan).clone());
        }
        LiveEvent::CandidateStatusChanged {
            strategy_id,
            status,
            ..
        } => {
            if let Some(cand) = session
                .candidates
                .iter_mut()
                .find(|c| c.strategy_id == *strategy_id)
            {
                cand.status = *status;
            } else {
                let mut cand = CandidateResult::pending(strategy_id);
                cand.status = *status;
                session.candidates.push(cand);
            }
        }
        LiveEvent::CandidateUpdated { candidate, .. } => {
            if let Some(existing) = session
                .candidates
                .iter_mut()
                .find(|c| c.strategy_id == candidate.strategy_id)
            {
                *existing = (**candidate).clone();
            } else {
                session.candidates.push((**candidate).clone());
            }
        }
        LiveEvent::ArenaCandidateDone {
            strategy_id,
            metrics,
            ..
        } => {
            if let Some(cand) = session
                .candidates
                .iter_mut()
                .find(|c| c.strategy_id == *strategy_id)
            {
                cand.arena_metrics = Some(metrics.clone());
                cand.status = CandidateStatus::BenchmarkDone;
            }
        }
        LiveEvent::ArenaCompleted { summary, .. } => {
            session.arena_summary = Some(summary.clone());
        }
        LiveEvent::BlastRadiusReady {
            strategy_id,
            report,
            ..
        } => {
            if let Some(cand) = session
                .candidates
                .iter_mut()
                .find(|c| c.strategy_id == *strategy_id)
            {
                cand.blast_radius = Some((**report).clone());
            }
        }
        LiveEvent::HazardAuditReady {
            strategy_id,
            hazards,
            composite_risk_score,
            ..
        } => {
            if let Some(cand) = session
                .candidates
                .iter_mut()
                .find(|c| c.strategy_id == *strategy_id)
            {
                let report = cand.blast_radius.get_or_insert_with(Default::default);
                report.semantic_hazards = hazards.clone();
                report.composite_risk_score = *composite_risk_score;
                cand.status = CandidateStatus::Audited;
            }
        }
        LiveEvent::AudioBriefingReady { audio_url, .. } => {
            session.audio_briefing_url = Some(audio_url.clone());
        }
        LiveEvent::TournamentCompleted { .. } => {
            session.stage = TournamentStage::Completed;
        }
        LiveEvent::SessionCreated { .. }
        | LiveEvent::ArenaTrialSample { .. }
        | LiveEvent::Log { .. }
        | LiveEvent::Error { .. } => {
            dirty = false;
        }
    }

    if dirty {
        session.updated_at = chrono::Utc::now();
    }
    dirty
}
