use crate::harvester::{infer_test_command, is_test_file_path};
use crate::verify::parse_github_repo_slug;
use anyhow::{anyhow, Context, Result};
use clap::ValueEnum;
use epicenter_common::{
    ArmOutcome, BlastRadiusReport, EvalArm, EvalIssueCase, MANAGED_AGENT_BASE,
};
use epicenter_gemini::{
    CreateInteractionRequest, GeminiClient, InteractionEnvironment, ResponseFormat,
};
use epicenter_graph::{
    normalize_rel_path, parse_unified_diff, BlastAnalysisConfig, CoverageMap, FileDiff,
    RepoCallGraph,
};
use serde::Deserialize;
use serde_json::json;
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::Duration;
use tokio::process::Command;
use tracing::{debug, info, warn};

/// Independent grading execution mode.
///
/// `HostSandbox` is the default live grading mode: it checks out `case.parent_commit` in an
/// isolated temporary worktree backed by a cached local git mirror, applies candidate + hidden
/// ground-truth patches with real `git apply`, runs the repository's real test runner via
/// `tokio::process::Command`, and computes blast radius from real checked-out repository files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum GradingMode {
    HostSandbox,
    RemoteSandbox,
}

/// Detected primary language / test toolchain for a checked-out repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoLanguage {
    Python,
    TypeScriptJs,
    Rust,
    Go,
    Unknown,
}

/// Configuration for independent grading of candidate arm submissions against hidden ground-truth tests.
#[derive(Debug, Clone)]
pub struct GraderConfig {
    pub mode: GradingMode,
    pub cache_root: PathBuf,
    pub test_timeout_secs: u64,
    pub setup_timeout_secs: u64,
    pub run_regression_suite: bool,
    pub poll_interval_secs: u64,
    pub sandbox_timeout_secs: u64,
}

impl Default for GraderConfig {
    fn default() -> Self {
        Self {
            mode: GradingMode::HostSandbox,
            cache_root: PathBuf::from("/tmp/epicenter-cache"),
            test_timeout_secs: 60,
            setup_timeout_secs: 180,
            run_regression_suite: true,
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

#[derive(Debug)]
struct CommandExecResult {
    success: bool,
    exit_code: Option<i32>,
    timed_out: bool,
    combined_output: String,
}

type NamedLockRegistry = OnceLock<StdMutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>;

static REPO_LOCKS: NamedLockRegistry = OnceLock::new();
static ENV_LOCKS: NamedLockRegistry = OnceLock::new();

fn get_named_lock(
    registry: &NamedLockRegistry,
    key: &str,
) -> Arc<tokio::sync::Mutex<()>> {
    let map = registry.get_or_init(|| StdMutex::new(HashMap::new()));
    let mut guard = map.lock().expect("lock map mutex poisoned");
    guard
        .entry(key.to_string())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

/// Grades an arm's generated `git_diff` against hidden `case.ground_truth_test_patch`.
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
            blast_risk_score: None,
            root_cause_depth_score: None,
            dark_zone_touches: None,
            duration_ms: submission.duration_ms,
            notes: Some("Arm returned an empty or unparseable unified diff".to_string()),
            failure_category: Some("empty_or_unparseable_diff".to_string()),
        };
    }

    // Hard anti-tampering check: candidate diff must NEVER touch any file in ground_truth_test_patch.
    let gt_files: HashSet<String> = parse_unified_diff(&case.ground_truth_test_patch)
        .into_iter()
        .map(|f| normalize_rel_path(&f.path))
        .collect();
    if parsed_candidate
        .iter()
        .any(|f| gt_files.contains(&normalize_rel_path(&f.path)))
    {
        return ArmOutcome {
            case_id: case.id.clone(),
            arm: submission.arm,
            resolved_ground_truth: false,
            regression_free: false,
            speedup_pct: None,
            blast_risk_score: None,
            root_cause_depth_score: None,
            dark_zone_touches: None,
            duration_ms: submission.duration_ms,
            notes: Some("Candidate diff attempted to edit locked ground-truth test file".to_string()),
            failure_category: Some("test_tampering_violation".to_string()),
        };
    }

    match config.mode {
        GradingMode::HostSandbox => {
            match grade_in_host_sandbox(case, submission, &parsed_candidate, config).await {
                Ok(outcome) => outcome,
                Err(err) => {
                    warn!(
                        case_id = %case.id,
                        arm = ?submission.arm,
                        error = %err,
                        "host sandbox grading failed; recording honest env/setup error"
                    );
                    ArmOutcome {
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
                        notes: Some(format!("Host sandbox error: {err}")),
                        failure_category: Some("sandbox_env_setup_error".to_string()),
                    }
                }
            }
        }
        GradingMode::RemoteSandbox => {
            let Some(client) = gemini else {
                return ArmOutcome {
                    case_id: case.id.clone(),
                    arm: submission.arm,
                    resolved_ground_truth: false,
                    regression_free: false,
                    speedup_pct: None,
                    blast_risk_score: None,
                    root_cause_depth_score: None,
                    dark_zone_touches: None,
                    duration_ms: submission.duration_ms,
                    notes: Some("RemoteSandbox grading requested without GEMINI_API_KEY".to_string()),
                    failure_category: Some("sandbox_env_setup_error".to_string()),
                };
            };
            match grade_in_remote_sandbox(case, submission, client, config).await {
                Ok(outcome) => outcome,
                Err(err) => {
                    warn!(
                        case_id = %case.id,
                        arm = ?submission.arm,
                        error = %err,
                        "remote verification sandbox failed; recording sandbox error outcome"
                    );
                    ArmOutcome {
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
                    }
                }
            }
        }
    }
}

/// Detects repository language cleanly from ground-truth test files, candidate files, and worktree
/// manifests. Never selects Python/pytest for TypeScript/JavaScript repositories.
pub fn detect_repo_language(
    worktree: &Path,
    gt_test_files: &[String],
    candidate_files: &[FileDiff],
) -> RepoLanguage {
    let classify_path = |p: &str| -> Option<RepoLanguage> {
        let lower = p.to_ascii_lowercase();
        if lower.ends_with(".ts")
            || lower.ends_with(".tsx")
            || lower.ends_with(".mts")
            || lower.ends_with(".cts")
            || lower.ends_with(".js")
            || lower.ends_with(".jsx")
            || lower.ends_with(".mjs")
            || lower.ends_with(".cjs")
        {
            Some(RepoLanguage::TypeScriptJs)
        } else if lower.ends_with(".py") {
            Some(RepoLanguage::Python)
        } else if lower.ends_with(".rs") {
            Some(RepoLanguage::Rust)
        } else if lower.ends_with(".go") {
            Some(RepoLanguage::Go)
        } else {
            None
        }
    };

    for f in gt_test_files {
        if let Some(lang) = classify_path(f) {
            return lang;
        }
    }
    for f in candidate_files {
        if let Some(lang) = classify_path(&f.path) {
            return lang;
        }
    }

    if worktree.join("package.json").exists() {
        return RepoLanguage::TypeScriptJs;
    }
    if worktree.join("pyproject.toml").exists()
        || worktree.join("setup.py").exists()
        || worktree.join("setup.cfg").exists()
        || worktree.join("pytest.ini").exists()
    {
        return RepoLanguage::Python;
    }
    if worktree.join("Cargo.toml").exists() {
        return RepoLanguage::Rust;
    }
    if worktree.join("go.mod").exists() {
        return RepoLanguage::Go;
    }

    RepoLanguage::Unknown
}

fn repo_mirror_key(repo_url: &str) -> String {
    if let Ok((owner, repo)) = parse_github_repo_slug(repo_url) {
        return format!("{}__{}", sanitize_ident(&owner), sanitize_ident(&repo));
    }

    let trimmed = repo_url
        .trim()
        .strip_prefix("file://")
        .unwrap_or(repo_url.trim());
    let base = Path::new(trimmed)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("repo");
    let mut hasher = DefaultHasher::new();
    repo_url.hash(&mut hasher);
    format!("local__{}_{:08x}", sanitize_ident(base), hasher.finish() as u32)
}

fn sanitize_ident(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn git_clone_source(repo_url: &str) -> String {
    let trimmed = repo_url.trim();
    if let Some(local) = trimmed.strip_prefix("file://") {
        local.to_string()
    } else {
        trimmed.to_string()
    }
}

async fn run_cmd_with_timeout(
    mut cmd: Command,
    timeout_secs: u64,
) -> Result<CommandExecResult> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let child = cmd.spawn().context("failed to spawn subprocess")?;
    match tokio::time::timeout(Duration::from_secs(timeout_secs.max(1)), child.wait_with_output())
        .await
    {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let mut combined = String::new();
            if !stdout.trim().is_empty() {
                combined.push_str(&stdout);
            }
            if !stderr.trim().is_empty() {
                if !combined.is_empty() {
                    combined.push('\n');
                }
                combined.push_str(&stderr);
            }
            Ok(CommandExecResult {
                success: output.status.success(),
                exit_code: output.status.code(),
                timed_out: false,
                combined_output: combined,
            })
        }
        Ok(Err(err)) => Err(anyhow!("subprocess wait error: {err}")),
        Err(_) => Ok(CommandExecResult {
            success: false,
            exit_code: None,
            timed_out: true,
            combined_output: format!("Process exceeded {timeout_secs}s timeout and was terminated."),
        }),
    }
}

/// Ensures a bare git mirror exists at `<cache_root>/repos/<repo_key>`, fetches `parent_commit`
/// if missing, and checks out an isolated detached worktree at `worktree_path`.
async fn checkout_isolated_worktree(
    repo_url: &str,
    parent_commit: &str,
    cache_root: &Path,
    worktree_path: &Path,
    setup_timeout_secs: u64,
) -> Result<(PathBuf, String)> {
    let repo_key = repo_mirror_key(repo_url);
    let repos_dir = cache_root.join("repos");
    std::fs::create_dir_all(&repos_dir)
        .with_context(|| format!("failed to create mirror cache dir {}", repos_dir.display()))?;
    let mirror_dir = repos_dir.join(&repo_key);
    let clone_src = git_clone_source(repo_url);

    let repo_lock = get_named_lock(&REPO_LOCKS, &mirror_dir.to_string_lossy());
    let _guard = repo_lock.lock().await;

    if !mirror_dir.exists() {
        info!(
            repo = %repo_url,
            mirror = %mirror_dir.display(),
            "cloning bare git mirror for host sandbox grading"
        );
        let mut clone_cmd = Command::new("git");
        clone_cmd
            .env("GIT_TERMINAL_PROMPT", "0")
            .args(["clone", "--mirror", &clone_src])
            .arg(&mirror_dir);
        let res = run_cmd_with_timeout(clone_cmd, setup_timeout_secs).await?;
        if !res.success {
            let _ = std::fs::remove_dir_all(&mirror_dir);
            return Err(anyhow!(
                "git clone --mirror failed for {}: {}",
                repo_url,
                truncate_tail(&res.combined_output, 300)
            ));
        }
    }

    // Check if parent_commit exists in the local mirror; fetch if absent.
    let commit_spec = format!("{parent_commit}^{{commit}}");
    let mut check_cmd = Command::new("git");
    check_cmd
        .env("GIT_TERMINAL_PROMPT", "0")
        .arg("--git-dir")
        .arg(&mirror_dir)
        .args(["cat-file", "-e", &commit_spec]);
    let has_commit = run_cmd_with_timeout(check_cmd, 30).await?.success;

    if !has_commit {
        debug!(repo = %repo_url, commit = %parent_commit, "fetching updates into git mirror");
        let mut fetch_all = Command::new("git");
        fetch_all
            .env("GIT_TERMINAL_PROMPT", "0")
            .arg("--git-dir")
            .arg(&mirror_dir)
            .args(["fetch", "--all", "--prune"]);
        let _ = run_cmd_with_timeout(fetch_all, setup_timeout_secs).await;

        let mut recheck = Command::new("git");
        recheck
            .env("GIT_TERMINAL_PROMPT", "0")
            .arg("--git-dir")
            .arg(&mirror_dir)
            .args(["cat-file", "-e", &commit_spec]);
        if !run_cmd_with_timeout(recheck, 30).await?.success {
            let mut fetch_sha = Command::new("git");
            fetch_sha
                .env("GIT_TERMINAL_PROMPT", "0")
                .arg("--git-dir")
                .arg(&mirror_dir)
                .args(["fetch", "origin", parent_commit]);
            let fetch_res = run_cmd_with_timeout(fetch_sha, setup_timeout_secs).await?;
            if !fetch_res.success {
                return Err(anyhow!(
                    "commit {} not found in mirror {} even after fetch: {}",
                    parent_commit,
                    mirror_dir.display(),
                    truncate_tail(&fetch_res.combined_output, 250)
                ));
            }
        }
    }

    let mut wt_cmd = Command::new("git");
    wt_cmd
        .env("GIT_TERMINAL_PROMPT", "0")
        .arg("--git-dir")
        .arg(&mirror_dir)
        .args(["worktree", "add", "--detach", "--force"])
        .arg(worktree_path)
        .arg(parent_commit);
    let wt_res = run_cmd_with_timeout(wt_cmd, 60).await?;
    if !wt_res.success {
        return Err(anyhow!(
            "git worktree add failed at {}: {}",
            parent_commit,
            truncate_tail(&wt_res.combined_output, 300)
        ));
    }

    Ok((mirror_dir, repo_key))
}

async fn cleanup_worktree(mirror_dir: &Path, worktree_path: &Path) {
    let repo_lock = get_named_lock(&REPO_LOCKS, &mirror_dir.to_string_lossy());
    let _guard = repo_lock.lock().await;

    let mut rm_cmd = Command::new("git");
    rm_cmd
        .env("GIT_TERMINAL_PROMPT", "0")
        .arg("--git-dir")
        .arg(mirror_dir)
        .args(["worktree", "remove", "--force"])
        .arg(worktree_path);
    let _ = run_cmd_with_timeout(rm_cmd, 30).await;

    let mut prune_cmd = Command::new("git");
    prune_cmd
        .env("GIT_TERMINAL_PROMPT", "0")
        .arg("--git-dir")
        .arg(mirror_dir)
        .args(["worktree", "prune"]);
    let _ = run_cmd_with_timeout(prune_cmd, 30).await;
}

/// Removes any editable-install hooks (`__editable__*`, `.egg-link`, or `.pth` files referencing
/// temporary worktrees or mirrors) inside a Python venv's `site-packages`, guaranteeing that
/// `PYTHONPATH=<worktree>/src:<worktree>` always resolves candidate edits from the active worktree.
pub fn strip_editable_hooks(venv_dir: &Path) -> Result<usize> {
    let mut removed = 0usize;
    let lib_dir = venv_dir.join("lib");
    if !lib_dir.exists() {
        return Ok(0);
    }

    let mut stack = vec![lib_dir];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if path.is_dir() {
                if name == "site-packages" || name.starts_with("python") {
                    stack.push(path);
                } else if name.starts_with("__editable__")
                    && std::fs::remove_dir_all(&path).is_ok()
                {
                    removed += 1;
                }
                continue;
            }

            let is_site_pkg_file = path
                .parent()
                .and_then(|p| p.file_name())
                .is_some_and(|n| n == "site-packages");
            if !is_site_pkg_file {
                continue;
            }

            let should_remove = if name.starts_with("__editable__") || name.ends_with(".egg-link") {
                true
            } else if name.ends_with(".pth") {
                let lower_name = name.to_ascii_lowercase();
                if lower_name.contains("editable") {
                    true
                } else if let Ok(content) = std::fs::read_to_string(&path) {
                    content.lines().any(|line| {
                        let l = line.trim();
                        l.contains("/tmp/")
                            || l.contains("/private/tmp/")
                            || l.contains("/var/folders/")
                            || l.contains("epicenter")
                            || l.contains("worktree")
                    })
                } else {
                    false
                }
            } else {
                false
            };

            if should_remove && std::fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }
    }

    Ok(removed)
}

async fn ensure_python_venv(
    cache_root: &Path,
    repo_key: &str,
    parent_commit: &str,
    worktree_path: &Path,
    setup_timeout_secs: u64,
) -> Result<(PathBuf, bool)> {
    let sha_prefix = &parent_commit[..parent_commit.len().min(12)];
    let venvs_dir = cache_root.join("venvs");
    std::fs::create_dir_all(&venvs_dir)
        .with_context(|| format!("failed to create venv cache dir {}", venvs_dir.display()))?;
    let venv_dir = venvs_dir.join(format!("{repo_key}_{sha_prefix}"));
    let venv_python = venv_dir.join("bin").join("python");
    let sentinel = venv_dir.join(".epicenter_ready");

    let env_lock = get_named_lock(&ENV_LOCKS, &venv_dir.to_string_lossy());
    let _guard = env_lock.lock().await;

    if !sentinel.exists() || !venv_python.exists() {
        info!(venv = %venv_dir.display(), "creating cached uv Python venv for host grading");
        let mut create_cmd = Command::new("uv");
        create_cmd.args(["venv", "--clear"]).arg(&venv_dir);
        let create_res = run_cmd_with_timeout(create_cmd, setup_timeout_secs).await;
        let created_with_uv = create_res.as_ref().is_ok_and(|r| r.success);

        if !created_with_uv {
            let mut fallback = Command::new("python3");
            fallback.args(["-m", "venv"]).arg(&venv_dir);
            let fb_res = run_cmd_with_timeout(fallback, setup_timeout_secs).await?;
            if !fb_res.success {
                return Err(anyhow!(
                    "failed to create Python venv with uv or python3: {}",
                    truncate_tail(&fb_res.combined_output, 250)
                ));
            }
        }

        // Install pytest + coverage (fallback to pytest alone if coverage fails).
        let mut install_pytest = Command::new("uv");
        install_pytest
            .args(["pip", "install", "--python"])
            .arg(&venv_python)
            .args(["pytest", "coverage"]);
        let pytest_res = run_cmd_with_timeout(install_pytest, setup_timeout_secs).await?;
        if !pytest_res.success {
            let mut install_pytest_only = Command::new("uv");
            install_pytest_only
                .args(["pip", "install", "--python"])
                .arg(&venv_python)
                .arg("pytest");
            let res2 = run_cmd_with_timeout(install_pytest_only, setup_timeout_secs).await?;
            if !res2.success {
                return Err(anyhow!(
                    "failed to install pytest into cached venv: {}",
                    truncate_tail(&res2.combined_output, 250)
                ));
            }
        }

        // Install project runtime/test dependencies if packaging metadata exists in the worktree.
        let has_pkg_meta = worktree_path.join("pyproject.toml").exists()
            || worktree_path.join("setup.py").exists()
            || worktree_path.join("setup.cfg").exists();
        if has_pkg_meta {
            let wt_str = worktree_path.to_string_lossy().to_string();
            let candidates = [
                format!("{wt_str}[test]"),
                format!("{wt_str}[dev]"),
                wt_str.clone(),
            ];
            for spec in &candidates {
                let mut dep_cmd = Command::new("uv");
                dep_cmd
                    .args(["pip", "install", "--python"])
                    .arg(&venv_python)
                    .args(["-e", spec]);
                if let Ok(r) = run_cmd_with_timeout(dep_cmd, setup_timeout_secs).await {
                    if r.success {
                        break;
                    }
                }
            }
        }

        for req_rel in [
            "requirements-test.txt",
            "requirements/tests.txt",
            "requirements-dev.txt",
            "test-requirements.txt",
        ] {
            let req_path = worktree_path.join(req_rel);
            if req_path.exists() {
                let mut req_cmd = Command::new("uv");
                req_cmd
                    .args(["pip", "install", "--python"])
                    .arg(&venv_python)
                    .arg("-r")
                    .arg(&req_path);
                let _ = run_cmd_with_timeout(req_cmd, setup_timeout_secs).await;
            }
        }

        let removed = strip_editable_hooks(&venv_dir)?;
        debug!(venv = %venv_dir.display(), removed, "stripped editable .pth hooks from cached venv");

        // Verify pytest is importable before writing ready sentinel.
        let mut verify_pytest = Command::new(&venv_python);
        verify_pytest.args(["-c", "import pytest"]);
        let vp_res = run_cmd_with_timeout(verify_pytest, 20).await?;
        if !vp_res.success {
            return Err(anyhow!(
                "pytest not importable in {}: {}",
                venv_dir.display(),
                truncate_tail(&vp_res.combined_output, 200)
            ));
        }

        std::fs::write(&sentinel, b"ready\n")?;
    } else {
        // Ensure no stale editable hooks linger across reused venvs.
        let _ = strip_editable_hooks(&venv_dir);
    }

    let mut check_cov = Command::new(&venv_python);
    check_cov.args(["-c", "import coverage"]);
    let has_coverage = run_cmd_with_timeout(check_cov, 15)
        .await
        .is_ok_and(|r| r.success);

    Ok((venv_python, has_coverage))
}

fn build_pythonpath(worktree_path: &Path) -> String {
    let src_dir = worktree_path.join("src");
    let mut parts = Vec::new();
    if src_dir.exists() {
        parts.push(src_dir.to_string_lossy().to_string());
    }
    parts.push(worktree_path.to_string_lossy().to_string());
    parts.join(":")
}

async fn ensure_node_modules_symlink(
    cache_root: &Path,
    repo_key: &str,
    parent_commit: &str,
    worktree_path: &Path,
    setup_timeout_secs: u64,
) -> Result<()> {
    if !worktree_path.join("package.json").exists() || worktree_path.join("node_modules").exists() {
        return Ok(());
    }

    let sha_prefix = &parent_commit[..parent_commit.len().min(12)];
    let node_envs_dir = cache_root.join("node_envs");
    std::fs::create_dir_all(&node_envs_dir)?;
    let env_dir = node_envs_dir.join(format!("{repo_key}_{sha_prefix}"));
    let cached_node_modules = env_dir.join("node_modules");
    let sentinel = env_dir.join(".epicenter_ready");

    let env_lock = get_named_lock(&ENV_LOCKS, &env_dir.to_string_lossy());
    let _guard = env_lock.lock().await;

    if !sentinel.exists() || !cached_node_modules.exists() {
        std::fs::create_dir_all(&env_dir)?;
        for manifest in [
            "package.json",
            "package-lock.json",
            "pnpm-lock.yaml",
            "pnpm-workspace.yaml",
            "yarn.lock",
            ".npmrc",
        ] {
            let src = worktree_path.join(manifest);
            if src.exists() {
                let _ = std::fs::copy(&src, env_dir.join(manifest));
            }
        }

        let install_res = if env_dir.join("pnpm-lock.yaml").exists() {
            let mut c = Command::new("pnpm");
            c.current_dir(&env_dir)
                .args(["install", "--ignore-scripts", "--no-frozen-lockfile"]);
            run_cmd_with_timeout(c, setup_timeout_secs).await
        } else if env_dir.join("yarn.lock").exists() {
            let mut c = Command::new("yarn");
            c.current_dir(&env_dir)
                .args(["install", "--ignore-scripts"]);
            run_cmd_with_timeout(c, setup_timeout_secs).await
        } else {
            let mut c = Command::new("npm");
            c.current_dir(&env_dir)
                .args(["install", "--ignore-scripts", "--no-audit", "--no-fund"]);
            run_cmd_with_timeout(c, setup_timeout_secs).await
        };

        if let Ok(res) = install_res {
            if res.success && cached_node_modules.exists() {
                let _ = std::fs::write(&sentinel, b"ready\n");
            } else {
                warn!(
                    env = %env_dir.display(),
                    output = %truncate_tail(&res.combined_output, 200),
                    "node dependency install did not succeed cleanly"
                );
            }
        }
    }

    if cached_node_modules.exists() && !worktree_path.join("node_modules").exists() {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&cached_node_modules, worktree_path.join("node_modules"))
                .context("failed to symlink cached node_modules into temp worktree")?;
        }
    }

    Ok(())
}

/// Collects existing pre-GT regression test files (existing versions of GT test files plus up to 4
/// sibling test files in the same directory and any candidate-added tests) so we can verify
/// `regression_free` strictly via real OS exit codes.
fn discover_regression_test_files(
    worktree_path: &Path,
    gt_test_files: &[String],
    candidate_files: &[FileDiff],
    lang: RepoLanguage,
) -> Vec<String> {
    let mut selected: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    let mut push_if_exists = |rel: &str| {
        let norm = normalize_rel_path(rel);
        if worktree_path.join(&norm).is_file() && seen.insert(norm.clone()) {
            selected.push(norm);
        }
    };

    // 1. Existing pre-patch versions of the ground-truth test files (tested BEFORE GT patch is applied).
    for gt in gt_test_files {
        push_if_exists(gt);
    }

    // 2. Any new/modified test files included in the candidate's own diff.
    for f in candidate_files {
        if is_test_file_path(&f.path) {
            push_if_exists(&f.path);
        }
    }

    // 3. Up to 4 sibling test files in the same directory as each GT test file.
    let mut sibling_count = 0usize;
    for gt in gt_test_files {
        if sibling_count >= 4 {
            break;
        }
        let parent_rel = Path::new(gt).parent().unwrap_or_else(|| Path::new(""));
        let parent_abs = worktree_path.join(parent_rel);
        let Ok(entries) = std::fs::read_dir(&parent_abs) else {
            continue;
        };
        let mut names: Vec<String> = entries
            .flatten()
            .filter_map(|e| {
                let p = e.path();
                if !p.is_file() {
                    return None;
                }
                let rel = p.strip_prefix(worktree_path).ok()?;
                let rel_str = rel.to_string_lossy().replace('\\', "/");
                let matches_lang = match lang {
                    RepoLanguage::Python => rel_str.ends_with(".py"),
                    RepoLanguage::TypeScriptJs => {
                        rel_str.ends_with(".ts")
                            || rel_str.ends_with(".tsx")
                            || rel_str.ends_with(".js")
                            || rel_str.ends_with(".jsx")
                    }
                    _ => true,
                };
                if matches_lang && is_test_file_path(&rel_str) {
                    Some(rel_str)
                } else {
                    None
                }
            })
            .collect();
        names.sort();
        for rel in names {
            if sibling_count >= 4 {
                break;
            }
            if seen.insert(rel.clone()) {
                selected.push(rel);
                sibling_count += 1;
            }
        }
    }

    selected
}

async fn run_language_tests(
    lang: RepoLanguage,
    worktree_path: &Path,
    test_files: &[String],
    python_bin: Option<&Path>,
    coverage_data_file: Option<&Path>,
    timeout_secs: u64,
) -> Result<CommandExecResult> {
    match lang {
        RepoLanguage::Python => {
            let py = python_bin.ok_or_else(|| anyhow!("Python interpreter not initialized"))?;
            let pythonpath = build_pythonpath(worktree_path);
            let mut cmd = Command::new(py);
            cmd.current_dir(worktree_path)
                .env("PYTHONPATH", pythonpath)
                .env("PYTHONDONTWRITEBYTECODE", "1");

            if let Some(cov_file) = coverage_data_file {
                cmd.args(["-m", "coverage", "run", "--data-file"])
                    .arg(cov_file)
                    .args(["-m", "pytest", "-q"]);
            } else {
                cmd.args(["-m", "pytest", "-q"]);
            }
            for f in test_files {
                cmd.arg(f);
            }
            run_cmd_with_timeout(cmd, timeout_secs).await
        }
        RepoLanguage::TypeScriptJs => {
            let pkg_text = std::fs::read_to_string(worktree_path.join("package.json"))
                .unwrap_or_default()
                .to_ascii_lowercase();
            let has_vitest_bin = worktree_path.join("node_modules/.bin/vitest").exists();
            let has_jest_bin = worktree_path.join("node_modules/.bin/jest").exists();

            let mut cmd = if has_vitest_bin || pkg_text.contains("\"vitest\"") {
                let mut c = Command::new("npx");
                c.current_dir(worktree_path);
                if has_vitest_bin {
                    c.arg("--no-install");
                }
                c.args(["vitest", "run"]);
                for f in test_files {
                    c.arg(f);
                }
                c
            } else if has_jest_bin || pkg_text.contains("\"jest\"") {
                let mut c = Command::new("npx");
                c.current_dir(worktree_path);
                if has_jest_bin {
                    c.arg("--no-install");
                }
                c.arg("jest");
                for f in test_files {
                    c.arg(f);
                }
                c
            } else {
                let mut c = Command::new("npm");
                c.current_dir(worktree_path).arg("test");
                if !test_files.is_empty() {
                    c.arg("--");
                    for f in test_files {
                        c.arg(f);
                    }
                }
                c
            };

            cmd.env("CI", "1");
            run_cmd_with_timeout(cmd, timeout_secs).await
        }
        RepoLanguage::Rust => {
            let mut cmd = Command::new("cargo");
            cmd.current_dir(worktree_path).arg("test");
            run_cmd_with_timeout(cmd, timeout_secs).await
        }
        RepoLanguage::Go => {
            let mut cmd = Command::new("go");
            cmd.current_dir(worktree_path).args(["test", "./..."]);
            run_cmd_with_timeout(cmd, timeout_secs).await
        }
        RepoLanguage::Unknown => Err(anyhow!(
            "unable to determine repository test runner language for {}",
            worktree_path.display()
        )),
    }
}

/// Loads real source files from the checked-out repository worktree for `RepoCallGraph::from_sources`.
pub fn load_real_repo_sources(
    worktree_path: &Path,
    candidate_files: &[FileDiff],
) -> BTreeMap<String, String> {
    const MAX_FILES: usize = 800;
    const MAX_FILE_BYTES: u64 = 256 * 1024;
    const SKIP_DIRS: &[&str] = &[
        ".git",
        "node_modules",
        ".venv",
        "venv",
        "__pycache__",
        "target",
        "dist",
        "build",
        ".tox",
        ".mypy_cache",
        ".pytest_cache",
        ".ruff_cache",
        ".next",
        "coverage",
    ];

    let mut sources = BTreeMap::new();

    // Always load files directly touched by the candidate diff first.
    for diff_file in candidate_files {
        let rel = normalize_rel_path(&diff_file.path);
        let abs = worktree_path.join(&rel);
        if abs.is_file() {
            if let Ok(meta) = std::fs::metadata(&abs) {
                if meta.len() <= MAX_FILE_BYTES {
                    if let Ok(content) = std::fs::read_to_string(&abs) {
                        sources.insert(rel, content);
                    }
                }
            }
        }
    }

    let mut stack = vec![worktree_path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if sources.len() >= MAX_FILES {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        paths.sort();

        for path in paths {
            if sources.len() >= MAX_FILES {
                break;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if path.is_dir() {
                if !SKIP_DIRS.contains(&name) {
                    stack.push(path);
                }
                continue;
            }
            if !is_graphable_source_file(name) {
                continue;
            }
            let Ok(rel_path) = path.strip_prefix(worktree_path) else {
                continue;
            };
            let rel_str = normalize_rel_path(&rel_path.to_string_lossy().replace('\\', "/"));
            if sources.contains_key(&rel_str) || is_test_file_path(&rel_str) {
                continue;
            }
            if let Ok(meta) = std::fs::metadata(&path) {
                if meta.len() <= MAX_FILE_BYTES {
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        sources.insert(rel_str, content);
                    }
                }
            }
        }
    }

    sources
}

fn is_graphable_source_file(filename: &str) -> bool {
    let lower = filename.to_ascii_lowercase();
    lower.ends_with(".py")
        || lower.ends_with(".ts")
        || lower.ends_with(".tsx")
        || lower.ends_with(".js")
        || lower.ends_with(".jsx")
        || lower.ends_with(".rs")
        || lower.ends_with(".go")
}

async fn grade_in_host_sandbox(
    case: &EvalIssueCase,
    submission: &CandidateSubmission,
    parsed_candidate: &[FileDiff],
    config: &GraderConfig,
) -> Result<ArmOutcome> {
    let gt_test_files: Vec<String> = parse_unified_diff(&case.ground_truth_test_patch)
        .into_iter()
        .map(|f| normalize_rel_path(&f.path))
        .collect();
    if gt_test_files.is_empty() {
        return Err(anyhow!(
            "ground_truth_test_patch for case '{}' has no parseable test files",
            case.id
        ));
    }

    let temp_root = tempfile::Builder::new()
        .prefix("epicenter-grade-")
        .tempdir()
        .context("failed to create temporary grading directory")?;
    let worktree_path = temp_root.path().join("repo");

    let (mirror_dir, repo_key) = checkout_isolated_worktree(
        &case.repo_url,
        &case.parent_commit,
        &config.cache_root,
        &worktree_path,
        config.setup_timeout_secs,
    )
    .await?;

    // Guarantee worktree metadata cleanup on every return path.
    let result = grade_inside_worktree(
        case,
        submission,
        parsed_candidate,
        &gt_test_files,
        temp_root.path(),
        &worktree_path,
        &repo_key,
        config,
    )
    .await;

    cleanup_worktree(&mirror_dir, &worktree_path).await;
    result
}

#[allow(clippy::too_many_arguments)]
async fn grade_inside_worktree(
    case: &EvalIssueCase,
    submission: &CandidateSubmission,
    parsed_candidate: &[FileDiff],
    gt_test_files: &[String],
    temp_root: &Path,
    worktree_path: &Path,
    repo_key: &str,
    config: &GraderConfig,
) -> Result<ArmOutcome> {
    // 1. Apply candidate patch with real `git apply`.
    let candidate_patch_path = temp_root.join("candidate.patch");
    std::fs::write(&candidate_patch_path, &submission.git_diff)
        .context("failed to write candidate patch file")?;

    let mut apply_cand = Command::new("git");
    apply_cand
        .current_dir(worktree_path)
        .args(["apply", "--whitespace=nowarn"])
        .arg(&candidate_patch_path);
    let cand_apply_res = run_cmd_with_timeout(apply_cand, 30).await?;
    if !cand_apply_res.success {
        return Ok(ArmOutcome {
            case_id: case.id.clone(),
            arm: submission.arm,
            resolved_ground_truth: false,
            regression_free: false,
            speedup_pct: submission.reported_speedup_pct,
            blast_risk_score: None,
            root_cause_depth_score: submission
                .reported_blast_radius
                .as_ref()
                .map(|b| b.root_cause_depth_score),
            dark_zone_touches: None,
            duration_ms: submission.duration_ms,
            notes: Some(format!(
                "git apply failed for candidate diff: {}",
                truncate_tail(&cand_apply_res.combined_output, 240)
            )),
            failure_category: Some("patch_apply_conflict".to_string()),
        });
    }

    // 2. Detect language cleanly from checked-out repository & test files.
    let lang = detect_repo_language(worktree_path, gt_test_files, parsed_candidate);
    info!(
        case_id = %case.id,
        arm = ?submission.arm,
        language = ?lang,
        "running independent host sandbox grading"
    );

    // 3. Prepare language runtime environment (cached uv venv for Python, cached node_modules for TS/JS).
    let (python_bin, has_coverage) = match lang {
        RepoLanguage::Python => {
            let (py, cov) = ensure_python_venv(
                &config.cache_root,
                repo_key,
                &case.parent_commit,
                worktree_path,
                config.setup_timeout_secs,
            )
            .await?;
            (Some(py), cov)
        }
        RepoLanguage::TypeScriptJs => {
            ensure_node_modules_symlink(
                &config.cache_root,
                repo_key,
                &case.parent_commit,
                worktree_path,
                config.setup_timeout_secs,
            )
            .await?;
            (None, false)
        }
        _ => (None, false),
    };

    // 4. Run regression check on existing pre-GT tests (after candidate patch, before hidden GT patch).
    let regression_files = if config.run_regression_suite {
        discover_regression_test_files(worktree_path, gt_test_files, parsed_candidate, lang)
    } else {
        Vec::new()
    };

    let mut reg_res_opt: Option<CommandExecResult> = None;
    if !regression_files.is_empty() {
        let reg_res = run_language_tests(
            lang,
            worktree_path,
            &regression_files,
            python_bin.as_deref(),
            None,
            config.test_timeout_secs,
        )
        .await?;
        reg_res_opt = Some(reg_res);
    }

    // 5. Apply hidden ground-truth test patch with real `git apply`.
    let gt_patch_path = temp_root.join("ground_truth.patch");
    std::fs::write(&gt_patch_path, &case.ground_truth_test_patch)
        .context("failed to write ground-truth test patch file")?;

    let mut apply_gt = Command::new("git");
    apply_gt
        .current_dir(worktree_path)
        .args(["apply", "--whitespace=nowarn"])
        .arg(&gt_patch_path);
    let gt_apply_res = run_cmd_with_timeout(apply_gt, 30).await?;
    if !gt_apply_res.success {
        return Ok(ArmOutcome {
            case_id: case.id.clone(),
            arm: submission.arm,
            resolved_ground_truth: false,
            regression_free: false,
            speedup_pct: submission.reported_speedup_pct,
            blast_risk_score: None,
            root_cause_depth_score: submission
                .reported_blast_radius
                .as_ref()
                .map(|b| b.root_cause_depth_score),
            dark_zone_touches: None,
            duration_ms: submission.duration_ms,
            notes: Some(format!(
                "git apply failed for hidden ground-truth test patch: {}",
                truncate_tail(&gt_apply_res.combined_output, 240)
            )),
            failure_category: Some("ground_truth_patch_apply_error".to_string()),
        });
    }

    // 6. Execute hidden ground-truth tests with real OS exit code (60s timeout).
    let cov_data_path = if lang == RepoLanguage::Python && has_coverage {
        Some(temp_root.join(".coverage"))
    } else {
        None
    };

    let gt_res = run_language_tests(
        lang,
        worktree_path,
        gt_test_files,
        python_bin.as_deref(),
        cov_data_path.as_deref(),
        config.test_timeout_secs,
    )
    .await?;

    // Export real coverage.json if Python coverage ran.
    let mut coverage_map: Option<CoverageMap> = None;
    if let (Some(py), Some(cov_data)) = (python_bin.as_deref(), cov_data_path.as_deref()) {
        if cov_data.exists() {
            let cov_json_path = temp_root.join("coverage.json");
            let mut cov_json_cmd = Command::new(py);
            cov_json_cmd
                .current_dir(worktree_path)
                .args(["-m", "coverage", "json", "--data-file"])
                .arg(cov_data)
                .arg("-o")
                .arg(&cov_json_path);
            if run_cmd_with_timeout(cov_json_cmd, 15)
                .await
                .is_ok_and(|r| r.success)
            {
                if let Ok(raw_cov) = std::fs::read_to_string(&cov_json_path) {
                    coverage_map = CoverageMap::from_coverage_json(&raw_cov).ok();
                }
            }
        }
    }
    if coverage_map.is_none() {
        let lcov_path = worktree_path.join("coverage").join("lcov.info");
        if let Ok(raw_lcov) = std::fs::read_to_string(&lcov_path) {
            coverage_map = Some(CoverageMap::from_lcov(&raw_lcov));
        }
    }

    // 7. Compute real BlastRadiusReport from real checked-out repository files + real coverage.
    let real_sources = load_real_repo_sources(worktree_path, parsed_candidate);
    let blast_report = if !real_sources.is_empty() {
        let graph = RepoCallGraph::from_sources(&real_sources);
        Some(graph.analyze_diff(
            &submission.git_diff,
            coverage_map.as_ref(),
            &BlastAnalysisConfig::default(),
        ))
    } else {
        submission.reported_blast_radius.clone()
    };

    let dark_zones = blast_report.as_ref().map(BlastRadiusReport::dark_zone_count);
    let blast_risk = blast_report.as_ref().map(|b| b.composite_risk_score);
    let root_cause_depth = submission
        .reported_blast_radius
        .as_ref()
        .map(|b| b.root_cause_depth_score);

    // Strictly derive resolved_ground_truth and regression_free from real OS exit codes.
    let resolved_ground_truth = gt_res.success && !gt_res.timed_out;
    let (regression_free, reg_timed_out, reg_tail) = match &reg_res_opt {
        Some(r) => (r.success && !r.timed_out, r.timed_out, r.combined_output.as_str()),
        None => (resolved_ground_truth, false, ""),
    };

    let timed_out = gt_res.timed_out || reg_timed_out;
    let failure_category = classify_failure_category(
        true,
        resolved_ground_truth,
        regression_free,
        timed_out,
        dark_zones.unwrap_or(0),
    );

    let diagnostic_note = if resolved_ground_truth && regression_free {
        format!(
            "HostSandbox PASS (lang={:?}, gt_exit={:?}, sources={}, cov={})",
            lang,
            gt_res.exit_code,
            real_sources.len(),
            coverage_map.is_some()
        )
    } else if !regression_free && !reg_tail.is_empty() {
        format!(
            "HostSandbox regression FAIL (reg_exit={:?}): {}",
            reg_res_opt.as_ref().and_then(|r| r.exit_code),
            truncate_tail(reg_tail, 240)
        )
    } else {
        format!(
            "HostSandbox GT FAIL (gt_exit={:?}): {}",
            gt_res.exit_code,
            truncate_tail(&gt_res.combined_output, 240)
        )
    };

    Ok(ArmOutcome {
        case_id: case.id.clone(),
        arm: submission.arm,
        resolved_ground_truth,
        regression_free,
        speedup_pct: submission.reported_speedup_pct,
        blast_risk_score: blast_risk,
        root_cause_depth_score: root_cause_depth,
        dark_zone_touches: dark_zones,
        duration_ms: submission.duration_ms,
        notes: Some(diagnostic_note),
        failure_category,
    })
}

async fn grade_in_remote_sandbox(
    case: &EvalIssueCase,
    submission: &CandidateSubmission,
    gemini: &GeminiClient,
    config: &GraderConfig,
) -> Result<ArmOutcome> {
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
         5. Collect modified & direct-caller source files into `touched_sources` (relative_path -> content)\n\
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
        Some(graph.analyze_diff(
            &submission.git_diff,
            cov.as_ref(),
            &BlastAnalysisConfig::default(),
        ))
    } else {
        submission.reported_blast_radius.clone()
    };

    let dark_zones = blast_report.as_ref().map(BlastRadiusReport::dark_zone_count);
    let blast_risk = blast_report.as_ref().map(|b| b.composite_risk_score);
    let root_cause_depth = submission
        .reported_blast_radius
        .as_ref()
        .map(|b| b.root_cause_depth_score);

    let resolved = verdict.patch_applied && verdict.resolved_ground_truth;
    let reg_free = verdict.patch_applied && verdict.regression_free;
    let failure_category = classify_failure_category(
        verdict.patch_applied,
        resolved,
        reg_free,
        false,
        dark_zones.unwrap_or(0),
    );

    Ok(ArmOutcome {
        case_id: case.id.clone(),
        arm: submission.arm,
        resolved_ground_truth: resolved,
        regression_free: reg_free,
        speedup_pct: verdict.speedup_pct.or(submission.reported_speedup_pct),
        blast_risk_score: blast_risk,
        root_cause_depth_score: root_cause_depth,
        dark_zone_touches: dark_zones,
        duration_ms: submission.duration_ms,
        notes: Some(truncate_tail(&verdict.test_output_tail, 280)),
        failure_category,
    })
}

/// Classifies unresolved or regressing outcomes strictly from observed execution outcomes.
pub fn classify_failure_category(
    patch_applied: bool,
    resolved_ground_truth: bool,
    regression_free: bool,
    timed_out: bool,
    dark_zone_touches: usize,
) -> Option<String> {
    if resolved_ground_truth && regression_free {
        return None;
    }
    if !patch_applied {
        return Some("patch_apply_conflict".to_string());
    }
    if timed_out {
        return Some("test_execution_timeout".to_string());
    }
    if !regression_free {
        if dark_zone_touches > 0 {
            return Some("regression_in_caller_dark_zone".to_string());
        }
        return Some("regression_in_existing_suite".to_string());
    }
    Some("ground_truth_test_failed".to_string())
}

fn truncate_tail(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let collected: String = s.chars().rev().take(max_chars).collect();
    format!("...{}", collected.chars().rev().collect::<String>())
}
