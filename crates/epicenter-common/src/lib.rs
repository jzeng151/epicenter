pub mod blast;
pub mod domain;
pub mod eval;
pub mod events;
pub mod models;

pub use blast::*;
pub use domain::*;
pub use eval::*;
pub use events::*;
pub use models::*;

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::str::FromStr;
    use uuid::Uuid;

    #[test]
    fn gemini4_model_roundtrip() {
        for &model in Gemini4Model::all() {
            let s = model.as_str();
            assert_eq!(Gemini4Model::from_str(s).unwrap(), model);
            let json = serde_json::to_string(&model).unwrap();
            assert_eq!(json, format!("\"{s}\""));
            let back: Gemini4Model = serde_json::from_str(&json).unwrap();
            assert_eq!(back, model);
        }
        assert_eq!(GEMINI_TTS_MODEL, "gemini-3.8-flash-tts");
        assert_eq!(MANAGED_AGENT_BASE, "antigravity-preview-09-2026");
    }

    #[test]
    fn strategy_plan_validates_bounds_and_uniqueness() {
        let bench = BenchmarkHarnessSpec {
            name: "hot-path".into(),
            command: "pytest -q bench.py".into(),
            setup_command: None,
            trials: 10,
            warmup_trials: 2,
            metric_unit: "ms".into(),
            entrypoint_contract: None,
            script_content: None,
        };
        let empty = StrategyPlan {
            degree_of_freedom_rationale: "none".into(),
            strategies: vec![],
            shared_benchmark_spec: bench.clone(),
        };
        assert!(empty.validate().is_err());

        let make_strat = |id: &str| StrategySpec {
            id: id.into(),
            title: format!("Strat {id}"),
            thesis: "thesis".into(),
            tradeoff_axis: "latency vs memory".into(),
            target_files_hint: vec!["src/lib.rs".into()],
            risk_hypothesis: "low risk".into(),
        };

        let six = StrategyPlan {
            degree_of_freedom_rationale: "many".into(),
            strategies: (1..=6).map(|i| make_strat(&format!("s{i}"))).collect(),
            shared_benchmark_spec: bench.clone(),
        };
        assert!(six.validate().is_err());
        let clamped = six.clamped();
        assert_eq!(clamped.strategies.len(), 5);
        assert!(clamped.validate().is_ok());

        let dup = StrategyPlan {
            degree_of_freedom_rationale: "dup".into(),
            strategies: vec![make_strat("s1"), make_strat("s1")],
            shared_benchmark_spec: bench,
        };
        assert!(matches!(
            dup.validate(),
            Err(CommonError::DuplicateStrategyId(_))
        ));
    }

    #[test]
    fn pareto_frontier_and_arena_metrics() {
        let m1 = CandidateArenaMetrics::from_interleaved_samples(
            vec![100.0, 102.0, 98.0, 100.0],
            vec![50.0, 52.0, 48.0, 50.0],
            Some(12_000),
            Some(4),
        );
        assert!((m1.p50_ms - 50.0).abs() < 1e-6);
        assert!((m1.speedup_pct - 50.0).abs() < 1e-6);

        let m2 = CandidateArenaMetrics::from_interleaved_samples(
            vec![100.0, 100.0],
            vec![80.0, 80.0],
            None,
            None,
        );
        let m3 = CandidateArenaMetrics::from_interleaved_samples(
            vec![100.0, 100.0],
            vec![75.0, 75.0],
            None,
            None,
        );

        let mut c1 = CandidateResult::pending("fast-risky");
        c1.repro_passed = true;
        c1.regression_suite_passed = true;
        c1.arena_metrics = Some(m1);
        c1.blast_radius = Some(BlastRadiusReport {
            composite_risk_score: 40.0,
            ..Default::default()
        });

        let mut c2 = CandidateResult::pending("safe-modest");
        c2.repro_passed = true;
        c2.regression_suite_passed = true;
        c2.arena_metrics = Some(m2);
        c2.blast_radius = Some(BlastRadiusReport {
            composite_risk_score: 12.0,
            ..Default::default()
        });

        let mut c3 = CandidateResult::pending("dominated");
        c3.repro_passed = true;
        c3.regression_suite_passed = true;
        c3.arena_metrics = Some(m3);
        c3.blast_radius = Some(BlastRadiusReport {
            composite_risk_score: 45.0,
            ..Default::default()
        });

        let mut candidates = vec![c1, c2, c3];
        let frontier = compute_pareto_frontier(&mut candidates);
        assert_eq!(frontier, vec!["fast-risky", "safe-modest"]);
        assert!(candidates[0].pareto_optimal);
        assert!(candidates[1].pareto_optimal);
        assert!(!candidates[2].pareto_optimal);
    }

    #[test]
    fn eval_scorecard_and_live_event_serialization() {
        let cases = vec![EvalIssueCase {
            id: "case-1".into(),
            repo_url: "https://github.com/example/repo".into(),
            issue_number: 42,
            parent_commit: "abc1234".into(),
            ground_truth_test_patch: "diff --git a/test.py b/test.py".into(),
            has_multimodal_media: true,
            media_urls: vec!["https://example.com/screen.mp4".into()],
        }];
        let outcomes = vec![
            ArmOutcome {
                case_id: "case-1".into(),
                arm: EvalArm::SingleModelCall,
                resolved_ground_truth: false,
                regression_free: true,
                speedup_pct: None,
                blast_risk_score: Some(55.0),
                duration_ms: 4_000,
                notes: None,
            },
            ArmOutcome {
                case_id: "case-1".into(),
                arm: EvalArm::EpicenterTournament,
                resolved_ground_truth: true,
                regression_free: true,
                speedup_pct: Some(34.5),
                blast_risk_score: Some(18.0),
                duration_ms: 28_000,
                notes: Some("Won on Pareto frontier".into()),
            },
        ];
        let scorecard = EvalScorecard::from_outcomes(&cases, &outcomes);
        assert_eq!(scorecard.total_cases, 1);
        assert_eq!(scorecard.multimodal_cases, 1);
        assert_eq!(scorecard.arm_metrics.len(), 2);

        let evt = LiveEvent::StageChanged {
            session_id: Uuid::new_v4(),
            stage: TournamentStage::Benchmarking,
            message: "Running interleaved trials".into(),
        };
        let encoded = serde_json::to_string(&evt).unwrap();
        let decoded: LiveEvent = serde_json::from_str(&encoded).unwrap();
        assert_eq!(evt, decoded);

        let red_gate = RedGateSpec::Bug(BugRedGate {
            repro_test_path: "tests/test_repro.py".into(),
            repro_test_patch: "diff --git a/tests/test_repro.py b/tests/test_repro.py".into(),
            test_command: "pytest tests/test_repro.py".into(),
            verified_fails_on_head: true,
            failure_excerpt: "AssertionError: expected 200 got 500".into(),
            base_environment_id: Some("env-123".into()),
        });
        assert_eq!(red_gate.task_kind(), TaskKind::Bug);
        let rg_json = serde_json::to_string(&red_gate).unwrap();
        let rg_back: RedGateSpec = serde_json::from_str(&rg_json).unwrap();
        assert_eq!(red_gate, rg_back);

        let _eval = FiveSigmaEval {
            run_id: Uuid::new_v4(),
            label: "smoke".into(),
            cases,
            outcomes,
            scorecard,
            created_at: Utc::now(),
        };
    }
}
