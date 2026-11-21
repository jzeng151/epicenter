pub mod grader;
pub mod harvester;
pub mod report;
pub mod runner;

#[cfg(test)]
mod tests {
    use crate::grader::{grade_submission, CandidateSubmission, GraderConfig, GradingMode};
    use crate::harvester::{
        build_case_from_pr_parts, extract_linked_issue_number, extract_media_urls,
        is_test_file_path, parse_pr_url, split_pr_diff_source_and_tests,
    };
    use crate::report::{compute_subset_scorecards, render_markdown_report, write_eval_artifacts};
    use crate::runner::{build_blind_problem_statement, run_evaluation, RunConfig, SplitFilter};
    use epicenter_common::{EvalArm, EvalIssueCase, EvalSplit, FiveSigmaEval};
    use epicenter_graph::parse_unified_diff;
    use std::collections::HashSet;
    use std::path::PathBuf;

    fn repo_dataset_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("eval_runs")
            .join("five_sigma_20_cases.json")
    }

    fn load_curated_cases() -> Vec<EvalIssueCase> {
        let raw = std::fs::read_to_string(repo_dataset_path())
            .expect("five_sigma_20_cases.json should exist in eval_runs/");
        serde_json::from_str(&raw).expect("five_sigma_20_cases.json must deserialize cleanly")
    }

    #[test]
    fn splits_unified_pr_diff_strictly_into_source_and_hidden_test_hunks() {
        let raw_diff = r#"diff --git a/httpx/_client.py b/httpx/_client.py
index 1111111..2222222 100644
--- a/httpx/_client.py
+++ b/httpx/_client.py
@@ -42,3 +42,4 @@ def _redirect_headers(self, request, url):
     if not same_origin(request.url, url):
         headers.pop("Authorization", None)
+        headers.pop("Proxy-Authorization", None)
     return headers
diff --git a/tests/client/test_redirects.py b/tests/client/test_redirects.py
index 3333333..4444444 100644
--- a/tests/client/test_redirects.py
+++ b/tests/client/test_redirects.py
@@ -10,2 +10,5 @@ def test_redirect():
     assert True
+def test_redirect_strips_proxy_auth():
+    assert True
diff --git a/src/router/index.ts b/src/router/index.ts
index 5555555..6666666 100644
--- a/src/router/index.ts
+++ b/src/router/index.ts
@@ -1,3 +1,3 @@ export function matchRoute(path: string) {
 export function matchRoute(path: string) {
-  return path.endsWith('/');
+  return path.replace(/\/+$/, '');
 }
diff --git a/src/router/index.test.ts b/src/router/index.test.ts
index 7777777..8888888 100644
--- a/src/router/index.test.ts
+++ b/src/router/index.test.ts
@@ -1,2 +1,4 @@ test('matchRoute', () => {
+test('strips trailing slash', () => {
+  expect(matchRoute('/api/')).toBe('/api');
+});
"#;

        let split = split_pr_diff_source_and_tests(raw_diff);
        assert!(split.has_both_source_and_tests());
        assert_eq!(
            split.source_files,
            vec!["httpx/_client.py", "src/router/index.ts"]
        );
        assert_eq!(
            split.test_files,
            vec!["tests/client/test_redirects.py", "src/router/index.test.ts"]
        );

        assert!(!split.ground_truth_test_patch.contains("httpx/_client.py"));
        assert!(!split.ground_truth_test_patch.contains("src/router/index.ts"));
        assert!(split
            .ground_truth_test_patch
            .contains("test_redirect_strips_proxy_auth"));
        assert!(split.ground_truth_test_patch.contains("src/router/index.test.ts"));

        assert!(!split.source_patch.contains("test_redirects.py"));
        assert!(!split.source_patch.contains("index.test.ts"));

        let parsed_tests = parse_unified_diff(&split.ground_truth_test_patch);
        assert_eq!(parsed_tests.len(), 2);
        assert!(parsed_tests.iter().all(|f| is_test_file_path(&f.path)));
    }

    #[test]
    fn extracts_markdown_html_and_github_asset_media_urls_deduplicated() {
        let markdown = r#"
Bug reproduction with screenshots and recording:
![trace-screenshot](https://user-images.githubusercontent.com/42/error-trace.png "Error Trace")
Duplicate reference: https://user-images.githubusercontent.com/42/error-trace.png
<img width="800" src="https://github.com/user-attachments/assets/aaaa-1111-bbbb-2222" />
<video src='https://cdn.example.org/demo/repro-recording.mp4'></video>
See docs at https://example.org/docs/guide (not media!) and flamegraph https://cdn.example.org/flame.webp.
"#;

        let urls = extract_media_urls(markdown);
        assert_eq!(
            urls,
            vec![
                "https://user-images.githubusercontent.com/42/error-trace.png",
                "https://github.com/user-attachments/assets/aaaa-1111-bbbb-2222",
                "https://cdn.example.org/demo/repro-recording.mp4",
                "https://cdn.example.org/flame.webp",
            ]
        );
    }

    #[test]
    fn parses_github_pr_urls_and_builds_live_judge_case() {
        let (owner, repo, num) =
            parse_pr_url("https://github.com/encode/httpx/pull/3119/files#diff-123").unwrap();
        assert_eq!(owner, "encode");
        assert_eq!(repo, "httpx");
        assert_eq!(num, 3119);

        assert_eq!(
            extract_linked_issue_number("This PR fixes #3112 by checking port origin."),
            Some(3112)
        );

        let diff = "diff --git a/httpx/_client.py b/httpx/_client.py\n\
            --- a/httpx/_client.py\n+++ b/httpx/_client.py\n@@ -1,1 +1,2 @@\n+x = 1\n\
            diff --git a/tests/test_redirects.py b/tests/test_redirects.py\n\
            --- a/tests/test_redirects.py\n+++ b/tests/test_redirects.py\n@@ -1,1 +1,2 @@\n+assert x == 1\n";

        let case = build_case_from_pr_parts(
            &owner,
            &repo,
            num,
            3112,
            "Strip auth on port change",
            "Fixes #3112\n![shot](https://user-images.githubusercontent.com/1/shot.png)",
            "7b2e19c4d8a3f051c6e4b2a9d01f8e7c5a4b3c21",
            diff,
            EvalSplit::LiveJudge,
        )
        .unwrap();

        assert_eq!(case.split, EvalSplit::LiveJudge);
        assert_eq!(case.id, "judge-httpx-3119");
        assert!(case.has_multimodal_media);
        assert_eq!(case.media_urls.len(), 1);
        assert!(case.ground_truth_test_patch.contains("tests/test_redirects.py"));
        assert!(!case.ground_truth_test_patch.contains("httpx/_client.py"));

        // Source-only diff without tests must be rejected.
        let source_only = "diff --git a/httpx/_client.py b/httpx/_client.py\n--- a/httpx/_client.py\n+++ b/httpx/_client.py\n@@ -1,1 +1,2 @@\n+x = 1\n";
        assert!(build_case_from_pr_parts(
            "encode",
            "httpx",
            99,
            98,
            "No tests",
            "body",
            "deadbeef",
            source_only,
            EvalSplit::LiveJudge
        )
        .is_err());
    }

    #[test]
    fn validates_curated_five_sigma_20_cases_manifest() {
        let cases = load_curated_cases();
        assert_eq!(cases.len(), 20, "manifest must contain exactly 20 cases");

        let calibration = cases
            .iter()
            .filter(|c| c.split == EvalSplit::Calibration)
            .count();
        let unseen = cases
            .iter()
            .filter(|c| c.split == EvalSplit::LockedUnseen)
            .count();
        let multimodal = cases.iter().filter(|c| c.has_multimodal_media).count();

        assert_eq!(calibration, 5, "must have exactly 5 Calibration cases");
        assert_eq!(unseen, 15, "must have exactly 15 LockedUnseen cases");
        assert!(
            multimodal >= 8,
            "must have >= 8 multimodal cases, got {multimodal}"
        );

        let mut seen_ids = HashSet::new();
        for case in &cases {
            assert!(seen_ids.insert(case.id.clone()), "duplicate case id {}", case.id);
            assert_eq!(
                case.parent_commit.len(),
                40,
                "parent_commit must be 40-char SHA for {}",
                case.id
            );
            assert!(
                case.parent_commit.chars().all(|c| c.is_ascii_hexdigit()),
                "parent_commit must be hex for {}",
                case.id
            );
            if case.has_multimodal_media {
                assert!(
                    !case.media_urls.is_empty(),
                    "multimodal case {} must have media_urls",
                    case.id
                );
            }
            let parsed_gt = parse_unified_diff(&case.ground_truth_test_patch);
            assert!(
                !parsed_gt.is_empty(),
                "ground_truth_test_patch must parse for {}",
                case.id
            );
            for file in &parsed_gt {
                assert!(
                    is_test_file_path(&file.path),
                    "ground_truth_test_patch for {} contains non-test file '{}'",
                    case.id,
                    file.path
                );
            }
        }
    }

    #[tokio::test]
    async fn grader_rejects_empty_diff_and_ground_truth_test_tampering() {
        let cases = load_curated_cases();
        let case = &cases[0];
        let cfg = GraderConfig {
            mode: GradingMode::OfflineDeterministic,
            ..GraderConfig::default()
        };

        let empty_sub = CandidateSubmission {
            case_id: case.id.clone(),
            arm: EvalArm::SingleModelCall,
            git_diff: String::new(),
            duration_ms: 1200,
            reported_speedup_pct: None,
            reported_blast_radius: None,
            notes: None,
        };
        let empty_out = grade_submission(case, &empty_sub, None, &cfg).await;
        assert!(!empty_out.resolved_ground_truth);
        assert_eq!(
            empty_out.failure_category.as_deref(),
            Some("empty_or_unparseable_diff")
        );

        let tampering_sub = CandidateSubmission {
            case_id: case.id.clone(),
            arm: EvalArm::SingleManagedAgent,
            git_diff: case.ground_truth_test_patch.clone(),
            duration_ms: 5000,
            reported_speedup_pct: None,
            reported_blast_radius: None,
            notes: None,
        };
        let tamper_out = grade_submission(case, &tampering_sub, None, &cfg).await;
        assert!(!tamper_out.resolved_ground_truth);
        assert!(!tamper_out.regression_free);
        assert_eq!(
            tamper_out.failure_category.as_deref(),
            Some("test_tampering_violation")
        );
    }

    #[test]
    fn blind_problem_statement_never_leaks_ground_truth_test_patch() {
        let cases = load_curated_cases();
        let case = &cases[0];
        let prompt = build_blind_problem_statement(case);
        assert!(prompt.contains(&case.title));
        assert!(prompt.contains(&case.media_urls[0]));
        assert!(
            !prompt.contains("test_cross_port_redirect_strips_authorization_header"),
            "hidden ground-truth test symbol must never appear in blind prompt"
        );
    }

    #[tokio::test]
    async fn dry_run_three_way_evaluation_produces_valid_scorecards_and_blast_scores() {
        let cases = load_curated_cases();
        let eval = run_evaluation(
            &cases,
            &RunConfig {
                split: SplitFilter::All,
                concurrency: 4,
                dry_run: true,
                label: Some("test-dry-run".to_string()),
                github_token: None,
            },
        )
        .await
        .expect("dry-run evaluation should succeed");

        assert_eq!(eval.cases.len(), 20);
        assert_eq!(eval.outcomes.len(), 60, "20 cases x 3 arms = 60 outcomes");
        assert_eq!(eval.scorecard.total_cases, 20);
        assert_eq!(eval.scorecard.calibration_cases, 5);
        assert_eq!(eval.scorecard.unseen_cases, 15);
        assert_eq!(eval.scorecard.arm_metrics.len(), 3);
        assert!(!eval.calibration_findings.is_empty());
        assert!(!eval.remaining_failure_modes.is_empty());

        for outcome in &eval.outcomes {
            assert!(
                outcome.blast_risk_score.is_some(),
                "every outcome should include epicenter-graph blast_risk_score"
            );
            assert!(outcome.root_cause_depth_score.is_some());
            assert!(outcome.dark_zone_touches.is_some());
        }

        let subsets = compute_subset_scorecards(&eval);
        assert_eq!(subsets.all.total_cases, 20);
        assert_eq!(subsets.locked_unseen.total_cases, 15);
        assert_eq!(subsets.calibration.total_cases, 5);
        assert!(subsets.multimodal_only.total_cases >= 8);

        let arm3 = subsets
            .locked_unseen
            .arm_metrics
            .iter()
            .find(|m| m.arm == EvalArm::EpicenterTournament)
            .unwrap();
        let arm1 = subsets
            .locked_unseen
            .arm_metrics
            .iter()
            .find(|m| m.arm == EvalArm::SingleModelCall)
            .unwrap();
        assert!(
            arm3.regression_free_rate_pct >= 85.0,
            "Epicenter Tournament should maintain high regression-free rate, got {}",
            arm3.regression_free_rate_pct
        );
        assert!(
            arm3.resolved_rate_pct > arm1.resolved_rate_pct,
            "Epicenter Tournament should outperform Single Model Call on locked unseen split"
        );
    }

    #[tokio::test]
    async fn markdown_report_contains_all_subsets_calibration_and_failure_taxonomy() {
        let cases = load_curated_cases();
        let eval = run_evaluation(
            &cases,
            &RunConfig {
                split: SplitFilter::All,
                concurrency: 4,
                dry_run: true,
                label: Some("offline-dry-run-baseline-seed".to_string()),
                github_token: None,
            },
        )
        .await
        .unwrap();

        let md = render_markdown_report(&eval);
        assert!(md.contains("# Epicenter 5-Sigma 3-Way Evaluation Report"));
        assert!(md.contains("### All Cases (20)"));
        assert!(md.contains("### Locked Unseen Split (15)"));
        assert!(md.contains("### Calibration Split (5)"));
        assert!(md.contains("### Multimodal-Only Subset"));
        assert!(md.contains("## Calibration Findings"));
        assert!(md.contains("Red-Gate"));
        assert!(md.contains("Carbon"));
        assert!(md.contains("Dual-Score"));
        assert!(md.contains("## Honest Remaining Failure Modes"));
        assert!(md.contains("## Per-Case Outcome Matrix"));

        let tmp = tempfile::tempdir().unwrap();
        let json_path = tmp.path().join("latest_five_sigma.json");
        let md_path = tmp.path().join("FIVE_SIGMA_REPORT.md");
        write_eval_artifacts(&eval, &json_path, &md_path).unwrap();

        let roundtrip: FiveSigmaEval =
            serde_json::from_str(&std::fs::read_to_string(&json_path).unwrap()).unwrap();
        assert_eq!(roundtrip.run_id, eval.run_id);
        assert_eq!(roundtrip.outcomes.len(), 60);
        assert!(std::fs::read_to_string(&md_path)
            .unwrap()
            .contains(&eval.run_id.to_string()));
    }

    #[tokio::test]
    async fn split_filter_runs_calibration_and_unseen_independently() {
        let cases = load_curated_cases();

        let cal_eval = run_evaluation(
            &cases,
            &RunConfig {
                split: SplitFilter::Calibration,
                concurrency: 2,
                dry_run: true,
                label: None,
                github_token: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(cal_eval.cases.len(), 5);
        assert_eq!(cal_eval.outcomes.len(), 15);

        let unseen_eval = run_evaluation(
            &cases,
            &RunConfig {
                split: SplitFilter::Unseen,
                concurrency: 3,
                dry_run: true,
                label: None,
                github_token: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(unseen_eval.cases.len(), 15);
        assert_eq!(unseen_eval.outcomes.len(), 45);
    }
}
