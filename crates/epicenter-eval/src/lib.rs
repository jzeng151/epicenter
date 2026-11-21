pub mod grader;
pub mod harvester;
pub mod report;
pub mod runner;
pub mod verify;

#[cfg(test)]
mod tests {
    use crate::grader::{
        detect_repo_language, grade_submission, strip_editable_hooks, CandidateSubmission,
        GraderConfig, GradingMode, RepoLanguage,
    };
    use crate::harvester::{
        build_case_from_pr_parts, extract_linked_issue_number,
        extract_linked_issue_number_for_repo, extract_media_urls, is_test_file_path, parse_pr_url,
        split_pr_diff_source_and_tests,
    };
    use crate::report::{compute_subset_scorecards, render_markdown_report, write_eval_artifacts};
    use crate::runner::{
        build_blind_problem_statement, load_calibration_findings, run_evaluation,
        validate_dry_run_plan, RunConfig, SplitFilter,
    };
    use crate::verify::{
        detect_placeholder_media_url, detect_synthetic_diff_indices,
        is_valid_and_non_synthetic_sha, verify_cases_static,
    };
    use chrono::Utc;
    use epicenter_common::{
        ArmOutcome, CalibrationFinding, EvalArm, EvalIssueCase, EvalScorecard, EvalSplit,
        FiveSigmaEval,
    };
    use epicenter_graph::parse_unified_diff;
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use uuid::Uuid;

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

    fn run_git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .current_dir(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@local.dev")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@local.dev")
            .output()
            .expect("git command should run");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// Creates a real temporary local Python git repository with a real bug on `parent_commit`,
    /// an existing passing test suite, a caller module, and a hidden ground-truth test patch that
    /// fails on `parent_commit` and passes when fixed.
    fn create_temp_python_git_repo() -> (tempfile::TempDir, EvalIssueCase, String, String, String) {
        let repo_dir = tempfile::Builder::new()
            .prefix("epicenter-test-repo-")
            .tempdir()
            .unwrap();
        let root = repo_dir.path();

        run_git(root, &["init", "-b", "main"]);
        std::fs::create_dir_all(root.join("src/calc")).unwrap();
        std::fs::create_dir_all(root.join("tests")).unwrap();

        std::fs::write(root.join("src/calc/__init__.py"), "").unwrap();
        std::fs::write(
            root.join("src/calc/core.py"),
            "def divide_safe(a, b):\n    if b == 0:\n        return None\n    return a / b\n\n\
             def strip_port_auth(origin_port, target_port, headers):\n    # Bug: ignores port change\n    return dict(headers)\n",
        )
        .unwrap();
        std::fs::write(
            root.join("src/calc/service.py"),
            "from calc.core import strip_port_auth\n\n\
             def forward_request(p1, p2, headers):\n    return strip_port_auth(p1, p2, headers)\n",
        )
        .unwrap();
        std::fs::write(
            root.join("tests/test_core.py"),
            "from calc.core import divide_safe, strip_port_auth\n\n\
             def test_divide_safe_basic():\n    assert divide_safe(10, 2) == 5\n    assert divide_safe(10, 0) is None\n\n\
             def test_same_port_keeps_auth():\n    out = strip_port_auth(443, 443, {'Authorization': 'Bearer x'})\n    assert out['Authorization'] == 'Bearer x'\n",
        )
        .unwrap();

        run_git(root, &["add", "."]);
        run_git(root, &["commit", "-m", "Initial buggy commit"]);
        let parent_sha = run_git(root, &["rev-parse", "HEAD"]);

        // Create ground-truth test patch via real `git diff` so blob indices and hunks are 100% genuine.
        std::fs::write(
            root.join("tests/test_core.py"),
            "from calc.core import divide_safe, strip_port_auth\n\n\
             def test_divide_safe_basic():\n    assert divide_safe(10, 2) == 5\n    assert divide_safe(10, 0) is None\n\n\
             def test_same_port_keeps_auth():\n    out = strip_port_auth(443, 443, {'Authorization': 'Bearer x'})\n    assert out['Authorization'] == 'Bearer x'\n\n\
             def test_cross_port_strips_auth():\n    out = strip_port_auth(8443, 9443, {'Authorization': 'Bearer x', 'Accept': '*/*'})\n    assert 'Authorization' not in out\n    assert out['Accept'] == '*/*'\n",
        )
        .unwrap();
        let gt_test_patch = format!("{}\n", run_git(root, &["diff", "tests/test_core.py"]));
        run_git(root, &["checkout", "--", "tests/test_core.py"]);

        // Create a valid candidate fix diff via real `git diff`.
        std::fs::write(
            root.join("src/calc/core.py"),
            "def divide_safe(a, b):\n    if b == 0:\n        return None\n    return a / b\n\n\
             def strip_port_auth(origin_port, target_port, headers):\n    out = dict(headers)\n    if origin_port != target_port:\n        out.pop('Authorization', None)\n    return out\n",
        )
        .unwrap();
        let valid_fix_diff = format!("{}\n", run_git(root, &["diff", "src/calc/core.py"]));
        run_git(root, &["checkout", "--", "src/calc/core.py"]);

        // Create a bad fix diff that does not fix cross-port stripping (leaves existing tests passing, fails GT test).
        std::fs::write(
            root.join("src/calc/core.py"),
            "def divide_safe(a, b):\n    if b == 0:\n        return None\n    return a / b\n\n\
             def strip_port_auth(origin_port, target_port, headers):\n    # Still does not strip Authorization\n    return dict(headers)\n",
        )
        .unwrap();
        let bad_fix_diff = format!("{}\n", run_git(root, &["diff", "src/calc/core.py"]));
        run_git(root, &["checkout", "--", "src/calc/core.py"]);

        // Create a regression-breaking fix that breaks existing `test_same_port_keeps_auth`.
        std::fs::write(
            root.join("src/calc/core.py"),
            "def divide_safe(a, b):\n    if b == 0:\n        return None\n    return a / b\n\n\
             def strip_port_auth(origin_port, target_port, headers):\n    out = dict(headers)\n    out.pop('Authorization', None)\n    return out\n",
        )
        .unwrap();
        let regression_fix_diff = format!("{}\n", run_git(root, &["diff", "src/calc/core.py"]));
        run_git(root, &["checkout", "--", "src/calc/core.py"]);

        let case = EvalIssueCase {
            id: "local-calc-1".to_string(),
            repo_url: format!("file://{}", root.display()),
            issue_number: 1,
            pr_number: Some(2),
            title: "Strip Authorization header when origin port changes".to_string(),
            issue_body: "Cross-port redirects leak Authorization header.".to_string(),
            parent_commit: parent_sha,
            ground_truth_test_patch: gt_test_patch,
            has_multimodal_media: false,
            media_urls: Vec::new(),
            split: EvalSplit::Calibration,
        };

        (
            repo_dir,
            case,
            valid_fix_diff,
            bad_fix_diff,
            regression_fix_diff,
        )
    }

    #[test]
    fn splits_unified_pr_diff_strictly_into_source_and_hidden_test_hunks() {
        let raw_diff = r#"diff --git a/httpx/_client.py b/httpx/_client.py
index 8a1b9c2..e4f7013 100644
--- a/httpx/_client.py
+++ b/httpx/_client.py
@@ -42,3 +42,4 @@ def _redirect_headers(self, request, url):
     if not same_origin(request.url, url):
         headers.pop("Authorization", None)
+        headers.pop("Proxy-Authorization", None)
     return headers
diff --git a/tests/client/test_redirects.py b/tests/client/test_redirects.py
index c3d19a4..f802be1 100644
--- a/tests/client/test_redirects.py
+++ b/tests/client/test_redirects.py
@@ -10,2 +10,5 @@ def test_redirect():
     assert True
+def test_redirect_strips_proxy_auth():
+    assert True
diff --git a/src/router/index.ts b/src/router/index.ts
index 4b82e1c..9d03f5a 100644
--- a/src/router/index.ts
+++ b/src/router/index.ts
@@ -1,3 +1,3 @@ export function matchRoute(path: string) {
 export function matchRoute(path: string) {
-  return path.endsWith('/');
+  return path.replace(/\/+$/, '');
 }
diff --git a/src/router/index.test.ts b/src/router/index.test.ts
index 2e94a7b..71c80d3 100644
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
    fn extracts_real_media_urls_and_ignores_badges_avatars_and_logos() {
        let markdown = r#"
Bug reproduction with screenshots and recording:
![ci-badge](https://img.shields.io/badge/build-passing-brightgreen.svg)
![avatar](https://avatars.githubusercontent.com/u/12345?v=4)
![logo](https://github.githubassets.com/images/modules/logos_page/GitHub-Mark.png)
![trace-screenshot](https://user-images.githubusercontent.com/42/error-trace.png "Error Trace")
Duplicate reference: https://user-images.githubusercontent.com/42/error-trace.png
<img width="800" src="https://github.com/user-attachments/assets/4c9b2a10-8e3f-4d19-9b62-7f0e3a51c8d4" />
<video src='https://cdn.realhost.io/demo/repro-recording.mp4'></video>
See docs at https://realhost.io/docs/guide (not media!) and flamegraph https://cdn.realhost.io/flame.webp.
"#;

        let urls = extract_media_urls(markdown);
        assert_eq!(
            urls,
            vec![
                "https://user-images.githubusercontent.com/42/error-trace.png",
                "https://github.com/user-attachments/assets/4c9b2a10-8e3f-4d19-9b62-7f0e3a51c8d4",
                "https://cdn.realhost.io/demo/repro-recording.mp4",
                "https://cdn.realhost.io/flame.webp",
            ]
        );
    }

    #[test]
    fn parses_github_pr_urls_and_extracts_linked_issues_including_full_urls() {
        let (owner, repo, num) =
            parse_pr_url("https://github.com/encode/httpx/pull/3119/files#diff-123").unwrap();
        assert_eq!(owner, "encode");
        assert_eq!(repo, "httpx");
        assert_eq!(num, 3119);

        assert_eq!(
            extract_linked_issue_number("This PR fixes #3112 by checking port origin."),
            Some(3112)
        );
        assert_eq!(
            extract_linked_issue_number("Closes: #4055"),
            Some(4055)
        );
        assert_eq!(
            extract_linked_issue_number_for_repo(
                "Resolves https://github.com/pallets/click/issues/2841",
                Some("pallets"),
                Some("click")
            ),
            Some(2841)
        );
        assert_eq!(
            extract_linked_issue_number_for_repo(
                "See https://github.com/pallets/flask/issues/5512 for repro screenshot",
                Some("pallets"),
                Some("flask")
            ),
            Some(5512)
        );

        let diff = "diff --git a/httpx/_client.py b/httpx/_client.py\n\
            index 8f31a0c..c294b1e 100644\n\
            --- a/httpx/_client.py\n+++ b/httpx/_client.py\n@@ -1,1 +1,2 @@\n+x = 1\n\
            diff --git a/tests/test_redirects.py b/tests/test_redirects.py\n\
            index 0000000..9d41e2f 100644\n\
            --- /dev/null\n+++ b/tests/test_redirects.py\n@@ -0,0 +1,2 @@\n+assert x == 1\n";

        let case = build_case_from_pr_parts(
            &owner,
            &repo,
            num,
            3112,
            "Strip auth on port change",
            "Fixes #3112\n![shot](https://user-images.githubusercontent.com/98712/shot.png)",
            "9f8e71c4d2a3b051c6e4f2a9d01f8e7c5a4b3c21",
            diff,
            EvalSplit::LiveJudge,
        )
        .unwrap();

        assert_eq!(case.split, EvalSplit::LiveJudge);
        assert_eq!(case.id, "judge-httpx-3119");
        assert!(case.has_multimodal_media);
        assert_eq!(case.media_urls.len(), 1);
    }

    #[test]
    fn verify_integrity_checker_catches_synthetic_shas_diff_indices_and_placeholder_media() {
        // Synthetic commit SHAs must fail.
        assert!(is_valid_and_non_synthetic_sha("1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a06").is_err());
        assert!(is_valid_and_non_synthetic_sha("2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b17").is_err());
        assert!(is_valid_and_non_synthetic_sha("3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b1c28").is_err());
        assert!(is_valid_and_non_synthetic_sha("9c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c13").is_err());
        assert!(is_valid_and_non_synthetic_sha("shortsha").is_err());
        assert!(is_valid_and_non_synthetic_sha("zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz").is_err());

        // Real git SHA must pass.
        assert!(is_valid_and_non_synthetic_sha("f4e81c92a7b03d65e1429a8f0c7d3b51e8a2904c").is_ok());

        // Synthetic diff indices must fail.
        let fake_diff = "diff --git a/tests/t.py b/tests/t.py\n\
            index 1111111..2222222 100644\n\
            --- a/tests/t.py\n+++ b/tests/t.py\n@@ -1,1 +1,2 @@\n+assert True\n";
        assert!(!detect_synthetic_diff_indices(fake_diff).is_empty());

        let fake_diff2 = "diff --git a/tests/t.py b/tests/t.py\n\
            index 3333333..4444444 100644\n\
            --- a/tests/t.py\n+++ b/tests/t.py\n@@ -1,1 +1,2 @@\n+assert True\n";
        assert!(!detect_synthetic_diff_indices(fake_diff2).is_empty());

        let fake_diff3 = "diff --git a/tests/t.py b/tests/t.py\n\
            index 1212121..2323232 100644\n\
            --- a/tests/t.py\n+++ b/tests/t.py\n@@ -1,1 +1,2 @@\n+assert True\n";
        assert!(!detect_synthetic_diff_indices(fake_diff3).is_empty());

        // Real new-file diff with 0000000..<real_hash> must pass!
        let real_new_file_diff = "diff --git a/tests/test_real.py b/tests/test_real.py\n\
            new file mode 100644\n\
            index 0000000..a83f19c\n\
            --- /dev/null\n+++ b/tests/test_real.py\n@@ -0,0 +1,2 @@\n+def test_ok():\n+    assert True\n";
        assert!(detect_synthetic_diff_indices(real_new_file_diff).is_empty());

        // Placeholder media URLs must fail.
        assert!(detect_placeholder_media_url(
            "https://github.com/user-attachments/assets/9481a1b2-c3d4-4e5f-8901-abcdef123456"
        )
        .is_some());
        assert!(detect_placeholder_media_url(
            "https://github.com/user-attachments/assets/1111-2222-3333-4444"
        )
        .is_some());
        assert!(detect_placeholder_media_url(
            "https://github.com/user-attachments/assets/aaaa-bbbb-cccc-dddd"
        )
        .is_some());

        // Real GitHub attachment UUID must pass.
        assert!(detect_placeholder_media_url(
            "https://github.com/user-attachments/assets/4c9b2a10-8e3f-4d19-9b62-7f0e3a51c8d4"
        )
        .is_none());

        // End-to-end static dataset report: fabricated case fails, real case passes.
        let fabricated = EvalIssueCase {
            id: "fake-case-1".to_string(),
            repo_url: "https://github.com/encode/httpx".to_string(),
            issue_number: 1,
            pr_number: Some(2),
            title: "Fake".to_string(),
            issue_body: "Fake".to_string(),
            parent_commit: "1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a06".to_string(),
            ground_truth_test_patch: fake_diff.to_string(),
            has_multimodal_media: true,
            media_urls: vec![
                "https://github.com/user-attachments/assets/9481a1b2-c3d4-4e5f-8901-abcdef123456"
                    .to_string(),
            ],
            split: EvalSplit::Calibration,
        };
        let bad_report = verify_cases_static(&[fabricated]);
        assert!(!bad_report.is_ok());
        assert!(bad_report.failures.len() >= 3);

        let genuine = EvalIssueCase {
            id: "cal-click-2841".to_string(),
            repo_url: "https://github.com/pallets/click".to_string(),
            issue_number: 2840,
            pr_number: Some(2841),
            title: "Real fix".to_string(),
            issue_body: "Real description".to_string(),
            parent_commit: "f4e81c92a7b03d65e1429a8f0c7d3b51e8a2904c".to_string(),
            ground_truth_test_patch: real_new_file_diff.to_string(),
            has_multimodal_media: true,
            media_urls: vec![
                "https://github.com/user-attachments/assets/4c9b2a10-8e3f-4d19-9b62-7f0e3a51c8d4"
                    .to_string(),
            ],
            split: EvalSplit::Calibration,
        };
        let good_report = verify_cases_static(&[genuine]);
        assert!(good_report.is_ok(), "unexpected failures: {:?}", good_report.failures);
    }

    #[tokio::test]
    async fn host_sandbox_independent_grader_verifies_real_git_repo_outcomes() {
        let (_repo_dir, case, valid_fix_diff, bad_fix_diff, regression_fix_diff) =
            create_temp_python_git_repo();
        let cache_dir = tempfile::Builder::new()
            .prefix("epicenter-test-cache-")
            .tempdir()
            .unwrap();
        let cfg = GraderConfig {
            mode: GradingMode::HostSandbox,
            cache_root: cache_dir.path().to_path_buf(),
            test_timeout_secs: 60,
            setup_timeout_secs: 120,
            run_regression_suite: true,
            ..GraderConfig::default()
        };

        // 1. Empty diff rejected cleanly.
        let empty_out = grade_submission(
            &case,
            &CandidateSubmission {
                case_id: case.id.clone(),
                arm: EvalArm::SingleModelCall,
                git_diff: String::new(),
                duration_ms: 100,
                reported_speedup_pct: None,
                reported_blast_radius: None,
                notes: None,
            },
            None,
            &cfg,
        )
        .await;
        assert!(!empty_out.resolved_ground_truth);
        assert_eq!(
            empty_out.failure_category.as_deref(),
            Some("empty_or_unparseable_diff")
        );

        // 2. Ground-truth test tampering rejected immediately.
        let tamper_out = grade_submission(
            &case,
            &CandidateSubmission {
                case_id: case.id.clone(),
                arm: EvalArm::SingleManagedAgent,
                git_diff: case.ground_truth_test_patch.clone(),
                duration_ms: 200,
                reported_speedup_pct: None,
                reported_blast_radius: None,
                notes: None,
            },
            None,
            &cfg,
        )
        .await;
        assert!(!tamper_out.resolved_ground_truth);
        assert!(!tamper_out.regression_free);
        assert_eq!(
            tamper_out.failure_category.as_deref(),
            Some("test_tampering_violation")
        );

        // 3. Patch conflict fails cleanly with patch_apply_conflict.
        let conflict_diff = "diff --git a/src/calc/core.py b/src/calc/core.py\n\
            --- a/src/calc/core.py\n+++ b/src/calc/core.py\n\
            @@ -1,3 +1,3 @@\n def nonexistent_line():\n-    old_code()\n+    new_code()\n";
        let conflict_out = grade_submission(
            &case,
            &CandidateSubmission {
                case_id: case.id.clone(),
                arm: EvalArm::SingleModelCall,
                git_diff: conflict_diff.to_string(),
                duration_ms: 300,
                reported_speedup_pct: None,
                reported_blast_radius: None,
                notes: None,
            },
            None,
            &cfg,
        )
        .await;
        assert!(!conflict_out.resolved_ground_truth);
        assert!(!conflict_out.regression_free);
        assert_eq!(
            conflict_out.failure_category.as_deref(),
            Some("patch_apply_conflict")
        );

        // 4. Valid fix passes with real pytest exit code 0 + real BlastRadiusReport from checked-out files!
        let valid_out = grade_submission(
            &case,
            &CandidateSubmission {
                case_id: case.id.clone(),
                arm: EvalArm::EpicenterTournament,
                git_diff: valid_fix_diff,
                duration_ms: 1500,
                reported_speedup_pct: None,
                reported_blast_radius: None,
                notes: None,
            },
            None,
            &cfg,
        )
        .await;
        assert!(
            valid_out.resolved_ground_truth,
            "valid fix should pass real pytest: {:?}",
            valid_out
        );
        assert!(
            valid_out.regression_free,
            "valid fix should remain regression-free: {:?}",
            valid_out
        );
        assert_eq!(valid_out.failure_category, None);
        assert!(
            valid_out.blast_risk_score.is_some(),
            "should compute real RepoCallGraph blast risk score from checked-out files"
        );
        assert!(
            valid_out.root_cause_depth_score.is_none(),
            "must never fabricate root_cause_depth_score when not audited by Carbon"
        );

        // 5. Bad fix fails real ground-truth test while existing tests stay green.
        let bad_out = grade_submission(
            &case,
            &CandidateSubmission {
                case_id: case.id.clone(),
                arm: EvalArm::SingleManagedAgent,
                git_diff: bad_fix_diff,
                duration_ms: 900,
                reported_speedup_pct: None,
                reported_blast_radius: None,
                notes: None,
            },
            None,
            &cfg,
        )
        .await;
        assert!(!bad_out.resolved_ground_truth);
        assert!(bad_out.regression_free);
        assert_eq!(
            bad_out.failure_category.as_deref(),
            Some("ground_truth_test_failed")
        );

        // 6. Regression-breaking fix fails regression check strictly from real OS exit code.
        let reg_out = grade_submission(
            &case,
            &CandidateSubmission {
                case_id: case.id.clone(),
                arm: EvalArm::SingleModelCall,
                git_diff: regression_fix_diff,
                duration_ms: 800,
                reported_speedup_pct: None,
                reported_blast_radius: None,
                notes: None,
            },
            None,
            &cfg,
        )
        .await;
        assert!(!reg_out.regression_free);
        assert!(reg_out.failure_category.is_some());
    }

    #[test]
    fn language_detector_never_runs_pytest_on_typescript_repos() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("package.json"), r#"{"name":"zod"}"#).unwrap();

        assert_eq!(
            detect_repo_language(tmp.path(), &["src/__tests__/coerce.test.ts".to_string()], &[]),
            RepoLanguage::TypeScriptJs
        );
        assert_eq!(
            detect_repo_language(tmp.path(), &[], &[]),
            RepoLanguage::TypeScriptJs
        );
    }

    #[test]
    fn strip_editable_hooks_removes_stale_pth_and_editable_finders() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = tmp.path().join("lib/python3.12/site-packages");
        std::fs::create_dir_all(&sp).unwrap();
        std::fs::write(sp.join("__editable__.demo-0.1.0.pth"), "/tmp/old-wt/src\n").unwrap();
        std::fs::write(sp.join("__editable___demo_0_1_0_finder.py"), "# finder\n").unwrap();
        std::fs::write(sp.join("custom_mirror.pth"), "/private/tmp/epicenter-grade-123/repo\n").unwrap();
        std::fs::write(sp.join("keep_me.pth"), "/opt/homebrew/lib/python3.12\n").unwrap();

        let removed = strip_editable_hooks(tmp.path()).unwrap();
        assert_eq!(removed, 3);
        assert!(!sp.join("__editable__.demo-0.1.0.pth").exists());
        assert!(!sp.join("__editable___demo_0_1_0_finder.py").exists());
        assert!(!sp.join("custom_mirror.pth").exists());
        assert!(sp.join("keep_me.pth").exists());
    }

    #[test]
    fn validates_curated_five_sigma_20_cases_manifest_passes_integrity_audit() {
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

        assert_eq!(calibration, 5, "must have exactly 5 Calibration cases");
        assert_eq!(unseen, 15, "must have exactly 15 LockedUnseen cases");

        let report = verify_cases_static(&cases);
        assert!(
            report.is_ok(),
            "curated dataset failed static integrity verification: {:?}",
            report.failures
        );

        let mut seen_ids = HashSet::new();
        for case in &cases {
            assert!(seen_ids.insert(case.id.clone()), "duplicate case id {}", case.id);
            let parsed_gt = parse_unified_diff(&case.ground_truth_test_patch);
            assert!(!parsed_gt.is_empty());
            for file in &parsed_gt {
                assert!(is_test_file_path(&file.path));
            }
        }
    }

    #[test]
    fn blind_problem_statement_never_leaks_ground_truth_test_patch() {
        let cases = load_curated_cases();
        let case = &cases[0];
        let prompt = build_blind_problem_statement(case);
        assert!(prompt.contains(&case.title));
        assert!(!prompt.contains(&case.ground_truth_test_patch));
    }

    #[tokio::test]
    async fn dry_run_validates_plan_only_and_never_fabricates_outcomes() {
        let cases = load_curated_cases();
        let cfg = RunConfig {
            split: SplitFilter::Calibration,
            concurrency: 2,
            dry_run: true,
            grading_mode: GradingMode::HostSandbox,
            label: None,
            github_token: None,
            calibration_findings: Vec::new(),
        };

        let plan = validate_dry_run_plan(&cases, &cfg).expect("dry-run plan validation should pass");
        assert_eq!(plan.selected_cases.len(), 5);
        assert!(plan.verification.is_ok());
        let summary = plan.render_summary();
        assert!(summary.contains("no synthetic outcomes or report files were written"));

        // Calling run_evaluation with dry_run=true must return Err instead of fabricating results.
        assert!(run_evaluation(&cases, &cfg).await.is_err());

        // Optional calibration findings defaults to empty Vec.
        assert!(load_calibration_findings(None).unwrap().is_empty());
    }

    #[test]
    fn markdown_report_renders_live_eval_and_roundtrips_artifacts() {
        let cases = load_curated_cases();
        let outcomes = vec![
            ArmOutcome {
                case_id: cases[0].id.clone(),
                arm: EvalArm::SingleModelCall,
                resolved_ground_truth: false,
                regression_free: true,
                speedup_pct: None,
                blast_risk_score: Some(24.0),
                root_cause_depth_score: None,
                dark_zone_touches: Some(1),
                duration_ms: 4200,
                notes: Some("HostSandbox GT FAIL".to_string()),
                failure_category: Some("ground_truth_test_failed".to_string()),
            },
            ArmOutcome {
                case_id: cases[0].id.clone(),
                arm: EvalArm::EpicenterTournament,
                resolved_ground_truth: true,
                regression_free: true,
                speedup_pct: Some(8.5),
                blast_risk_score: Some(12.0),
                root_cause_depth_score: Some(84.0),
                dark_zone_touches: Some(0),
                duration_ms: 41200,
                notes: Some("HostSandbox PASS".to_string()),
                failure_category: None,
            },
        ];
        let scorecard = EvalScorecard::from_outcomes(&cases, &outcomes);
        let eval = FiveSigmaEval {
            run_id: Uuid::new_v4(),
            label: "live-host-verified-test".to_string(),
            cases: cases.clone(),
            outcomes,
            scorecard,
            calibration_findings: vec![CalibrationFinding {
                failure_mode: "Observed real failure".to_string(),
                observed_on_case: cases[0].id.clone(),
                system_change_made: "Adjusted prompt".to_string(),
                impact_summary: "Passed host pytest".to_string(),
            }],
            remaining_failure_modes: vec![
                "Arm 1: Single Model Call — `ground_truth_test_failed` (1 case)".to_string(),
            ],
            created_at: Utc::now(),
        };

        let subsets = compute_subset_scorecards(&eval);
        assert_eq!(subsets.all.total_cases, 20);
        assert_eq!(subsets.calibration.total_cases, 5);
        assert_eq!(subsets.locked_unseen.total_cases, 15);

        let md = render_markdown_report(&eval);
        assert!(md.contains("# Epicenter 5-Sigma 3-Way Evaluation Report"));
        assert!(md.contains("Independent Host Sandbox Verification"));
        assert!(!md.contains("Offline Dry-Run Baseline Seed"));

        let tmp = tempfile::tempdir().unwrap();
        let json_path = tmp.path().join("latest_five_sigma.json");
        let md_path = tmp.path().join("FIVE_SIGMA_REPORT.md");
        write_eval_artifacts(&eval, &json_path, &md_path).unwrap();

        let roundtrip: FiveSigmaEval =
            serde_json::from_str(&std::fs::read_to_string(&json_path).unwrap()).unwrap();
        assert_eq!(roundtrip.run_id, eval.run_id);
        assert_eq!(roundtrip.outcomes.len(), 2);
    }
}
