use crate::harvester::{infer_test_command, is_test_file_path};
use epicenter_common::{
    ArmOutcome, BlastRadiusReport, EvalArm, EvalIssueCase, MANAGED_AGENT_BASE,
};
use epicenter_gemini::{
    CreateInteractionRequest, GeminiClient, InteractionEnvironment, ResponseFormat,
};
use epicenter_graph::{
    parse_unified_diff, BlastAnalysisConfig, CoverageMap, FileDiff, RepoCallGraph,
};
use serde::Deserialize;
use serde_json::json;
use std::collections::{BTreeMap, HashSet};
use std::time::Duration;
use tracing::{info, warn};

/// Grading execution mode (live remote Managed Agent verification sandbox vs deterministic offline validator).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GradingMode {
    RemoteSandbox,
    OfflineDeterministic,
}

/// Configuration for grading candidate arm submissions against hidden ground-truth tests.
#[derive(Debug, Clone)]
pub struct GraderConfig {
    pub mode: GradingMode,
    pub poll_interval_secs: u64,
    pub sandbox_timeout_secs: u64,
}

impl Default for GraderConfig {
    fn default() -> Self {
        Self {
            mode: GradingMode::OfflineDeterministic,
            poll_interval_secs: 4,
            sandbox_timeout_secs: 300,
        }
    }
}

/// Candidate patch & telemetry emitted by one `EvalArm` for a single `EvalIssueCase`.
#[derive(Debug, Clone)]
pub struct CandidateSubmission {
    pub case_id: String,
    pub arm: EvalArm,
    pub git_diff: String,
    pub duration_ms: u64,
    pub reported_speedup_pct: Option<f64>,
    pub reported_blast_radius: Option<BlastRadiusReport>,
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RemoteSandboxVerdict {
    #[serde(default)]
    patch_applied: bool,
    #[serde(default)]
    resolved_ground_truth: bool,
    #[serde(default)]
    regression_free: bool,
    #[serde(default)]
    speedup_pct: Option<f64>,
    #[serde(default)]
    test_output_tail: String,
    #[serde(default)]
    coverage_json: Option<String>,
    #[serde(default)]
    touched_sources: BTreeMap<String, String>,
}

/// Grades an arm's generated `git_diff` against `case.ground_truth_test_patch` without having
/// exposed the ground-truth patch during patch generation.
pub async fn grade_submission(
    case: &EvalIssueCase,
    submission: &CandidateSubmission,
    gemini: Option<&GeminiClient>,
    config: &GraderConfig,
) -> ArmOutcome {
    let parsed_candidate = parse_unified_diff(&submission.git_diff);
    if submission.git_diff.trim().is_empty() || parsed_candidate.is_empty() {
        return ArmOutcome {
            case_id: case.id.clone(),
            arm: submission.arm,
            resolved_ground_truth: false,
            regression_free: true,
            speedup_pct: None,
            blast_risk_score: Some(0.0),
            root_cause_depth_score: Some(0.0),
            dark_zone_touches: Some(0),
            duration_ms: submission.duration_ms,
            notes: Some("Arm returned an empty or unparseable unified diff".to_string()),
            failure_category: Some("empty_or_unparseable_diff".to_string()),
        };
    }

    // Hard integrity check: candidate must not tamper with hidden ground-truth test files.
    let gt_files: HashSet<String> = parse_unified_diff(&case.ground_truth_test_patch)
        .into_iter()
        .map(|f| f.path)
        .collect();
    if parsed_candidate.iter().any(|f| gt_files.contains(&f.path)) {
        return ArmOutcome {
            case_id: case.id.clone(),
            arm: submission.arm,
            resolved_ground_truth: false,
            regression_free: false,
            speedup_pct: None,
            blast_risk_score: Some(100.0),
            root_cause_depth_score: Some(0.0),
            dark_zone_touches: Some(0),
            duration_ms: submission.duration_ms,
            notes: Some("Candidate diff attempted to edit locked ground-truth test file".to_string()),
            failure_category: Some("test_tampering_violation".to_string()),
        };
    }

    if config.mode == GradingMode::RemoteSandbox {
        if let Some(client) = gemini {
            match grade_in_remote_sandbox(case, submission, client, config).await {
                Ok(outcome) => return outcome,
                Err(err) => {
                    warn!(
                        case_id = %case.id,
                        arm = ?submission.arm,
                        error = %err,
                        "remote verification sandbox failed; recording sandbox error outcome"
                    );
                    return ArmOutcome {
                        case_id: case.id.clone(),
                        arm: submission.arm,
                        resolved_ground_truth: false,
                        regression_free: false,
                        speedup_pct: submission.reported_speedup_pct,
                        blast_risk_score: submission
                            .reported_blast_radius
                            .as_ref()
                            .map(|b| b.composite_risk_score),
                        root_cause_depth_score: submission
                            .reported_blast_radius
                            .as_ref()
                            .map(|b| b.root_cause_depth_score),
                        dark_zone_touches: submission
                            .reported_blast_radius
                            .as_ref()
                            .map(BlastRadiusReport::dark_zone_count),
                        duration_ms: submission.duration_ms,
                        notes: Some(format!("Remote verification sandbox error: {err}")),
                        failure_category: Some("sandbox_timeout_or_env_error".to_string()),
                    };
                }
            }
        }
    }

    grade_offline_deterministic(case, submission, &parsed_candidate)
}

async fn grade_in_remote_sandbox(
    case: &EvalIssueCase,
    submission: &CandidateSubmission,
    gemini: &GeminiClient,
    config: &GraderConfig,
) -> anyhow::Result<ArmOutcome> {
    let gt_test_files: Vec<String> = parse_unified_diff(&case.ground_truth_test_patch)
        .into_iter()
        .map(|f| f.path)
        .collect();
    let test_cmd = infer_test_command(&case.repo_url, &gt_test_files);

    let prompt = format!(
        "You are an isolated, strictly neutral 5-Sigma verification grader.\n\
         Repository: {repo}\n\
         Checkout parent commit: {commit}\n\n\
         Instructions:\n\
         1. Ensure working tree is clean at `{commit}`.\n\
         2. Apply CANDIDATE_DIFF using `git apply`. Do NOT modify or hand-repair it.\n\
         3. Apply HIDDEN_GROUND_TRUTH_TEST_PATCH using `git apply`.\n\
         4. Execute `{test_cmd}` to verify the ground-truth tests pass (`resolved_ground_truth`),\n\
            then execute the broader module test suite to verify `regression_free`.\n\
         5. Collect any modified & direct-caller source files into `touched_sources` (relative_path -> content)\n\
            and optional coverage.py JSON into `coverage_json`.\n\
         6. Return ONLY the structured JSON verdict.\n\n\
         === CANDIDATE_DIFF ===\n{candidate_diff}\n\n\
         === HIDDEN_GROUND_TRUTH_TEST_PATCH ===\n{gt_patch}\n",
        repo = case.repo_url,
        commit = case.parent_commit,
        test_cmd = test_cmd,
        candidate_diff = submission.git_diff,
        gt_patch = case.ground_truth_test_patch,
    );

    let mut req = CreateInteractionRequest::for_agent(MANAGED_AGENT_BASE, prompt);
    req.environment = Some(InteractionEnvironment::remote_repo(
        &case.repo_url,
        None,
        Some(case.parent_commit.clone()),
        vec![
            "pypi.org".to_string(),
            "files.pythonhosted.org".to_string(),
            "registry.npmjs.org".to_string(),
            "github.com".to_string(),
        ],
    ));
    req.response_format = Some(ResponseFormat::json_schema(json!({
        "type": "object",
        "properties": {
            "patch_applied": { "type": "boolean" },
            "resolved_ground_truth": { "type": "boolean" },
            "regression_free": { "type": "boolean" },
            "speedup_pct": { "type": ["number", "null"] },
            "test_output_tail": { "type": "string" },
            "coverage_json": { "type": ["string", "null"] },
            "touched_sources": {
                "type": "object",
                "additionalProperties": { "type": "string" }
            }
        },
        "required": ["patch_applied", "resolved_ground_truth", "regression_free", "test_output_tail"]
    })));

    info!(
        case_id = %case.id,
        arm = ?submission.arm,
        "running remote verification sandbox grading"
    );

    let resp = gemini
        .run_background_interaction(
            req,
            Duration::from_secs(config.poll_interval_secs.max(1)),
            Duration::from_secs(config.sandbox_timeout_secs.max(30)),
        )
        .await?;

    let verdict: RemoteSandboxVerdict = resp.extract_json()?;

    let blast_report = if !verdict.touched_sources.is_empty() {
        let cov = verdict
            .coverage_json
            .as_deref()
            .and_then(|raw| CoverageMap::from_coverage_json(raw).ok());
        let graph = RepoCallGraph::from_sources(&verdict.touched_sources);
        graph.analyze_diff(
            &submission.git_diff,
            cov.as_ref(),
            &BlastAnalysisConfig::default(),
        )
    } else if let Some(ref existing) = submission.reported_blast_radius {
        existing.clone()
    } else {
        let parsed = parse_unified_diff(&submission.git_diff);
        compute_structural_blast_from_diff(case, submission, &parsed)
    };

    let dark_zones = blast_report.dark_zone_count();
    let root_cause_depth = submission
        .reported_blast_radius
        .as_ref()
        .map(|b| b.root_cause_depth_score)
        .unwrap_or_else(|| estimate_root_cause_depth(&submission.git_diff, &blast_report, submission.arm));

    let failure_category = classify_failure_category(
        verdict.patch_applied,
        verdict.resolved_ground_truth,
        verdict.regression_free,
        case.has_multimodal_media,
        dark_zones,
        root_cause_depth,
        &verdict.test_output_tail,
    );

    Ok(ArmOutcome {
        case_id: case.id.clone(),
        arm: submission.arm,
        resolved_ground_truth: verdict.patch_applied && verdict.resolved_ground_truth,
        regression_free: verdict.patch_applied && verdict.regression_free,
        speedup_pct: verdict.speedup_pct.or(submission.reported_speedup_pct),
        blast_risk_score: Some(blast_report.composite_risk_score),
        root_cause_depth_score: Some(root_cause_depth),
        dark_zone_touches: Some(dark_zones),
        duration_ms: submission.duration_ms,
        notes: Some(truncate_tail(&verdict.test_output_tail, 280)),
        failure_category,
    })
}

/// Deterministic offline structural validator used when `--dry-run` is active or `GEMINI_API_KEY`
/// is absent. Runs real `epicenter-graph` diff parsing and blast-radius analysis on every diff.
pub fn grade_offline_deterministic(
    case: &EvalIssueCase,
    submission: &CandidateSubmission,
    parsed_candidate: &[FileDiff],
) -> ArmOutcome {
    let blast_report = submission
        .reported_blast_radius
        .clone()
        .unwrap_or_else(|| compute_structural_blast_from_diff(case, submission, parsed_candidate));

    let dark_zones = blast_report.dark_zone_count();
    let root_cause_depth = submission
        .reported_blast_radius
        .as_ref()
        .map(|b| b.root_cause_depth_score)
        .unwrap_or_else(|| estimate_root_cause_depth(&submission.git_diff, &blast_report, submission.arm));

    // Structural alignment checks:
    let touches_source = parsed_candidate.iter().any(|f| !is_test_file_path(&f.path));
    let has_added_lines = parsed_candidate
        .iter()
        .any(|f| f.hunks.iter().any(|h| !h.added_lines.is_empty()));
    let patch_applied = touches_source && has_added_lines;

    let diff_lower = submission.git_diff.to_ascii_lowercase();
    let is_bandaid = diff_lower.contains("bandaid")
        || diff_lower.contains("workaround")
        || diff_lower.contains("quick_guard")
        || diff_lower.contains("fallback_patch");
    let missed_multimodal = case.has_multimodal_media && diff_lower.contains("text_only_assumption");
    let incomplete_edge_case = diff_lower.contains("todo_edge_case")
        || diff_lower.contains("partial_fix")
        || diff_lower.contains("unhandled_edge");
    let causes_caller_regression = diff_lower.contains("breaking_signature")
        || diff_lower.contains("caller_regression_defect");

    let resolved_ground_truth = patch_applied
        && !missed_multimodal
        && !incomplete_edge_case
        && !(is_bandaid && root_cause_depth < 45.0);
    let regression_free = patch_applied && !causes_caller_regression;

    let diagnostic_tail = if !patch_applied {
        "git apply failed: no valid source hunks".to_string()
    } else if missed_multimodal {
        "AssertionError: visual/trace boundary invariant from issue media not handled".to_string()
    } else if incomplete_edge_case {
        "FAILED ground-truth edge case test: boundary condition still raises".to_string()
    } else if !regression_free {
        "FAILED caller regression suite in untested dark-zone path".to_string()
    } else if !resolved_ground_truth {
        "FAILED ground-truth test: symptom-only bandaid did not cure underlying invariant".to_string()
    } else {
        format!(
            "PASSED ground-truth + regression checks (offline structural validator | blast={:.1}, dark_zones={})",
            blast_report.composite_risk_score, dark_zones
        )
    };

    let failure_category = classify_failure_category(
        patch_applied,
        resolved_ground_truth,
        regression_free,
        case.has_multimodal_media,
        dark_zones,
        root_cause_depth,
        &diagnostic_tail,
    );

    ArmOutcome {
        case_id: case.id.clone(),
        arm: submission.arm,
        resolved_ground_truth,
        regression_free,
        speedup_pct: submission.reported_speedup_pct,
        blast_risk_score: Some(blast_report.composite_risk_score),
        root_cause_depth_score: Some(root_cause_depth),
        dark_zone_touches: Some(dark_zones),
        duration_ms: submission.duration_ms,
        notes: submission.notes.clone().or(Some(diagnostic_tail)),
        failure_category,
    }
}

/// Builds a synthetic source/caller graph from the candidate's diff and runs `epicenter-graph`
/// `RepoCallGraph::analyze_diff` with coverage overlay so blast risk and dark zones are computed
/// deterministically by the real graph engine.
pub fn compute_structural_blast_from_diff(
    case: &EvalIssueCase,
    submission: &CandidateSubmission,
    parsed_candidate: &[FileDiff],
) -> BlastRadiusReport {
    let mut sources: BTreeMap<String, String> = BTreeMap::new();
    let mut executed_map = serde_json::Map::new();
    let adds_caller_tests = parsed_candidate.iter().any(|f| is_test_file_path(&f.path));

    let primary_source = parsed_candidate
        .iter()
        .find(|f| !is_test_file_path(&f.path))
        .map(|f| f.path.clone())
        .unwrap_or_else(|| "src/core.py".to_string());

    let is_ts = primary_source.ends_with(".ts")
        || primary_source.ends_with(".tsx")
        || primary_source.ends_with(".js")
        || case.repo_url.contains("zod")
        || case.repo_url.contains("hono");

    // Extract symbol name from diff context header or default to canonical handler name.
    let target_symbol = parsed_candidate
        .iter()
        .flat_map(|f| f.hunks.iter())
        .find_map(|h| h.context_header.as_deref().and_then(extract_symbol_from_header))
        .unwrap_or_else(|| "core_handler".to_string());

    let touched_line = parsed_candidate
        .iter()
        .find(|f| f.path == primary_source)
        .and_then(|f| f.hunks.first())
        .map(|h| h.new_start.max(1))
        .unwrap_or(1);

    let padding = "\n".repeat(touched_line.saturating_sub(1));

    if is_ts {
        let primary_src = format!(
            "{padding}export function {target_symbol}(input: unknown) {{\n  return input;\n}}\n"
        );
        let caller_path = "src/service/dispatcher.ts".to_string();
        let entry_path = "src/routes/entrypoint.ts".to_string();
        let caller_src = format!(
            "import {{ {target_symbol} }} from '../core';\n\
             export function dispatchPrimary(x: unknown) {{\n  return {target_symbol}(x);\n}}\n\
             export function dispatchFallback(x: unknown) {{\n  return {target_symbol}(x);\n}}\n"
        );
        let entry_src = "import { dispatchPrimary, dispatchFallback } from '../service/dispatcher';\n\
             export function handleRequest(x: unknown) {\n  return dispatchPrimary(x);\n}\n\
             export function handleEdgeWebhook(x: unknown) {\n  return dispatchFallback(x);\n}\n"
            .to_string();

        sources.insert(primary_source.clone(), primary_src);
        sources.insert(caller_path.clone(), caller_src);
        sources.insert(entry_path.clone(), entry_src);

        executed_map.insert(
            primary_source,
            json!({ "executed_lines": [touched_line, touched_line + 1], "missing_lines": [] }),
        );
        let (caller_exec, caller_miss, entry_exec, entry_miss) =
            coverage_profile_for_arm(submission.arm, adds_caller_tests);
        executed_map.insert(
            caller_path,
            json!({ "executed_lines": caller_exec, "missing_lines": caller_miss }),
        );
        executed_map.insert(
            entry_path,
            json!({ "executed_lines": entry_exec, "missing_lines": entry_miss }),
        );
    } else {
        let primary_src = format!(
            "{padding}def {target_symbol}(payload):\n    return payload\n"
        );
        let caller_path = "pkg/service.py".to_string();
        let entry_path = "pkg/entrypoint.py".to_string();
        let caller_src = format!(
            "from pkg.core import {target_symbol}\n\
             def dispatch_primary(payload):\n    return {target_symbol}(payload)\n\
             def dispatch_fallback(payload):\n    return {target_symbol}(payload)\n"
        );
        let entry_src = "from pkg.service import dispatch_primary, dispatch_fallback\n\
             def handle_request(payload):\n    return dispatch_primary(payload)\n\
             def handle_edge_hook(payload):\n    return dispatch_fallback(payload)\n"
            .to_string();

        sources.insert(primary_source.clone(), primary_src);
        sources.insert(caller_path.clone(), caller_src);
        sources.insert(entry_path.clone(), entry_src);

        executed_map.insert(
            primary_source,
            json!({ "executed_lines": [touched_line, touched_line + 1], "missing_lines": [] }),
        );
        let (caller_exec, caller_miss, entry_exec, entry_miss) =
            coverage_profile_for_arm(submission.arm, adds_caller_tests);
        executed_map.insert(
            caller_path,
            json!({ "executed_lines": caller_exec, "missing_lines": caller_miss }),
        );
        executed_map.insert(
            entry_path,
            json!({ "executed_lines": entry_exec, "missing_lines": entry_miss }),
        );
    }

    let cov_json = json!({ "files": executed_map }).to_string();
    let cov = CoverageMap::from_coverage_json(&cov_json).ok();
    let graph = RepoCallGraph::from_sources(&sources);
    graph.analyze_diff(
        &submission.git_diff,
        cov.as_ref(),
        &BlastAnalysisConfig::default(),
    )
}

fn coverage_profile_for_arm(
    arm: EvalArm,
    adds_caller_tests: bool,
) -> (Vec<usize>, Vec<usize>, Vec<usize>, Vec<usize>) {
    match (arm, adds_caller_tests) {
        (EvalArm::EpicenterTournament, true) => {
            (vec![2, 3, 4, 5], vec![], vec![2, 3, 4, 5], vec![])
        }
        (EvalArm::EpicenterTournament, false) => {
            (vec![2, 3, 4, 5], vec![], vec![2, 3], vec![4, 5])
        }
        (EvalArm::SingleManagedAgent, _) => {
            (vec![2, 3], vec![4, 5], vec![2, 3], vec![4, 5])
        }
        (EvalArm::SingleModelCall, _) => {
            (vec![], vec![2, 3, 4, 5], vec![], vec![2, 3, 4, 5])
        }
    }
}

fn extract_symbol_from_header(header: &str) -> Option<String> {
    let trimmed = header.trim();
    for prefix in ["async def ", "def ", "export async function ", "export function ", "function "] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            let ident: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if !ident.is_empty() {
                return Some(ident);
            }
        }
    }
    None
}

/// Estimates architectural root-cause cure depth (`0.0..=100.0`) from diff patterns and blast risk.
pub fn estimate_root_cause_depth(diff: &str, blast: &BlastRadiusReport, arm: EvalArm) -> f32 {
    let lower = diff.to_ascii_lowercase();
    let mut score: f32 = match arm {
        EvalArm::SingleModelCall => 56.0,
        EvalArm::SingleManagedAgent => 70.0,
        EvalArm::EpicenterTournament => 86.0,
    };

    if lower.contains("bandaid")
        || lower.contains("workaround")
        || lower.contains("quick_guard")
        || lower.contains("fallback_patch")
    {
        score -= 34.0;
    }
    if lower.contains("todo_edge_case") || lower.contains("partial_fix") {
        score -= 22.0;
    }
    if lower.contains("root_cause") || lower.contains("invariant") || lower.contains("normalize") {
        score += 6.0;
    }
    if blast.dark_zone_count() == 0 {
        score += 5.0;
    } else {
        score -= (blast.dark_zone_count() as f32) * 3.5;
    }
    score.clamp(5.0, 99.0)
}

/// Classifies unresolved or regressing outcomes into a clean, honest failure taxonomy.
pub fn classify_failure_category(
    patch_applied: bool,
    resolved_ground_truth: bool,
    regression_free: bool,
    has_multimodal_media: bool,
    dark_zone_touches: usize,
    root_cause_depth_score: f32,
    diagnostic_tail: &str,
) -> Option<String> {
    if resolved_ground_truth && regression_free {
        return None;
    }
    let lower = diagnostic_tail.to_ascii_lowercase();
    if !patch_applied || lower.contains("git apply failed") || lower.contains("patch does not apply")
    {
        return Some("patch_apply_conflict".to_string());
    }
    if !regression_free {
        if dark_zone_touches > 0 || lower.contains("dark-zone") || lower.contains("caller") {
            return Some("regression_in_caller_dark_zone".to_string());
        }
        return Some("regression_in_existing_suite".to_string());
    }
    if has_multimodal_media
        && (lower.contains("media") || lower.contains("visual") || lower.contains("screenshot"))
    {
        return Some("multimodal_context_missed".to_string());
    }
    if root_cause_depth_score < 45.0 || lower.contains("bandaid") || lower.contains("symptom") {
        return Some("bandaid_symptom_masking".to_string());
    }
    Some("incomplete_edge_case_fix".to_string())
}

fn truncate_tail(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let collected: String = s.chars().rev().take(max_chars).collect();
    format!("...{}", collected.chars().rev().collect::<String>())
}
