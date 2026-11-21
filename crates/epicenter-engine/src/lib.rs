pub mod arena;
pub mod auditor;
pub mod baselines;
pub mod briefing;
pub mod demo;
pub mod intake;
pub mod planner;
pub mod red_gate;
pub mod repo;
pub mod verifier;
pub mod workers;

use chrono::Utc;
use epicenter_common::{
    CreateSessionRequest, ExportBundleResponse, LiveEvent, LogLevel, TaskKind, TournamentSession,
    TournamentStage,
};
use epicenter_gemini::GeminiClient;
use std::collections::BTreeMap;
use tokio::sync::broadcast;
use tracing::warn;
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
    ///
    /// - When `self.gemini` is `Some(client)`, executes the fast host-verified 7-stage Gemini 4 pipeline:
    ///   1. Local cached git mirror checkout + concurrent runtime env prep & `Hydrogen` multimodal intake.
    ///   2. Independent Host-Verified Locked Red-Gate (2× `Neon` -> 1× `Carbon` escalation, honest fail-fast).
    ///   3. `Carbon` dynamic-K (`1..=5`, default 3) orthogonal strategy planner + fast micro-benchmark spec.
    ///   4. Parallel host-verified `Neon` edit-test workers (up to 3 loops, 90s deadline) + `0/K` Carbon rescue.
    ///   5. Single-Host Interleaved Benchmark Arena (`std::time::Instant`) + real coverage capture.
    ///   6. 3-Layer Blast-Radius (`RepoCallGraph` + `CoverageMap` + concurrent `Carbon` Dual-Score hazards/depth).
    ///   7. 2-Speaker War-Room Executive Briefing (`Hydrogen` -> `Carbon` fallback) + optional TTS WAV.
    /// - When `self.gemini` is `None`, runs the deterministic offline simulation (`demo::run_offline_demo_tournament`).
    pub async fn execute_session(
        &self,
        mut session: TournamentSession,
        attachments: Vec<UploadedAttachmentBytes>,
        opts: EngineRunOptions,
        event_tx: Option<broadcast::Sender<LiveEvent>>,
    ) -> anyhow::Result<EngineRunOutput> {
        let Some(ref gemini) = self.gemini else {
            return demo::run_offline_demo_tournament(session, attachments, opts, event_tx).await;
        };

        let verifier = verifier::HostVerifier::new();

        // Fast local git mirror checkout + full file tree & ~160 source files (no slow HTTP raw fetches)
        let mut prepared_repo = match verifier
            .prepare_checkout(
                &session.intake.repo_url,
                session.intake.commit_sha.as_deref(),
                session.intake.git_ref.as_deref(),
            )
            .await
        {
            Ok(prep) => prep,
            Err(err) => {
                let msg = format!("Repository checkout failed: {err}");
                session.stage = TournamentStage::Failed;
                session.updated_at = Utc::now();
                repo::emit_event(
                    &event_tx,
                    LiveEvent::Error {
                        session_id: Some(session.id),
                        message: msg.clone(),
                    },
                );
                repo::emit_event(
                    &event_tx,
                    LiveEvent::StageChanged {
                        session_id: session.id,
                        stage: TournamentStage::Failed,
                        message: msg,
                    },
                );
                return Err(err);
            }
        };

        let mut repo_snapshot = prepared_repo
            .load_repo_snapshot(&session.intake.suspect_files)
            .await
            .unwrap_or_default();

        repo::emit_log(
            &event_tx,
            session.id,
            LogLevel::Info,
            "repo",
            format!(
                "Checked out {}/{}@{} locally ({}, {} tree entries, {} source files loaded)",
                repo_snapshot.owner,
                repo_snapshot.repo,
                &prepared_repo.resolved_sha[..prepared_repo.resolved_sha.len().min(12)],
                prepared_repo.runtime.label(),
                repo_snapshot.file_tree.len(),
                repo_snapshot.sources.len()
            ),
        );

        // Run cached runtime environment setup (uv venv / node_modules) CONCURRENTLY with Stage 1 Hydrogen intake!
        let (env_res, intake_res) = tokio::join!(
            prepared_repo.build_environment(),
            intake::run_intake_stage(gemini, &mut session, &attachments, &repo_snapshot, &event_tx),
        );
        intake_res?;

        match env_res {
            Ok(env) => {
                if !env.notes.is_empty() {
                    repo::emit_log(
                        &event_tx,
                        session.id,
                        LogLevel::Info,
                        "verifier",
                        format!(
                            "Host sandbox ready ({}): {}",
                            prepared_repo.runtime.label(),
                            env.notes.join("; ")
                        ),
                    );
                }
                prepared_repo.set_env(env);
            }
            Err(err) => {
                warn!(error = %err, "host verification environment preparation failed");
                let msg = format!("Host sandbox environment setup failed: {err}");
                session.stage = TournamentStage::Failed;
                session.updated_at = Utc::now();
                repo::emit_event(
                    &event_tx,
                    LiveEvent::Error {
                        session_id: Some(session.id),
                        message: msg.clone(),
                    },
                );
                repo::emit_event(
                    &event_tx,
                    LiveEvent::StageChanged {
                        session_id: session.id,
                        stage: TournamentStage::Failed,
                        message: msg,
                    },
                );
                return Err(err);
            }
        }

        // Enrich local repo sources with any newly-discovered suspect files from Stage 1
        if let Ok(added) = prepared_repo
            .enrich_snapshot_from_local(&mut repo_snapshot, &session.intake.suspect_files)
            .await
        {
            if added > 0 {
                repo::emit_log(
                    &event_tx,
                    session.id,
                    LogLevel::Info,
                    "repo",
                    format!("Loaded {added} additional Stage-1 suspect source file(s) from local mirror"),
                );
            }
        }

        // Stage 2: Independent Host-Verified Locked Red-Gate (fail-fast honestly if unverifiable)
        let red_gate = match red_gate::run_red_gate_stage(
            gemini,
            &mut session,
            &prepared_repo,
            &repo_snapshot,
            &event_tx,
        )
        .await
        {
            Ok(rg) => rg,
            Err(err) => {
                session.stage = TournamentStage::Failed;
                session.updated_at = Utc::now();
                repo::emit_event(
                    &event_tx,
                    LiveEvent::Error {
                        session_id: Some(session.id),
                        message: err.to_string(),
                    },
                );
                repo::emit_event(
                    &event_tx,
                    LiveEvent::StageChanged {
                        session_id: session.id,
                        stage: TournamentStage::Failed,
                        message: format!("Red-Gate verification failed: {err}"),
                    },
                );
                return Err(err);
            }
        };

        // Stage 3: Carbon Dynamic-K Orthogonal Planner
        let mut plan = planner::run_planner_stage(
            gemini,
            &mut session,
            &prepared_repo,
            &red_gate,
            &repo_snapshot,
            &event_tx,
        )
        .await?;

        // Ensure all planner target_files_hint entries are loaded locally for Stage 4 & Stage 6
        let plan_hints: Vec<String> = plan
            .strategies
            .iter()
            .flat_map(|s| s.target_files_hint.iter().cloned())
            .collect();
        let _ = prepared_repo
            .enrich_snapshot_from_local(&mut repo_snapshot, &plan_hints)
            .await;

        // Stage 4: Parallel Host-Verified Neon Edit-Test Workers + 0/K Carbon Rescue
        workers::run_workers_stage(
            gemini,
            &mut session,
            &prepared_repo,
            &red_gate,
            &mut plan,
            &repo_snapshot,
            &event_tx,
        )
        .await?;

        // Stage 5: Single-Host Interleaved Benchmark Arena + Real Coverage Capture
        let arena_res = arena::run_arena_stage(
            &mut session,
            &prepared_repo,
            &red_gate,
            &plan.shared_benchmark_spec,
            &event_tx,
        )
        .await?;

        // Stage 6: 3-Layer Blast-Radius & Concurrent Carbon Dual-Score Auditor
        let recommended = auditor::run_auditor_stage(
            gemini,
            &mut session,
            &repo_snapshot,
            &arena_res.coverage_maps,
            &event_tx,
        )
        .await?;

        // Stage 7: 2-Speaker War-Room Executive Briefing + Optional TTS WAV
        let briefing_wav =
            briefing::run_briefing_stage(gemini, &mut session, &opts, &event_tx).await;

        session.stage = TournamentStage::Completed;
        session.updated_at = Utc::now();
        repo::emit_event(
            &event_tx,
            LiveEvent::TournamentCompleted {
                session_id: session.id,
                recommended_strategy_id: recommended,
            },
        );

        Ok(EngineRunOutput {
            session,
            briefing_wav,
        })
    }

    /// Arm 1 Baseline for 5-Sigma Evaluation: Single `gemini-4-neon` model call returning unified `git diff`.
    pub async fn run_single_model_baseline(
        &self,
        repo_url: &str,
        parent_commit: Option<&str>,
        problem_statement: &str,
        repo_files: &BTreeMap<String, String>,
    ) -> anyhow::Result<String> {
        baselines::execute_single_model_baseline(
            self.gemini.as_ref(),
            repo_url,
            parent_commit,
            problem_statement,
            repo_files,
        )
        .await
    }

    /// Arm 2 Baseline for 5-Sigma Evaluation: Single `gemini-4-neon` Managed Agent sandbox returning unified `git diff`.
    pub async fn run_single_managed_agent_baseline(
        &self,
        repo_url: &str,
        parent_commit: Option<&str>,
        problem_statement: &str,
    ) -> anyhow::Result<String> {
        baselines::execute_single_managed_agent_baseline(
            self.gemini.as_ref(),
            repo_url,
            parent_commit,
            problem_statement,
        )
        .await
    }

    /// Builds a zero-auth `.patch` + Repro Test + Markdown Blast-Radius Audit export bundle for any candidate.
    pub fn build_export_bundle(
        session: &TournamentSession,
        strategy_id: &str,
    ) -> Option<ExportBundleResponse> {
        baselines::build_rich_export_bundle(session, strategy_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use epicenter_common::{
        AttachmentMeta, BenchmarkHarnessSpec, BugRedGate, CandidateBadge, CandidateStatus,
        MediaKind, RedGateResolution, RedGateSpec, StrategyDepthTier, StrategyPlan, StrategySpec,
    };
    use serde::Deserialize;

    fn sample_request() -> CreateSessionRequest {
        CreateSessionRequest {
            repo_url: "https://github.com/acme/billing-service".to_string(),
            commit_sha: Some("deadbeef1234".to_string()),
            git_ref: Some("main".to_string()),
            task_kind: Some(TaskKind::Bug),
            title: Some("Tenant pricing cache leak".to_string()),
            problem_statement: "Concurrent tenants receive each other's cached tax totals."
                .to_string(),
            attachment_ids: Vec::new(),
            attachments: Vec::new(),
        }
    }

    #[test]
    fn strip_locked_repro_diff_removes_tampered_hunks_only() {
        let diff = concat!(
            "diff --git a/app/pricing.py b/app/pricing.py\n",
            "--- a/app/pricing.py\n",
            "+++ b/app/pricing.py\n",
            "@@ -1,3 +1,3 @@\n",
            "-return 1\n",
            "+return 2\n",
            "diff --git a/tests/test_epicenter_repro.py b/tests/test_epicenter_repro.py\n",
            "--- a/tests/test_epicenter_repro.py\n",
            "+++ b/tests/test_epicenter_repro.py\n",
            "@@ -1,3 +1,3 @@\n",
            "-assert us != eu\n",
            "+assert True\n",
            "diff --git a/tests/test_billing_callers.py b/tests/test_billing_callers.py\n",
            "--- a/tests/test_billing_callers.py\n",
            "+++ b/tests/test_billing_callers.py\n",
            "@@ -1,2 +1,3 @@\n",
            "+assert refund_invoice('t1', [10.0], 0.1) == -11.0\n",
        );

        let (cleaned, stripped) =
            repo::strip_locked_test_from_diff(diff, "./tests/test_epicenter_repro.py");
        assert_eq!(stripped, 1);
        assert!(cleaned.contains("app/pricing.py"));
        assert!(cleaned.contains("tests/test_billing_callers.py"));
        assert!(!cleaned.contains("tests/test_epicenter_repro.py"));
        assert!(!cleaned.contains("assert True"));
    }

    #[test]
    fn diff_adds_caller_tests_ignores_locked_repro_file() {
        let only_locked = concat!(
            "diff --git a/tests/test_epicenter_repro.py b/tests/test_epicenter_repro.py\n",
            "--- a/tests/test_epicenter_repro.py\n",
            "+++ b/tests/test_epicenter_repro.py\n",
            "@@ -1,1 +1,2 @@\n",
            "+assert True\n",
        );
        assert!(!repo::diff_adds_caller_tests(
            only_locked,
            Some("tests/test_epicenter_repro.py")
        ));

        let with_caller = concat!(
            "diff --git a/tests/test_billing_callers.py b/tests/test_billing_callers.py\n",
            "--- /dev/null\n",
            "+++ b/tests/test_billing_callers.py\n",
            "@@ -0,0 +1,2 @@\n",
            "+def test_caller(): pass\n",
        );
        assert!(repo::diff_adds_caller_tests(
            with_caller,
            Some("tests/test_epicenter_repro.py")
        ));
    }

    #[test]
    fn extract_json_from_text_handles_prose_fences_and_nested_strings() {
        #[derive(Debug, Deserialize, PartialEq)]
        struct Payload {
            winner: String,
            score: u32,
            note: String,
        }

        let noisy = r#"
I ran the benchmark suite and inspected {non-json braces} in the log output.
Here is the final structured summary:
```json
{
  "winner": "s3-root-cause-cure",
  "score": 95,
  "note": "contains {braces} inside string"
}
```
Done!
"#;
        let parsed: Payload = repo::extract_json_from_text(noisy).unwrap();
        assert_eq!(
            parsed,
            Payload {
                winner: "s3-root-cause-cure".into(),
                score: 95,
                note: "contains {braces} inside string".into(),
            }
        );

        let unfenced = r#"Agent trace... {"winner":"s1","score":42,"note":"plain"}"#;
        let parsed2: Payload = repo::extract_json_from_text(unfenced).unwrap();
        assert_eq!(parsed2.winner, "s1");
        assert_eq!(parsed2.score, 42);
    }

    #[test]
    fn extract_unified_diff_from_text_strips_fences_and_prose() {
        let raw = "Here is the patch:\n```diff\ndiff --git a/src/a.py b/src/a.py\n--- a/src/a.py\n+++ b/src/a.py\n@@ -1 +1 @@\n-old\n+new\n```\nAll tests green.";
        let diff = repo::extract_unified_diff_from_text(raw);
        assert!(diff.starts_with("diff --git a/src/a.py b/src/a.py"));
        assert!(diff.contains("+new"));
        assert!(!diff.contains("All tests green"));
    }

    #[test]
    fn parses_github_urls_and_normalizes_clone_url() {
        let https = repo::parse_github_repo_url("https://github.com/jzeng151/epicenter/").unwrap();
        assert_eq!(https.owner, "jzeng151");
        assert_eq!(https.repo, "epicenter");

        let ssh = repo::parse_github_repo_url("git@github.com:jzeng151/epicenter.git").unwrap();
        assert_eq!(ssh.owner, "jzeng151");
        assert_eq!(ssh.repo, "epicenter");

        assert_eq!(
            repo::normalize_clone_url("https://github.com/jzeng151/epicenter"),
            "https://github.com/jzeng151/epicenter.git"
        );
        assert!(repo::parse_github_repo_url("https://gitlab.com/foo/bar").is_none());
    }

    #[test]
    fn normalize_and_validate_plan_clamps_dedupes_and_enables_caller_tests() {
        let session = TournamentEngine::initialize_session(&sample_request());
        let rg = RedGateSpec::Bug(BugRedGate {
            repro_test_path: "tests/test_repro.py".into(),
            repro_test_patch: String::new(),
            test_command: "pytest -q".into(),
            verified_fails_on_head: true,
            failure_excerpt: "AssertionError".into(),
            base_environment_id: None,
            resolution: RedGateResolution::VerifiedNeon,
        });

        let make = |id: &str, tier: StrategyDepthTier| StrategySpec {
            id: id.into(),
            title: id.into(),
            thesis: "t".into(),
            tradeoff_axis: "a".into(),
            target_files_hint: vec![],
            risk_hypothesis: "r".into(),
            depth_tier: tier,
            anti_overlap_rules: vec![],
            allow_caller_tests: false,
        };

        let raw_plan = StrategyPlan {
            degree_of_freedom_rationale: "test".into(),
            strategies: vec![
                make("dup", StrategyDepthTier::SurgicalHotfix),
                make("dup", StrategyDepthTier::TargetedRefactor),
                make("root", StrategyDepthTier::ArchitecturalRootCause),
                make("alg", StrategyDepthTier::AlgorithmicOptimization),
                make("extra1", StrategyDepthTier::TargetedRefactor),
                make("extra2", StrategyDepthTier::TargetedRefactor),
            ],
            shared_benchmark_spec: BenchmarkHarnessSpec {
                name: "bench".into(),
                command: String::new(),
                setup_command: None,
                trials: 0,
                warmup_trials: 1,
                metric_unit: String::new(),
                entrypoint_contract: None,
                script_content: None,
            },
        };

        let normalized = planner::normalize_and_validate_plan(raw_plan, &session, &rg);
        assert_eq!(normalized.strategies.len(), 5);
        assert!(normalized.validate().is_ok());
        assert_eq!(normalized.strategies[0].id, "dup");
        assert_eq!(normalized.strategies[1].id, "dup-2");
        assert!(normalized.strategies[2].allow_caller_tests);
        assert_eq!(normalized.shared_benchmark_spec.trials, 6);
        assert_eq!(normalized.shared_benchmark_spec.command, "pytest -q");
        assert_eq!(normalized.shared_benchmark_spec.metric_unit, "ms");
    }

    #[tokio::test]
    async fn offline_simulation_runs_all_7_stages_and_crowns_dual_score_badges() {
        let engine = TournamentEngine::new(None);
        assert!(!engine.has_live_gemini());

        let session = TournamentEngine::initialize_session(&sample_request());
        let (tx, mut rx) = broadcast::channel(128);

        let att = UploadedAttachmentBytes {
            meta: AttachmentMeta {
                id: Uuid::new_v4(),
                filename: "checkout_bug.mp4".into(),
                mime_type: "video/mp4".into(),
                media_kind: MediaKind::ScreenRecording,
                size_bytes: 128,
                file_uri: None,
            },
            bytes: vec![0u8; 16],
        };

        let output = engine
            .execute_session(
                session,
                vec![att],
                EngineRunOptions {
                    synthesize_tts: false,
                    github_token: None,
                },
                Some(tx),
            )
            .await
            .expect("offline tournament succeeds");

        assert!(output.briefing_wav.is_none());
        assert_eq!(output.session.stage, TournamentStage::Completed);
        assert!(output.session.red_gate.is_some());
        assert_eq!(output.session.candidates.len(), 3);
        assert!(output.session.war_room_script.len() >= 4);

        // Verify all 3 candidates audited with genuine Ring 0/1/2 blast graphs
        for c in &output.session.candidates {
            assert_eq!(c.status, CandidateStatus::Audited);
            assert!(c.is_green());
            assert!(c.arena_metrics.is_some());
            let br = c.blast_radius.as_ref().expect("blast radius present");
            assert!(!br.ring0_mutated.is_empty());
            assert!(!br.ring1_direct_callers.is_empty());
            assert!(!br.ring2_transitive_entrypoints.is_empty());
        }

        // Verify Dual-Score badge crowning differentiates all 3 archetypes
        let s1 = output
            .session
            .candidates
            .iter()
            .find(|c| c.strategy_id == "s1-surgical-hotfix")
            .unwrap();
        let s2 = output
            .session
            .candidates
            .iter()
            .find(|c| c.strategy_id == "s2-algorithmic-cache")
            .unwrap();
        let s3 = output
            .session
            .candidates
            .iter()
            .find(|c| c.strategy_id == "s3-root-cause-cure")
            .unwrap();

        assert!(s1.badges.contains(&CandidateBadge::SafestEmergencyHotfix));
        assert!(s1.blast_radius.as_ref().unwrap().bandaid_debt_warning.is_some());
        assert!(s2.badges.contains(&CandidateBadge::FastestThroughput));
        assert!(s2.blast_radius.as_ref().unwrap().dark_zone_count() > 0);
        assert!(s3.badges.contains(&CandidateBadge::BestOverall));
        assert_eq!(s3.blast_radius.as_ref().unwrap().dark_zone_count(), 0);
        assert!(s3.added_caller_tests);

        let summary = output.session.arena_summary.as_ref().unwrap();
        assert_eq!(
            summary.recommended_strategy_id.as_deref(),
            Some("s3-root-cause-cure")
        );
        assert!(!summary.pareto_frontier_ids.is_empty());

        // Drain broadcast channel and verify key live events were emitted
        let mut events = Vec::new();
        while let Ok(evt) = rx.try_recv() {
            events.push(evt);
        }
        assert!(events.iter().any(|e| matches!(e, LiveEvent::IntakeReady { .. })));
        assert!(events.iter().any(|e| matches!(e, LiveEvent::RedGateVerified { .. })));
        assert!(events.iter().any(|e| matches!(e, LiveEvent::StrategyPlanReady { .. })));
        assert!(events.iter().any(|e| matches!(e, LiveEvent::ArenaTrialSample { .. })));
        assert!(events.iter().any(|e| matches!(e, LiveEvent::ArenaCompleted { .. })));
        assert!(events.iter().any(|e| matches!(e, LiveEvent::BlastRadiusReady { .. })));
        assert!(events.iter().any(|e| matches!(e, LiveEvent::HazardAuditReady { .. })));
        assert!(events.iter().any(|e| matches!(
            e,
            LiveEvent::TournamentCompleted {
                recommended_strategy_id: Some(id),
                ..
            } if id == "s3-root-cause-cure"
        )));
    }

    #[tokio::test]
    async fn build_export_bundle_produces_rich_markdown_audit_and_patch() {
        let engine = TournamentEngine::new(None);
        let session = TournamentEngine::initialize_session(&sample_request());
        let out = engine
            .execute_session(session, vec![], EngineRunOptions::default(), None)
            .await
            .unwrap();

        let bundle =
            TournamentEngine::build_export_bundle(&out.session, "s1-surgical-hotfix").unwrap();
        assert_eq!(bundle.strategy_id, "s1-surgical-hotfix");
        assert!(bundle.git_diff.contains("diff --git a/app/pricing.py"));
        assert!(bundle.repro_test_patch.contains("tests/test_epicenter_repro.py"));
        assert!(bundle.git_apply_command.starts_with("git apply epicenter-"));
        assert!(bundle.audit_markdown.contains("# Epicenter Blast-Radius & Dual-Score Audit Report"));
        assert!(bundle.audit_markdown.contains("Root-Cause Depth Score"));
        assert!(bundle.audit_markdown.contains("Composite Blast-Radius Risk"));
        assert!(bundle.audit_markdown.contains("Bandaid Debt Warning"));
        assert!(bundle.audit_markdown.contains("Ring 0"));
        assert!(bundle.audit_markdown.contains("Ring 1"));
        assert!(bundle.audit_markdown.contains("Ring 2"));
        assert!(bundle.audit_markdown.contains("[DARK ZONE]"));
        assert!(bundle.audit_markdown.contains("War-Room Executive Briefing Transcript"));

        assert!(TournamentEngine::build_export_bundle(&out.session, "nonexistent").is_none());
    }

    #[tokio::test]
    async fn offline_baselines_return_cleanly_without_live_key() {
        let engine = TournamentEngine::new(None);
        let files = BTreeMap::new();
        let d1 = engine
            .run_single_model_baseline("https://github.com/acme/repo", Some("abc"), "fix bug", &files)
            .await
            .unwrap();
        let d2 = engine
            .run_single_managed_agent_baseline("https://github.com/acme/repo", Some("abc"), "fix bug")
            .await
            .unwrap();
        assert!(d1.is_empty());
        assert!(d2.is_empty());
    }

    #[test]
    fn adaptive_thinking_levels_use_low_for_fast_path_and_escalate_on_retries() {
        // Stage 3 Carbon planner & Stage 6 concurrent Carbon auditor use low thinking.
        let planner_cfg = planner::planner_generation_config();
        assert_eq!(planner_cfg.thinking_level.as_deref(), Some("low"));
        assert_eq!(
            serde_json::to_value(&planner_cfg).unwrap(),
            serde_json::json!({ "thinking_level": "low" })
        );

        let auditor_cfg = auditor::auditor_generation_config();
        assert_eq!(auditor_cfg.thinking_level.as_deref(), Some("low"));

        // Stage 2 Bug Red-Gate: attempt 1 (Neon 1/2) uses low; attempt 2 & Carbon escalation escalate.
        assert_eq!(
            red_gate::bug_red_gate_generation_config(1)
                .and_then(|c| c.thinking_level)
                .as_deref(),
            Some("low")
        );
        assert!(red_gate::bug_red_gate_generation_config(2).is_none());
        assert!(red_gate::bug_red_gate_generation_config(3).is_none());

        // Stage 4 workers: loop 1 regular Neon uses low; retry loops & 0/K Carbon rescue escalate.
        assert_eq!(
            workers::worker_generation_config(1, false)
                .and_then(|c| c.thinking_level)
                .as_deref(),
            Some("low")
        );
        assert!(workers::worker_generation_config(2, false).is_none());
        assert!(workers::worker_generation_config(3, false).is_none());
        assert!(workers::worker_generation_config(1, true).is_none());
        assert!(workers::worker_generation_config(2, true).is_none());
    }
}
