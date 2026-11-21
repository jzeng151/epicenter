use crate::repo::{
    emit_event, file_priority_score, is_ignored_path, is_source_file, is_test_like_path,
    normalize_clone_url, parse_github_repo_url, truncate_str, RepoSnapshot, MAX_FILE_BYTES,
};
use epicenter_common::{BenchmarkHarnessSpec, LiveEvent};
use epicenter_graph::{normalize_rel_path, parse_unified_diff, CoverageMap, FileDiff};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tempfile::TempDir;
use tokio::process::Command;
use tokio::sync::broadcast;
use tracing::{debug, warn};
use uuid::Uuid;
use walkdir::WalkDir;

const DEFAULT_CACHE_DIR: &str = "/tmp/epicenter-cache";
const MAX_LOCAL_SNAPSHOT_FILES: usize = 160;
const TEST_CMD_TIMEOUT: Duration = Duration::from_secs(45);
const GROUND_TRUTH_TIMEOUT: Duration = Duration::from_secs(60);
const BENCH_TRIAL_TIMEOUT: Duration = Duration::from_secs(20);
const COVERAGE_TIMEOUT: Duration = Duration::from_secs(30);
const ENV_SETUP_TIMEOUT: Duration = Duration::from_secs(120);
const GIT_FETCH_TIMEOUT: Duration = Duration::from_secs(60);

/// Detected repository language / test runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepoRuntime {
    Python,
    NodeTs,
    Rust,
    Go,
    Unknown,
}

impl RepoRuntime {
    /// Inspects repository root manifests (and top-level file extensions as fallback) to identify
    /// the primary runtime. Never misclassifies Node/TS, Rust, or Go repositories as Python.
    pub fn detect(repo_dir: &Path) -> Self {
        if repo_dir.join("pyproject.toml").exists()
            || repo_dir.join("setup.py").exists()
            || repo_dir.join("setup.cfg").exists()
        {
            return Self::Python;
        }
        if repo_dir.join("package.json").exists() {
            return Self::NodeTs;
        }
        if repo_dir.join("Cargo.toml").exists() {
            return Self::Rust;
        }
        if repo_dir.join("go.mod").exists() {
            return Self::Go;
        }
        if repo_dir.join("requirements.txt").exists() {
            return Self::Python;
        }

        // Fallback: inspect shallow source file extensions (useful for minimal repos without manifests)
        let mut py = 0usize;
        let mut js_ts = 0usize;
        let mut rs = 0usize;
        let mut go = 0usize;
        for entry in WalkDir::new(repo_dir)
            .max_depth(3)
            .into_iter()
            .filter_map(Result::ok)
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let Some(ext) = entry.path().extension().and_then(|e| e.to_str()) else {
                continue;
            };
            match ext.to_ascii_lowercase().as_str() {
                "py" => py += 1,
                "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => js_ts += 1,
                "rs" => rs += 1,
                "go" => go += 1,
                _ => {}
            }
        }
        let max_count = py.max(js_ts).max(rs).max(go);
        if max_count == 0 {
            Self::Unknown
        } else if py == max_count {
            Self::Python
        } else if js_ts == max_count {
            Self::NodeTs
        } else if rs == max_count {
            Self::Rust
        } else {
            Self::Go
        }
    }

    pub const fn label(&self) -> &'static str {
        match self {
            Self::Python => "Python",
            Self::NodeTs => "Node/TypeScript",
            Self::Rust => "Rust",
            Self::Go => "Go",
            Self::Unknown => "Unknown",
        }
    }
}

/// Detected Node/TypeScript test runner parsed from `package.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NodeTestRunner {
    Vitest,
    Jest,
    NodeTest,
    #[default]
    NpmTest,
}

impl NodeTestRunner {
    pub fn detect(repo_dir: &Path) -> Self {
        let pkg_path = repo_dir.join("package.json");
        let Ok(raw) = std::fs::read_to_string(pkg_path) else {
            return Self::NpmTest;
        };
        let lower = raw.to_ascii_lowercase();
        if lower.contains("\"vitest\"") {
            Self::Vitest
        } else if lower.contains("\"jest\"") {
            Self::Jest
        } else if lower.contains("node --test") {
            Self::NodeTest
        } else {
            Self::NpmTest
        }
    }
}

/// Structured search-and-replace edit emitted by Stage 4 workers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEdit {
    pub path: String,
    pub old_text: String,
    pub new_text: String,
}

/// New file payload emitted by Stage 4 workers (e.g. caller regression tests).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewFileSpec {
    pub path: String,
    pub content: String,
}

/// Result of applying structured worker edits inside a clean worktree to synthesize a canonical diff.
#[derive(Debug, Clone)]
pub struct EditApplicationResult {
    pub git_diff: String,
    pub tampered: bool,
    pub errors: Vec<String>,
}

/// Captured OS subprocess execution telemetry.
#[derive(Debug, Clone)]
pub struct CommandOutput {
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub stdout: String,
    pub stderr: String,
    pub combined_output: String,
    pub elapsed_ms: f64,
}

/// Independent host verification verdict for Stage 2 Bug Red-Gate.
#[derive(Debug, Clone)]
pub struct ReproVerificationResult {
    pub patch_applied: bool,
    pub touches_only_repro_file: bool,
    pub verified_fails_on_head: bool,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub is_broken_test_or_env_error: bool,
    pub command_used: String,
    pub output_tail: String,
    pub diagnostic: String,
}

/// Independent host verification verdict for Stage 4 candidate patches.
#[derive(Debug, Clone)]
pub struct CandidateVerificationResult {
    pub tampered: bool,
    pub patch_applied: bool,
    pub repro_patch_applied: bool,
    pub repro_passed: bool,
    pub regression_suite_passed: bool,
    pub repro_command: String,
    pub regression_command: String,
    pub test_output_tail: String,
    pub failure_reason: Option<String>,
}

/// Candidate input passed to Stage 5 single-host interleaved benchmark arena.
#[derive(Debug, Clone)]
pub struct ArenaCandidateInput {
    pub strategy_id: String,
    pub git_diff: String,
}

/// Per-candidate real wall-clock timings and coverage captured in Stage 5.
#[derive(Debug, Clone)]
pub struct HostArenaCandidateResult {
    pub strategy_id: String,
    pub baseline_samples_ms: Vec<f64>,
    pub candidate_samples_ms: Vec<f64>,
    pub peak_memory_kb: Option<u64>,
    pub coverage_map: Option<CoverageMap>,
}

/// Aggregate result of Stage 5 single-host interleaved benchmark arena.
#[derive(Debug, Clone)]
pub struct HostArenaResult {
    pub baseline_samples_ms: Vec<f64>,
    pub candidates: Vec<HostArenaCandidateResult>,
    pub trials_run: u32,
    pub benchmark_used: String,
    pub summary_notes: String,
}

/// Independent ground-truth grading result exposed for `epicenter-eval` & `epicenter-server`.
#[derive(Debug, Clone)]
pub struct GroundTruthGradeResult {
    pub patch_applied: bool,
    pub tampered_with_ground_truth: bool,
    pub ground_truth_patch_applied: bool,
    pub resolved_ground_truth: bool,
    pub regression_free: bool,
    pub ground_truth_command: String,
    pub regression_command: String,
    pub test_output_tail: String,
    pub coverage_map: Option<CoverageMap>,
    pub failure_category: Option<String>,
}

/// Cached runtime environment attached to a `PreparedRepo`.
#[derive(Debug, Clone, Default)]
pub struct PreparedEnv {
    pub python_venv_dir: Option<PathBuf>,
    pub node_env_dir: Option<PathBuf>,
    pub cargo_target_dir: Option<PathBuf>,
    pub notes: Vec<String>,
}

/// An isolated temporary git worktree checked out at a pinned commit.
/// Dropping this handle removes the temporary directory from disk; stale git worktree
/// admin references are pruned safely under the repo lock on subsequent checkouts.
pub struct IsolatedWorktree {
    _temp_dir: TempDir,
    repo_dir: PathBuf,
}

impl IsolatedWorktree {
    pub fn path(&self) -> &Path {
        &self.repo_dir
    }
}

/// Local bare mirror + pinned commit + cached isolated test environment.
#[derive(Debug, Clone)]
pub struct PreparedRepo {
    pub repo_url: String,
    pub repo_key: String,
    pub owner: String,
    pub repo_name: String,
    pub mirror_dir: PathBuf,
    pub resolved_sha: String,
    pub runtime: RepoRuntime,
    pub node_runner: NodeTestRunner,
    pub cache_root: PathBuf,
    pub env: PreparedEnv,
    pub file_tree: Vec<String>,
}

/// Host-side deterministic git + test verification sandbox manager.
#[derive(Debug, Clone)]
pub struct HostVerifier {
    cache_root: PathBuf,
}

impl Default for HostVerifier {
    fn default() -> Self {
        let root = std::env::var("EPICENTER_CACHE_DIR")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_CACHE_DIR));
        Self { cache_root: root }
    }
}

impl HostVerifier {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_cache_root(cache_root: PathBuf) -> Self {
        Self { cache_root }
    }

    pub fn cache_root(&self) -> &Path {
        &self.cache_root
    }

    /// Prepares the cached bare git mirror, resolves the exact `commit_sha` / `git_ref`,
    /// detects runtime & test runner, and collects the full repository file tree — WITHOUT yet
    /// installing Python/Node dependencies so callers can overlap environment setup with Stage 1.
    pub async fn prepare_checkout(
        &self,
        repo_url: &str,
        commit_sha: Option<&str>,
        git_ref: Option<&str>,
    ) -> anyhow::Result<PreparedRepo> {
        let trimmed_url = repo_url.trim();
        if trimmed_url.is_empty() {
            anyhow::bail!("repository URL must not be empty");
        }

        let (owner, repo_name, repo_key) = derive_repo_identity(trimmed_url);
        let clone_url = resolve_git_clone_source(trimmed_url);

        let repos_dir = self.cache_root.join("repos");
        tokio::fs::create_dir_all(&repos_dir).await?;
        let mirror_dir = repos_dir.join(&repo_key);

        let lock = repo_lock(&repo_key);
        let resolved_sha = {
            let _guard = lock.lock().await;
            ensure_bare_mirror(&mirror_dir, &clone_url).await?;
            resolve_and_fetch_commit(&mirror_dir, commit_sha, git_ref).await?
        };

        // Create a brief inspection worktree to detect runtime, node runner, and file tree.
        let mut prepared = PreparedRepo {
            repo_url: trimmed_url.to_string(),
            repo_key,
            owner,
            repo_name,
            mirror_dir,
            resolved_sha,
            runtime: RepoRuntime::Unknown,
            node_runner: NodeTestRunner::NpmTest,
            cache_root: self.cache_root.clone(),
            env: PreparedEnv::default(),
            file_tree: Vec::new(),
        };

        let wt = prepared.checkout_worktree().await?;
        prepared.runtime = RepoRuntime::detect(wt.path());
        if prepared.runtime == RepoRuntime::NodeTs {
            prepared.node_runner = NodeTestRunner::detect(wt.path());
        }
        prepared.file_tree = collect_file_tree(wt.path());
        drop(wt);

        Ok(prepared)
    }

    /// Full preparation: `prepare_checkout` + cached isolated runtime environment setup.
    pub async fn prepare_repo(
        &self,
        repo_url: &str,
        commit_sha: Option<&str>,
        git_ref: Option<&str>,
    ) -> anyhow::Result<PreparedRepo> {
        let mut prepared = self
            .prepare_checkout(repo_url, commit_sha, git_ref)
            .await?;
        prepared.ensure_environment().await?;
        Ok(prepared)
    }
}

impl PreparedRepo {
    /// Builds (or reuses cached) isolated test environment for this repository & commit without
    /// mutating `&self`, allowing callers to run it concurrently inside `tokio::join!`.
    pub async fn build_environment(&self) -> anyhow::Result<PreparedEnv> {
        let commit_prefix = &self.resolved_sha[..self.resolved_sha.len().min(12)];
        let env_key = format!("{}_{}", self.repo_key, commit_prefix);
        let mut env = PreparedEnv::default();

        match self.runtime {
            RepoRuntime::Python => {
                let lock = env_lock(&format!("py:{env_key}"));
                let _guard = lock.lock().await;

                let venvs_dir = self.cache_root.join("venvs");
                tokio::fs::create_dir_all(&venvs_dir).await?;
                let venv_dir = venvs_dir.join(&env_key);
                let ready_marker = venv_dir.join(".epicenter_env_ready");
                let venv_python = venv_dir.join("bin").join("python");

                if ready_marker.exists() && venv_python.exists() {
                    env.python_venv_dir = Some(venv_dir);
                    env.notes.push("Reused cached Python uv venv".to_string());
                    return Ok(env);
                }

                if venv_dir.exists() {
                    let _ = tokio::fs::remove_dir_all(&venv_dir).await;
                }

                let uv_bin = find_uv_binary();
                if let Some(ref uv) = uv_bin {
                    let out = run_program(
                        &venvs_dir,
                        uv.to_string_lossy().as_ref(),
                        &["venv", "--quiet", venv_dir.to_string_lossy().as_ref()],
                        &[],
                        ENV_SETUP_TIMEOUT,
                    )
                    .await?;
                    if out.exit_code != Some(0) {
                        warn!(stderr = %out.stderr, "uv venv failed; falling back to python3 -m venv");
                        create_stdlib_venv(&venv_dir).await?;
                    }
                } else {
                    create_stdlib_venv(&venv_dir).await?;
                }

                // Install pytest + coverage into the venv first.
                let py_str = venv_python.to_string_lossy().to_string();
                let base_install = if let Some(ref uv) = uv_bin {
                    run_program(
                        &venvs_dir,
                        uv.to_string_lossy().as_ref(),
                        &[
                            "pip", "install", "--quiet", "--python", &py_str, "pytest", "coverage",
                        ],
                        &[],
                        ENV_SETUP_TIMEOUT,
                    )
                    .await?
                } else {
                    run_program(
                        &venvs_dir,
                        &py_str,
                        &["-m", "pip", "install", "-q", "pytest", "coverage"],
                        &[],
                        ENV_SETUP_TIMEOUT,
                    )
                    .await?
                };
                if base_install.exit_code != Some(0) {
                    anyhow::bail!(
                        "failed to install pytest/coverage into venv: {}",
                        tail_chars(&base_install.combined_output, 500)
                    );
                }

                // Inspect a clean checkout to install repository dependencies, then STRIP all
                // editable links/pth files so Python NEVER imports from the mirror/setup checkout!
                let wt = self.checkout_worktree().await?;
                let local_pkgs = discover_local_python_packages(wt.path());

                if wt.path().join("pyproject.toml").exists()
                    || wt.path().join("setup.py").exists()
                    || wt.path().join("setup.cfg").exists()
                {
                    let wt_str = wt.path().to_string_lossy().to_string();
                    let install_res = if let Some(ref uv) = uv_bin {
                        run_program(
                            wt.path(),
                            uv.to_string_lossy().as_ref(),
                            &["pip", "install", "--quiet", "--python", &py_str, "-e", &wt_str],
                            &[],
                            ENV_SETUP_TIMEOUT,
                        )
                        .await
                    } else {
                        run_program(
                            wt.path(),
                            &py_str,
                            &["-m", "pip", "install", "-q", "-e", &wt_str],
                            &[],
                            ENV_SETUP_TIMEOUT,
                        )
                        .await
                    };
                    match install_res {
                        Ok(o) if o.exit_code == Some(0) => {
                            env.notes.push("Installed project deps via editable install".to_string());
                        }
                        Ok(o) => {
                            let msg = format!(
                                "Editable project install non-zero ({:?}): {}",
                                o.exit_code,
                                tail_chars(&o.combined_output, 240)
                            );
                            warn!("{msg}");
                            env.notes.push(msg);
                        }
                        Err(e) => {
                            warn!(error = %e, "editable project install error");
                            env.notes.push(format!("Editable project install error: {e}"));
                        }
                    }
                }

                for req_rel in [
                    "requirements.txt",
                    "requirements-dev.txt",
                    "requirements/tests.txt",
                    "requirements/dev.txt",
                ] {
                    let req_path = wt.path().join(req_rel);
                    if req_path.exists() {
                        let req_str = req_path.to_string_lossy().to_string();
                        let _ = if let Some(ref uv) = uv_bin {
                            run_program(
                                wt.path(),
                                uv.to_string_lossy().as_ref(),
                                &[
                                    "pip", "install", "--quiet", "--python", &py_str, "-r", &req_str,
                                ],
                                &[],
                                ENV_SETUP_TIMEOUT,
                            )
                            .await
                        } else {
                            run_program(
                                wt.path(),
                                &py_str,
                                &["-m", "pip", "install", "-q", "-r", &req_str],
                                &[],
                                ENV_SETUP_TIMEOUT,
                            )
                            .await
                        };
                    }
                }

                // CRITICAL: Remove ALL editable links / .pth files pointing at setup checkout and
                // any site-packages copies of local packages so active worktree PYTHONPATH always wins!
                let removed = strip_editable_and_local_links(&venv_dir, wt.path(), &local_pkgs);
                debug!(removed, venv = %venv_dir.display(), "stripped editable links from cached Python venv");

                tokio::fs::write(&ready_marker, b"ready\n").await?;
                drop(wt);
                env.python_venv_dir = Some(venv_dir);
            }
            RepoRuntime::NodeTs => {
                let lock = env_lock(&format!("node:{env_key}"));
                let _guard = lock.lock().await;

                let node_envs_dir = self.cache_root.join("node_envs");
                tokio::fs::create_dir_all(&node_envs_dir).await?;
                let env_dir = node_envs_dir.join(&env_key);
                let ready_marker = env_dir.join(".epicenter_env_ready");

                if ready_marker.exists() && env_dir.join("node_modules").exists() {
                    env.node_env_dir = Some(env_dir);
                    env.notes.push("Reused cached Node node_modules".to_string());
                    return Ok(env);
                }

                let wt = self.checkout_worktree().await?;
                if has_npm_dependencies(wt.path()) {
                    tokio::fs::create_dir_all(&env_dir).await?;
                    for manifest in [
                        "package.json",
                        "package-lock.json",
                        "npm-shrinkwrap.json",
                        "yarn.lock",
                        "pnpm-lock.yaml",
                    ] {
                        let src = wt.path().join(manifest);
                        if src.exists() {
                            let _ = tokio::fs::copy(&src, env_dir.join(manifest)).await;
                        }
                    }

                    let install_cmd = if env_dir.join("package-lock.json").exists() {
                        "npm ci --ignore-scripts --no-audit --no-fund || npm install --ignore-scripts --no-audit --no-fund"
                    } else {
                        "npm install --ignore-scripts --no-audit --no-fund"
                    };
                    let out = run_shell_command(&env_dir, install_cmd, &[], ENV_SETUP_TIMEOUT).await?;
                    if out.exit_code == Some(0) && env_dir.join("node_modules").exists() {
                        tokio::fs::write(&ready_marker, b"ready\n").await?;
                        env.node_env_dir = Some(env_dir);
                        env.notes.push("Installed cached Node dependencies".to_string());
                    } else {
                        env.notes.push(format!(
                            "Node dependency install skipped/failed ({:?}): {}",
                            out.exit_code,
                            tail_chars(&out.combined_output, 240)
                        ));
                    }
                } else {
                    env.notes.push("Node repo has zero external dependencies".to_string());
                }
                drop(wt);
            }
            RepoRuntime::Rust => {
                let target_dir = self.cache_root.join("cargo_target").join(&self.repo_key);
                tokio::fs::create_dir_all(&target_dir).await?;
                env.cargo_target_dir = Some(target_dir);
            }
            RepoRuntime::Go | RepoRuntime::Unknown => {}
        }

        Ok(env)
    }

    pub fn set_env(&mut self, env: PreparedEnv) {
        self.env = env;
    }

    pub async fn ensure_environment(&mut self) -> anyhow::Result<()> {
        let env = self.build_environment().await?;
        self.env = env;
        Ok(())
    }

    /// Returns a language-appropriate test command. Never returns `pytest` for non-Python repos.
    pub fn default_test_command(&self, target_path: Option<&str>) -> String {
        let cleaned = target_path
            .map(normalize_rel_path)
            .filter(|p| !p.is_empty());
        match self.runtime {
            RepoRuntime::Python => match cleaned {
                Some(p) => format!("pytest -q {p}"),
                None => "pytest -q".to_string(),
            },
            RepoRuntime::NodeTs => match self.node_runner {
                NodeTestRunner::Vitest => match cleaned {
                    Some(p) => format!("npx vitest run {p}"),
                    None => "npx vitest run".to_string(),
                },
                NodeTestRunner::Jest => match cleaned {
                    Some(p) => format!("npx jest {p}"),
                    None => "npx jest".to_string(),
                },
                NodeTestRunner::NodeTest => match cleaned {
                    Some(p) => format!("node --test {p}"),
                    None => "node --test".to_string(),
                },
                NodeTestRunner::NpmTest => match cleaned {
                    Some(p) if p.ends_with(".mjs") || p.ends_with(".cjs") || p.ends_with(".js") => {
                        format!("node --test {p}")
                    }
                    _ => "npm test".to_string(),
                },
            },
            RepoRuntime::Rust => "cargo test".to_string(),
            RepoRuntime::Go => "go test ./...".to_string(),
            RepoRuntime::Unknown => "true".to_string(),
        }
    }

    /// Validates a model-proposed test command against the detected runtime so non-Python repos
    /// can never accidentally execute `pytest`.
    pub fn sanitize_test_command(&self, proposed: &str, target_path: Option<&str>) -> String {
        let trimmed = proposed.trim();
        if trimmed.is_empty() {
            return self.default_test_command(target_path);
        }
        let lower = trimmed.to_ascii_lowercase();
        let looks_python = lower.starts_with("pytest")
            || lower.starts_with("python -m pytest")
            || lower.starts_with("python3 -m pytest")
            || lower.starts_with("python -m unittest")
            || lower.starts_with("python3 -m unittest");
        if self.runtime != RepoRuntime::Python && looks_python {
            return self.default_test_command(target_path);
        }
        trimmed.to_string()
    }

    /// Creates a brand-new isolated temporary git worktree checked out at `self.resolved_sha`.
    pub async fn checkout_worktree(&self) -> anyhow::Result<IsolatedWorktree> {
        let temp_dir = tempfile::Builder::new()
            .prefix("epicenter-wt-")
            .tempdir()?;
        let repo_dir = temp_dir.path().join("repo");

        let lock = repo_lock(&self.repo_key);
        {
            let _guard = lock.lock().await;
            // Clean up any dropped temporary worktree administrative records safely under the lock.
            let _ = run_git(
                &self.mirror_dir,
                &["--git-dir", &path_str(&self.mirror_dir), "worktree", "prune"],
                Duration::from_secs(15),
            )
            .await;

            let out = run_git(
                &self.mirror_dir,
                &[
                    "--git-dir",
                    &path_str(&self.mirror_dir),
                    "worktree",
                    "add",
                    "--detach",
                    "-q",
                    &path_str(&repo_dir),
                    &self.resolved_sha,
                ],
                Duration::from_secs(30),
            )
            .await?;
            if out.exit_code != Some(0) {
                anyhow::bail!(
                    "git worktree add failed for {}@{}: {}",
                    self.repo_key,
                    self.resolved_sha,
                    tail_chars(&out.combined_output, 400)
                );
            }
        }

        // Link cached node_modules into the worktree when applicable.
        if self.runtime == RepoRuntime::NodeTs {
            if let Some(ref node_env) = self.env.node_env_dir {
                let cached_nm = node_env.join("node_modules");
                let wt_nm = repo_dir.join("node_modules");
                if cached_nm.exists() && !wt_nm.exists() {
                    #[cfg(unix)]
                    {
                        let _ = std::os::unix::fs::symlink(&cached_nm, &wt_nm);
                    }
                }
            }
        }

        Ok(IsolatedWorktree {
            _temp_dir: temp_dir,
            repo_dir,
        })
    }

    /// Loads the complete local file tree and up to ~160 prioritized source files directly from
    /// the local git checkout (eliminating slow GitHub raw HTTP fetches).
    pub async fn load_repo_snapshot(&self, priority_hints: &[String]) -> anyhow::Result<RepoSnapshot> {
        let wt = self.checkout_worktree().await?;
        let mut candidates: Vec<(i32, String, PathBuf)> = Vec::new();
        let mut file_tree = Vec::new();

        for entry in WalkDir::new(wt.path())
            .into_iter()
            .filter_entry(|e| {
                let name = e.file_name().to_string_lossy();
                name != ".git" && name != "node_modules" && name != "target" && name != "__pycache__"
            })
            .filter_map(Result::ok)
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let Ok(rel) = entry.path().strip_prefix(wt.path()) else {
                continue;
            };
            let norm = normalize_rel_path(&rel.to_string_lossy());
            if norm.is_empty() || is_ignored_path(&norm) {
                continue;
            }
            file_tree.push(norm.clone());

            let meta_len = entry.metadata().map(|m| m.len() as usize).unwrap_or(0);
            if meta_len > MAX_FILE_BYTES {
                continue;
            }
            if is_source_file(&norm) || norm.ends_with("conftest.py") {
                let mut score = file_priority_score(&norm, priority_hints);
                if norm.ends_with("conftest.py") {
                    score += 30;
                }
                candidates.push((score, norm, entry.path().to_path_buf()));
            }
        }

        file_tree.sort();
        candidates.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        candidates.truncate(MAX_LOCAL_SNAPSHOT_FILES);

        let mut sources = BTreeMap::new();
        for (_, rel, full_path) in candidates {
            if let Ok(content) = std::fs::read_to_string(&full_path) {
                if content.len() <= MAX_FILE_BYTES {
                    sources.insert(rel, content);
                }
            }
        }

        drop(wt);
        Ok(RepoSnapshot {
            owner: self.owner.clone(),
            repo: self.repo_name.clone(),
            resolved_ref: self.resolved_sha.clone(),
            file_tree,
            sources,
        })
    }

    /// Loads any newly-discovered `suspect_files` (e.g. from Stage 1 Intake) directly from the
    /// local bare mirror via `git show <sha>:<path>` in milliseconds.
    pub async fn enrich_snapshot_from_local(
        &self,
        snapshot: &mut RepoSnapshot,
        suspect_files: &[String],
    ) -> anyhow::Result<usize> {
        let mut added = 0usize;
        for raw_path in suspect_files {
            let norm = normalize_rel_path(raw_path);
            if norm.is_empty() || snapshot.sources.contains_key(&norm) || !is_safe_rel_path(&norm) {
                continue;
            }
            let spec = format!("{}:{}", self.resolved_sha, norm);
            let out = run_git(
                &self.mirror_dir,
                &["--git-dir", &path_str(&self.mirror_dir), "show", &spec],
                Duration::from_secs(10),
            )
            .await?;
            if out.exit_code == Some(0) && out.stdout.len() <= MAX_FILE_BYTES {
                snapshot.sources.insert(norm, out.stdout);
                added += 1;
            }
        }
        Ok(added)
    }

    /// Deterministically generates a clean `git apply`-valid unified diff for creating (or
    /// replacing) a single standalone file using `git add -N && git diff`.
    pub async fn generate_new_file_patch(
        &self,
        rel_path: &str,
        content: &str,
    ) -> anyhow::Result<String> {
        let norm = normalize_rel_path(rel_path);
        if !is_safe_rel_path(&norm) {
            anyhow::bail!("invalid or unsafe relative path '{rel_path}'");
        }

        let wt = self.checkout_worktree().await?;
        let full_path = wt.path().join(&norm);
        if let Some(parent) = full_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let body = if content.is_empty() || content.ends_with('\n') {
            content.to_string()
        } else {
            format!("{content}\n")
        };
        tokio::fs::write(&full_path, body).await?;

        let add_out = run_git(
            wt.path(),
            &["-C", &path_str(wt.path()), "add", "-N", "--", &norm],
            Duration::from_secs(15),
        )
        .await?;
        if add_out.exit_code != Some(0) {
            anyhow::bail!(
                "git add -N failed for '{norm}': {}",
                tail_chars(&add_out.combined_output, 300)
            );
        }

        let diff_out = run_git(
            wt.path(),
            &["-C", &path_str(wt.path()), "diff", "--", &norm],
            Duration::from_secs(15),
        )
        .await?;
        if diff_out.exit_code != Some(0) {
            anyhow::bail!(
                "git diff failed for '{norm}': {}",
                tail_chars(&diff_out.combined_output, 300)
            );
        }
        Ok(diff_out.stdout)
    }

    /// Applies structured `edits` + `new_files` inside an isolated worktree and captures the exact
    /// canonical `git diff`. Rejects immediately if any path touches `locked_test_path`.
    pub async fn synthesize_diff_from_edits(
        &self,
        edits: &[FileEdit],
        new_files: &[NewFileSpec],
        locked_test_path: Option<&str>,
    ) -> anyhow::Result<EditApplicationResult> {
        let locked_norm = locked_test_path
            .map(normalize_rel_path)
            .filter(|s| !s.is_empty());

        for edit in edits {
            let norm = normalize_rel_path(&edit.path);
            if let Some(ref lp) = locked_norm {
                if paths_match_locked(&norm, lp) {
                    return Ok(EditApplicationResult {
                        git_diff: String::new(),
                        tampered: true,
                        errors: vec![format!(
                            "REJECTED: attempted structured edit on immutable locked Red-Gate file '{lp}'"
                        )],
                    });
                }
            }
        }
        for nf in new_files {
            let norm = normalize_rel_path(&nf.path);
            if let Some(ref lp) = locked_norm {
                if paths_match_locked(&norm, lp) {
                    return Ok(EditApplicationResult {
                        git_diff: String::new(),
                        tampered: true,
                        errors: vec![format!(
                            "REJECTED: attempted new_file overwrite on immutable locked Red-Gate file '{lp}'"
                        )],
                    });
                }
            }
        }

        let wt = self.checkout_worktree().await?;
        let mut errors = Vec::new();

        for edit in edits {
            let norm = normalize_rel_path(&edit.path);
            if !is_safe_rel_path(&norm) {
                errors.push(format!("unsafe edit path '{}'", edit.path));
                continue;
            }
            let full_path = wt.path().join(&norm);
            let Ok(original) = tokio::fs::read_to_string(&full_path).await else {
                errors.push(format!("file not found for edit: '{norm}'"));
                continue;
            };
            match apply_text_replacement(&original, &edit.old_text, &edit.new_text) {
                Ok(updated) => {
                    if let Err(err) = tokio::fs::write(&full_path, updated).await {
                        errors.push(format!("failed writing '{norm}': {err}"));
                    }
                }
                Err(msg) => errors.push(format!("{norm}: {msg}")),
            }
        }

        for nf in new_files {
            let norm = normalize_rel_path(&nf.path);
            if !is_safe_rel_path(&norm) {
                errors.push(format!("unsafe new_file path '{}'", nf.path));
                continue;
            }
            let full_path = wt.path().join(&norm);
            if let Some(parent) = full_path.parent() {
                if let Err(err) = tokio::fs::create_dir_all(parent).await {
                    errors.push(format!("failed creating parent dir for '{norm}': {err}"));
                    continue;
                }
            }
            let body = if nf.content.is_empty() || nf.content.ends_with('\n') {
                nf.content.clone()
            } else {
                format!("{}\n", nf.content)
            };
            if let Err(err) = tokio::fs::write(&full_path, body).await {
                errors.push(format!("failed writing new file '{norm}': {err}"));
                continue;
            }
            let _ = run_git(
                wt.path(),
                &["-C", &path_str(wt.path()), "add", "-N", "--", &norm],
                Duration::from_secs(15),
            )
            .await;
        }

        if !errors.is_empty() {
            return Ok(EditApplicationResult {
                git_diff: String::new(),
                tampered: false,
                errors,
            });
        }

        let diff_out = run_git(
            wt.path(),
            &["-C", &path_str(wt.path()), "diff"],
            Duration::from_secs(15),
        )
        .await?;
        Ok(EditApplicationResult {
            git_diff: diff_out.stdout,
            tampered: false,
            errors: Vec::new(),
        })
    }

    /// Executes an arbitrary shell command inside a given `IsolatedWorktree` using the repository's
    /// isolated runtime environment (`PYTHONPATH`, `PATH`, `PYTHONDONTWRITEBYTECODE=1`, etc.).
    pub async fn run_command_in_worktree(
        &self,
        wt: &IsolatedWorktree,
        command: &str,
        timeout: Duration,
    ) -> anyhow::Result<CommandOutput> {
        let env_vars = self.env_vars_for_worktree(wt.path());
        run_shell_command(wt.path(), command, &env_vars, timeout).await
    }

    /// Independent Stage 2 Red-Gate verification on unmodified base HEAD.
    ///
    /// Rejects patches that touch any file other than `repro_test_path`, fail `git apply`,
    /// pass with exit code 0, time out, or fail due to syntax/import/collection/environment errors.
    pub async fn verify_repro_fails_on_head(
        &self,
        repro_test_path: &str,
        repro_test_patch: &str,
        test_command: &str,
    ) -> anyhow::Result<ReproVerificationResult> {
        let norm_repro = normalize_rel_path(repro_test_path);
        let cmd = self.sanitize_test_command(test_command, Some(&norm_repro));

        if norm_repro.is_empty() || !is_safe_rel_path(&norm_repro) {
            return Ok(ReproVerificationResult {
                patch_applied: false,
                touches_only_repro_file: false,
                verified_fails_on_head: false,
                exit_code: None,
                timed_out: false,
                is_broken_test_or_env_error: true,
                command_used: cmd,
                output_tail: String::new(),
                diagnostic: format!("Invalid repro_test_path '{repro_test_path}'"),
            });
        }

        if repro_test_patch.trim().is_empty() {
            return Ok(ReproVerificationResult {
                patch_applied: false,
                touches_only_repro_file: false,
                verified_fails_on_head: false,
                exit_code: None,
                timed_out: false,
                is_broken_test_or_env_error: true,
                command_used: cmd,
                output_tail: String::new(),
                diagnostic: "Empty repro_test_patch".to_string(),
            });
        }

        // Strict scope check: Red-Gate patch MUST ONLY touch `repro_test_path`.
        let parsed_files = parse_unified_diff(repro_test_patch);
        if parsed_files.is_empty() {
            return Ok(ReproVerificationResult {
                patch_applied: false,
                touches_only_repro_file: false,
                verified_fails_on_head: false,
                exit_code: None,
                timed_out: false,
                is_broken_test_or_env_error: true,
                command_used: cmd,
                output_tail: String::new(),
                diagnostic: "repro_test_patch is not a valid unified diff".to_string(),
            });
        }
        for file in &parsed_files {
            if !file_diff_only_matches(file, &norm_repro) {
                return Ok(ReproVerificationResult {
                    patch_applied: false,
                    touches_only_repro_file: false,
                    verified_fails_on_head: false,
                    exit_code: None,
                    timed_out: false,
                    is_broken_test_or_env_error: false,
                    command_used: cmd,
                    output_tail: String::new(),
                    diagnostic: format!(
                        "Red-Gate patch rejected: touched '{}' outside repro_test_path '{}'",
                        file.path, norm_repro
                    ),
                });
            }
        }

        let wt = self.checkout_worktree().await?;
        let apply_out = apply_patch_in_dir(wt.path(), repro_test_patch).await?;
        if apply_out.exit_code != Some(0) {
            let err_tail = tail_chars(&apply_out.combined_output, 1000);
            return Ok(ReproVerificationResult {
                patch_applied: false,
                touches_only_repro_file: true,
                verified_fails_on_head: false,
                exit_code: apply_out.exit_code,
                timed_out: apply_out.timed_out,
                is_broken_test_or_env_error: true,
                command_used: cmd,
                output_tail: err_tail.clone(),
                diagnostic: format!("git apply failed for repro_test_patch: {err_tail}"),
            });
        }

        let test_out = self
            .run_command_in_worktree(&wt, &cmd, TEST_CMD_TIMEOUT)
            .await?;
        let output_tail = tail_chars(&test_out.combined_output, 1800);

        if test_out.timed_out {
            return Ok(ReproVerificationResult {
                patch_applied: true,
                touches_only_repro_file: true,
                verified_fails_on_head: false,
                exit_code: test_out.exit_code,
                timed_out: true,
                is_broken_test_or_env_error: true,
                command_used: cmd,
                output_tail,
                diagnostic: "Red-Gate test command timed out after 45s".to_string(),
            });
        }

        if test_out.exit_code == Some(0) {
            return Ok(ReproVerificationResult {
                patch_applied: true,
                touches_only_repro_file: true,
                verified_fails_on_head: false,
                exit_code: Some(0),
                timed_out: false,
                is_broken_test_or_env_error: false,
                command_used: cmd,
                output_tail,
                diagnostic: "Reproduction test PASSED on unmodified HEAD (exit code 0); a valid Red-Gate must FAIL on unmodified HEAD.".to_string(),
            });
        }

        if let Some(reason) =
            detect_broken_test_or_env_error(self.runtime, &cmd, test_out.exit_code, &test_out.combined_output)
        {
            return Ok(ReproVerificationResult {
                patch_applied: true,
                touches_only_repro_file: true,
                verified_fails_on_head: false,
                exit_code: test_out.exit_code,
                timed_out: false,
                is_broken_test_or_env_error: true,
                command_used: cmd,
                output_tail,
                diagnostic: format!("Reproduction test rejected as broken/env error: {reason}"),
            });
        }

        Ok(ReproVerificationResult {
            patch_applied: true,
            touches_only_repro_file: true,
            verified_fails_on_head: true,
            exit_code: test_out.exit_code,
            timed_out: false,
            is_broken_test_or_env_error: false,
            command_used: cmd,
            output_tail,
            diagnostic: format!(
                "Verified genuine failure on unmodified HEAD (exit code {:?})",
                test_out.exit_code
            ),
        })
    }

    /// Independent Stage 4 candidate verification inside a fresh isolated worktree.
    ///
    /// Enforces hard anti-tampering against `locked_test_path` (and files touched by
    /// `locked_repro_patch`), applies `candidate_diff` + immutable `locked_repro_patch`,
    /// runs the locked repro test, and runs inferred module regression tests.
    pub async fn verify_candidate_patch(
        &self,
        candidate_diff: &str,
        locked_test_path: Option<&str>,
        locked_repro_patch: &str,
        repro_test_command: &str,
    ) -> anyhow::Result<CandidateVerificationResult> {
        let repro_cmd = self.sanitize_test_command(repro_test_command, locked_test_path);

        let parsed_cand = parse_unified_diff(candidate_diff);
        if candidate_diff.trim().is_empty() || parsed_cand.is_empty() {
            return Ok(CandidateVerificationResult {
                tampered: false,
                patch_applied: false,
                repro_patch_applied: false,
                repro_passed: false,
                regression_suite_passed: false,
                repro_command: repro_cmd,
                regression_command: String::new(),
                test_output_tail: "[REJECTED: empty or unparseable candidate unified diff]".to_string(),
                failure_reason: Some("empty or unparseable unified diff".to_string()),
            });
        }

        // Collect all immutable locked test paths (explicit path + any file in locked_repro_patch).
        let mut locked_paths: Vec<String> = Vec::new();
        if let Some(lp) = locked_test_path.map(normalize_rel_path).filter(|s| !s.is_empty()) {
            locked_paths.push(lp);
        }
        for f in parse_unified_diff(locked_repro_patch) {
            let norm = normalize_rel_path(&f.path);
            if !norm.is_empty() && !locked_paths.contains(&norm) {
                locked_paths.push(norm);
            }
        }

        // Hard independent anti-tampering rejection.
        for file in &parsed_cand {
            for lp in &locked_paths {
                if file_diff_touches_path(file, lp) {
                    let msg = format!(
                        "[REJECTED: candidate diff tampered with immutable locked Red-Gate test '{lp}']"
                    );
                    return Ok(CandidateVerificationResult {
                        tampered: true,
                        patch_applied: false,
                        repro_patch_applied: false,
                        repro_passed: false,
                        regression_suite_passed: false,
                        repro_command: repro_cmd,
                        regression_command: String::new(),
                        test_output_tail: msg.clone(),
                        failure_reason: Some(msg),
                    });
                }
            }
        }

        let wt = self.checkout_worktree().await?;

        // 1. Apply candidate diff cleanly on base HEAD.
        let cand_apply = apply_patch_in_dir(wt.path(), candidate_diff).await?;
        if cand_apply.exit_code != Some(0) {
            let err = tail_chars(&cand_apply.combined_output, 1200);
            return Ok(CandidateVerificationResult {
                tampered: false,
                patch_applied: false,
                repro_patch_applied: false,
                repro_passed: false,
                regression_suite_passed: false,
                repro_command: repro_cmd,
                regression_command: String::new(),
                test_output_tail: format!("[git apply failed for candidate patch]\n{err}"),
                failure_reason: Some(format!("candidate git apply failed: {err}")),
            });
        }

        // 2. Apply immutable locked Red-Gate patch directly from server memory.
        if !locked_repro_patch.trim().is_empty() {
            let repro_apply = apply_patch_in_dir(wt.path(), locked_repro_patch).await?;
            if repro_apply.exit_code != Some(0) {
                let err = tail_chars(&repro_apply.combined_output, 1200);
                return Ok(CandidateVerificationResult {
                    tampered: false,
                    patch_applied: true,
                    repro_patch_applied: false,
                    repro_passed: false,
                    regression_suite_passed: false,
                    repro_command: repro_cmd,
                    regression_command: String::new(),
                    test_output_tail: format!(
                        "[git apply failed for immutable locked Red-Gate patch]\n{err}"
                    ),
                    failure_reason: Some(format!("locked repro patch conflict: {err}")),
                });
            }
        }

        // 3. Run locked Red-Gate test command.
        let repro_out = self
            .run_command_in_worktree(&wt, &repro_cmd, TEST_CMD_TIMEOUT)
            .await?;
        let repro_passed = !repro_out.timed_out && repro_out.exit_code == Some(0);
        let repro_tail = tail_chars(&repro_out.combined_output, 1400);

        if !repro_passed {
            let reason = if repro_out.timed_out {
                format!("locked repro command '{repro_cmd}' timed out after 45s")
            } else {
                format!(
                    "locked repro command '{repro_cmd}' failed (exit code {:?})",
                    repro_out.exit_code
                )
            };
            let tail_with_cmd = format!("$ {repro_cmd}\n{repro_tail}");
            return Ok(CandidateVerificationResult {
                tampered: false,
                patch_applied: true,
                repro_patch_applied: true,
                repro_passed: false,
                regression_suite_passed: false,
                repro_command: repro_cmd,
                regression_command: String::new(),
                test_output_tail: tail_with_cmd,
                failure_reason: Some(reason),
            });
        }

        // 4. Infer & execute targeted module regression tests (bounded to 45s).
        let regression_cmd = self.infer_regression_command(&parsed_cand, &locked_paths);
        let (regression_suite_passed, reg_tail, reg_reason) = if regression_cmd.is_empty() {
            (true, "(no additional module test files in repository)".to_string(), None)
        } else {
            let reg_out = self
                .run_command_in_worktree(&wt, &regression_cmd, TEST_CMD_TIMEOUT)
                .await?;
            let ok = !reg_out.timed_out && reg_out.exit_code == Some(0);
            let tail = tail_chars(&reg_out.combined_output, 1200);
            let reason = if ok {
                None
            } else if reg_out.timed_out {
                Some(format!("regression command '{regression_cmd}' timed out after 45s"))
            } else {
                Some(format!(
                    "regression command '{regression_cmd}' failed (exit code {:?})",
                    reg_out.exit_code
                ))
            };
            (ok, tail, reason)
        };

        let combined_tail = truncate_str(
            &format!(
                "$ {repro_cmd}\n{}\n\n$ {}\n{}",
                tail_chars(&repro_tail, 700),
                if regression_cmd.is_empty() { "(no extra regression suite)" } else { &regression_cmd },
                tail_chars(&reg_tail, 1000)
            ),
            2000,
        );

        Ok(CandidateVerificationResult {
            tampered: false,
            patch_applied: true,
            repro_patch_applied: true,
            repro_passed,
            regression_suite_passed,
            repro_command: repro_cmd,
            regression_command: regression_cmd,
            test_output_tail: combined_tail,
            failure_reason: reg_reason,
        })
    }

    /// Independent Stage 5 Single-Host Interleaved Benchmark Arena + per-candidate coverage capture.
    ///
    /// Executes inside ONE clean worktree on the host CPU, interleaving
    /// `Baseline -> Candidate_1 -> ... -> Candidate_M` over `warmup` + `trials` iterations,
    /// timing every execution with `std::time::Instant::now()`, then capturing real coverage
    /// with the immutable locked Red-Gate patch applied.
    #[allow(clippy::too_many_arguments)]
    pub async fn run_interleaved_arena(
        &self,
        bench_spec: &BenchmarkHarnessSpec,
        locked_test_path: Option<&str>,
        locked_repro_patch: &str,
        repro_test_command: &str,
        candidates: &[ArenaCandidateInput],
        session_id: Option<Uuid>,
        event_tx: Option<&broadcast::Sender<LiveEvent>>,
    ) -> anyhow::Result<HostArenaResult> {
        let trials = if bench_spec.trials == 0 {
            5
        } else {
            bench_spec.trials.clamp(3, 7)
        };
        let warmup = bench_spec.warmup_trials.clamp(1, 2);

        if candidates.is_empty() {
            return Ok(HostArenaResult {
                baseline_samples_ms: Vec::new(),
                candidates: Vec::new(),
                trials_run: trials,
                benchmark_used: String::new(),
                summary_notes: "No green candidates to benchmark.".to_string(),
            });
        }

        let wt = self.checkout_worktree().await?;
        let scratch = tempfile::Builder::new()
            .prefix("epicenter-arena-")
            .tempdir()?;

        // Persist all patch files outside the worktree so `git clean -fd` never touches them.
        let repro_patch_file = scratch.path().join("repro.patch");
        let has_repro_patch = !locked_repro_patch.trim().is_empty();
        if has_repro_patch {
            tokio::fs::write(&repro_patch_file, ensure_trailing_newline(locked_repro_patch)).await?;
        }
        let mut cand_patch_files = Vec::with_capacity(candidates.len());
        for (idx, c) in candidates.iter().enumerate() {
            let p = scratch.path().join(format!("cand_{idx}.patch"));
            tokio::fs::write(&p, ensure_trailing_newline(&c.git_diff)).await?;
            cand_patch_files.push(p);
        }

        let sanitized_repro_cmd = self.sanitize_test_command(repro_test_command, locked_test_path);

        // Select runnable benchmark command (prefer script_content -> bench_spec.command -> repro test).
        let (bench_cmd, needs_repro_during_bench, bench_label) = self
            .resolve_arena_benchmark_command(
                &wt,
                scratch.path(),
                bench_spec,
                &sanitized_repro_cmd,
                has_repro_patch,
                &repro_patch_file,
            )
            .await?;

        // Warmup pass on clean baseline.
        for _ in 0..warmup {
            reset_worktree(wt.path()).await?;
            if needs_repro_during_bench && has_repro_patch {
                let _ = apply_patch_file_in_dir(wt.path(), &repro_patch_file).await;
            }
            let _ = self
                .run_command_in_worktree(&wt, &bench_cmd, BENCH_TRIAL_TIMEOUT)
                .await;
        }

        let mut baseline_samples_ms = Vec::with_capacity(trials as usize);
        let mut per_cand_samples: Vec<Vec<f64>> =
            vec![Vec::with_capacity(trials as usize); candidates.len()];

        // Strict interleaved measurement loop on the single host worktree.
        for trial_idx in 0..trials {
            // Baseline timing
            reset_worktree(wt.path()).await?;
            if needs_repro_during_bench && has_repro_patch {
                let _ = apply_patch_file_in_dir(wt.path(), &repro_patch_file).await;
            }
            let base_started = Instant::now();
            let _ = self
                .run_command_in_worktree(&wt, &bench_cmd, BENCH_TRIAL_TIMEOUT)
                .await?;
            let base_ms = base_started.elapsed().as_secs_f64() * 1000.0;
            baseline_samples_ms.push(base_ms);

            // Each candidate in deterministic order
            for (c_idx, cand) in candidates.iter().enumerate() {
                reset_worktree(wt.path()).await?;
                let apply_res = apply_patch_file_in_dir(wt.path(), &cand_patch_files[c_idx]).await?;
                if apply_res.exit_code != Some(0) {
                    warn!(
                        strategy = %cand.strategy_id,
                        "failed to apply candidate patch during arena trial {trial_idx}"
                    );
                    continue;
                }
                if needs_repro_during_bench && has_repro_patch {
                    let _ = apply_patch_file_in_dir(wt.path(), &repro_patch_file).await;
                }

                let cand_started = Instant::now();
                let _ = self
                    .run_command_in_worktree(&wt, &bench_cmd, BENCH_TRIAL_TIMEOUT)
                    .await?;
                let cand_ms = cand_started.elapsed().as_secs_f64() * 1000.0;
                per_cand_samples[c_idx].push(cand_ms);

                if let (Some(sid), Some(tx)) = (session_id, event_tx) {
                    let opt_tx = Some(tx.clone());
                    emit_event(
                        &opt_tx,
                        LiveEvent::ArenaTrialSample {
                            session_id: sid,
                            strategy_id: cand.strategy_id.clone(),
                            trial_index: trial_idx,
                            baseline_ms: base_ms,
                            candidate_ms: cand_ms,
                        },
                    );
                }
            }
        }

        // Capture genuine per-candidate line coverage for Layer 2 Dark-Zone mapping.
        let mut locked_paths: Vec<String> = Vec::new();
        if let Some(lp) = locked_test_path.map(normalize_rel_path).filter(|s| !s.is_empty()) {
            locked_paths.push(lp);
        }
        let mut candidate_results = Vec::with_capacity(candidates.len());

        for (c_idx, cand) in candidates.iter().enumerate() {
            reset_worktree(wt.path()).await?;
            let mut coverage_map = None;
            let applied_cand = apply_patch_file_in_dir(wt.path(), &cand_patch_files[c_idx]).await?;
            if applied_cand.exit_code == Some(0) {
                if has_repro_patch {
                    let _ = apply_patch_file_in_dir(wt.path(), &repro_patch_file).await;
                }
                let parsed_cand = parse_unified_diff(&cand.git_diff);
                coverage_map = self
                    .capture_coverage_in_worktree(
                        &wt,
                        scratch.path(),
                        c_idx,
                        &parsed_cand,
                        &locked_paths,
                    )
                    .await;
            }

            candidate_results.push(HostArenaCandidateResult {
                strategy_id: cand.strategy_id.clone(),
                baseline_samples_ms: baseline_samples_ms.clone(),
                candidate_samples_ms: per_cand_samples[c_idx].clone(),
                peak_memory_kb: None,
                coverage_map,
            });
        }

        let summary_notes = format!(
            "Single-host interleaved Arena ran {trials} measured trials (+{warmup} warmup) across {} candidate(s) using {bench_label}.",
            candidates.len()
        );

        Ok(HostArenaResult {
            baseline_samples_ms,
            candidates: candidate_results,
            trials_run: trials,
            benchmark_used: bench_label,
            summary_notes,
        })
    }

    /// Independently grades any candidate diff against a hidden `ground_truth_test_patch`
    /// inside an isolated temporary worktree on the host. Exposed publicly for `epicenter-eval`
    /// and `epicenter-server` live-judge endpoints.
    pub async fn grade_against_ground_truth(
        &self,
        candidate_diff: &str,
        ground_truth_test_patch: &str,
    ) -> anyhow::Result<GroundTruthGradeResult> {
        let parsed_cand = parse_unified_diff(candidate_diff);
        if candidate_diff.trim().is_empty() || parsed_cand.is_empty() {
            return Ok(GroundTruthGradeResult {
                patch_applied: false,
                tampered_with_ground_truth: false,
                ground_truth_patch_applied: false,
                resolved_ground_truth: false,
                regression_free: true,
                ground_truth_command: String::new(),
                regression_command: String::new(),
                test_output_tail: "Candidate diff is empty or unparseable".to_string(),
                coverage_map: None,
                failure_category: Some("empty_or_unparseable_diff".to_string()),
            });
        }

        let gt_files: Vec<String> = parse_unified_diff(ground_truth_test_patch)
            .into_iter()
            .map(|f| normalize_rel_path(&f.path))
            .filter(|p| !p.is_empty())
            .collect();

        // Hard anti-tampering check against ground-truth test files.
        for f in &parsed_cand {
            for gt in &gt_files {
                if file_diff_touches_path(f, gt) {
                    return Ok(GroundTruthGradeResult {
                        patch_applied: false,
                        tampered_with_ground_truth: true,
                        ground_truth_patch_applied: false,
                        resolved_ground_truth: false,
                        regression_free: false,
                        ground_truth_command: String::new(),
                        regression_command: String::new(),
                        test_output_tail: format!(
                            "[REJECTED: candidate diff tampered with hidden ground-truth test '{gt}']"
                        ),
                        coverage_map: None,
                        failure_category: Some("test_tampering_violation".to_string()),
                    });
                }
            }
        }

        let wt = self.checkout_worktree().await?;

        let cand_apply = apply_patch_in_dir(wt.path(), candidate_diff).await?;
        if cand_apply.exit_code != Some(0) {
            return Ok(GroundTruthGradeResult {
                patch_applied: false,
                tampered_with_ground_truth: false,
                ground_truth_patch_applied: false,
                resolved_ground_truth: false,
                regression_free: false,
                ground_truth_command: String::new(),
                regression_command: String::new(),
                test_output_tail: tail_chars(&cand_apply.combined_output, 1500),
                coverage_map: None,
                failure_category: Some("patch_apply_failed".to_string()),
            });
        }

        if !ground_truth_test_patch.trim().is_empty() {
            let gt_apply = apply_patch_in_dir(wt.path(), ground_truth_test_patch).await?;
            if gt_apply.exit_code != Some(0) {
                return Ok(GroundTruthGradeResult {
                    patch_applied: true,
                    tampered_with_ground_truth: false,
                    ground_truth_patch_applied: false,
                    resolved_ground_truth: false,
                    regression_free: false,
                    ground_truth_command: String::new(),
                    regression_command: String::new(),
                    test_output_tail: tail_chars(&gt_apply.combined_output, 1500),
                    coverage_map: None,
                    failure_category: Some("ground_truth_apply_failed".to_string()),
                });
            }
        }

        let gt_cmd = self.command_for_test_paths(&gt_files);
        let gt_out = self
            .run_command_in_worktree(&wt, &gt_cmd, GROUND_TRUTH_TIMEOUT)
            .await?;
        let resolved = !gt_out.timed_out && gt_out.exit_code == Some(0);
        let gt_tail = tail_chars(&gt_out.combined_output, 1200);

        let reg_cmd = self.infer_regression_command(&parsed_cand, &gt_files);
        let (reg_ok, reg_tail) = if reg_cmd.is_empty() {
            (true, String::new())
        } else {
            let reg_out = self
                .run_command_in_worktree(&wt, &reg_cmd, GROUND_TRUTH_TIMEOUT)
                .await?;
            (
                !reg_out.timed_out && reg_out.exit_code == Some(0),
                tail_chars(&reg_out.combined_output, 800),
            )
        };

        let coverage_map = if resolved {
            let scratch = TempDir::new()?;
            self.capture_coverage_in_worktree(&wt, scratch.path(), 0, &parsed_cand, &gt_files)
                .await
        } else {
            None
        };

        let failure_category = if resolved && reg_ok {
            None
        } else if !resolved {
            if gt_out.timed_out {
                Some("ground_truth_timeout".to_string())
            } else {
                Some("ground_truth_test_failed".to_string())
            }
        } else {
            Some("regression_test_failed".to_string())
        };

        let combined_tail = truncate_str(
            &format!("$ {gt_cmd}\n{gt_tail}\n\n$ {reg_cmd}\n{reg_tail}"),
            2000,
        );

        Ok(GroundTruthGradeResult {
            patch_applied: true,
            tampered_with_ground_truth: false,
            ground_truth_patch_applied: true,
            resolved_ground_truth: resolved,
            regression_free: reg_ok,
            ground_truth_command: gt_cmd,
            regression_command: reg_cmd,
            test_output_tail: combined_tail,
            coverage_map,
            failure_category,
        })
    }

    fn env_vars_for_worktree(&self, worktree_dir: &Path) -> Vec<(String, String)> {
        let mut vars = Vec::new();
        let current_path = std::env::var("PATH").unwrap_or_else(|_| {
            "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin".to_string()
        });

        match self.runtime {
            RepoRuntime::Python => {
                if let Some(ref venv) = self.env.python_venv_dir {
                    let bin = venv.join("bin");
                    vars.push(("PATH".to_string(), format!("{}:{current_path}", bin.display())));
                    vars.push(("VIRTUAL_ENV".to_string(), venv.display().to_string()));
                }
                let src_dir = worktree_dir.join("src");
                let pythonpath = if src_dir.is_dir() {
                    format!("{}:{}", src_dir.display(), worktree_dir.display())
                } else {
                    worktree_dir.display().to_string()
                };
                vars.push(("PYTHONPATH".to_string(), pythonpath));
                // Prevent stale `.pyc` bytecode caching across interleaved Arena trials!
                vars.push(("PYTHONDONTWRITEBYTECODE".to_string(), "1".to_string()));
            }
            RepoRuntime::NodeTs => {
                if let Some(ref node_env) = self.env.node_env_dir {
                    let bin = node_env.join("node_modules").join(".bin");
                    vars.push(("PATH".to_string(), format!("{}:{current_path}", bin.display())));
                }
                vars.push(("CI".to_string(), "true".to_string()));
            }
            RepoRuntime::Rust => {
                if let Some(ref target_dir) = self.env.cargo_target_dir {
                    vars.push((
                        "CARGO_TARGET_DIR".to_string(),
                        target_dir.display().to_string(),
                    ));
                }
            }
            RepoRuntime::Go | RepoRuntime::Unknown => {}
        }

        vars
    }

    fn command_for_test_paths(&self, test_paths: &[String]) -> String {
        let valid: Vec<String> = test_paths
            .iter()
            .map(|p| normalize_rel_path(p))
            .filter(|p| !p.is_empty() && is_safe_rel_path(p))
            .collect();
        if valid.is_empty() {
            return self.default_test_command(None);
        }
        let joined = valid
            .iter()
            .map(|p| shell_quote(p))
            .collect::<Vec<_>>()
            .join(" ");
        match self.runtime {
            RepoRuntime::Python => format!("pytest -q {joined}"),
            RepoRuntime::NodeTs => match self.node_runner {
                NodeTestRunner::Vitest => format!("npx vitest run {joined}"),
                NodeTestRunner::Jest => format!("npx jest {joined}"),
                NodeTestRunner::NodeTest => format!("node --test {joined}"),
                NodeTestRunner::NpmTest => "npm test".to_string(),
            },
            RepoRuntime::Rust => "cargo test".to_string(),
            RepoRuntime::Go => {
                let mut dirs: Vec<String> = valid
                    .iter()
                    .map(|p| {
                        Path::new(p)
                            .parent()
                            .and_then(|d| d.to_str())
                            .filter(|s| !s.is_empty())
                            .map(|d| format!("./{d}"))
                            .unwrap_or_else(|| "./.".to_string())
                    })
                    .collect();
                dirs.sort();
                dirs.dedup();
                format!("go test {}", dirs.join(" "))
            }
            RepoRuntime::Unknown => "true".to_string(),
        }
    }

    /// Infers a fast, targeted module regression command from touched source paths + existing
    /// repository test files (plus any caller tests added by the candidate).
    fn infer_regression_command(
        &self,
        touched_files: &[FileDiff],
        excluded_locked_paths: &[String],
    ) -> String {
        match self.runtime {
            RepoRuntime::Rust => return "cargo test".to_string(),
            RepoRuntime::Go => {
                let mut pkgs: Vec<String> = touched_files
                    .iter()
                    .filter(|f| f.path.ends_with(".go"))
                    .map(|f| {
                        Path::new(&f.path)
                            .parent()
                            .and_then(|d| d.to_str())
                            .filter(|s| !s.is_empty())
                            .map(|d| format!("./{d}"))
                            .unwrap_or_else(|| "./.".to_string())
                    })
                    .collect();
                pkgs.sort();
                pkgs.dedup();
                return if pkgs.is_empty() {
                    "go test ./...".to_string()
                } else {
                    format!("go test {}", pkgs.join(" "))
                };
            }
            RepoRuntime::Unknown => return String::new(),
            RepoRuntime::Python | RepoRuntime::NodeTs => {}
        }

        let excluded: HashSet<String> = excluded_locked_paths
            .iter()
            .map(|p| normalize_rel_path(p))
            .collect();

        // Collect stems from touched production files.
        let mut stems: Vec<String> = Vec::new();
        let mut added_caller_tests: Vec<String> = Vec::new();

        for f in touched_files {
            let norm = normalize_rel_path(&f.path);
            if excluded.contains(&norm) {
                continue;
            }
            if is_test_like_path(&norm) && !f.is_deleted_file {
                added_caller_tests.push(norm);
                continue;
            }
            if let Some(stem) = Path::new(&norm).file_stem().and_then(|s| s.to_str()) {
                let lower = stem.to_ascii_lowercase();
                if lower.len() >= 3 && lower != "__init__" && lower != "index" && lower != "mod" {
                    stems.push(lower);
                }
            }
            if let Some(parent) = Path::new(&norm)
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|s| s.to_str())
            {
                let lower = parent.to_ascii_lowercase();
                if lower.len() >= 3 && lower != "src" && lower != "lib" && lower != "app" {
                    stems.push(lower);
                }
            }
        }

        let all_repo_tests: Vec<&String> = self
            .file_tree
            .iter()
            .filter(|p| !excluded.contains(p.as_str()) && is_runtime_regression_test(self.runtime, p))
            .collect();

        let mut selected: Vec<String> = Vec::new();
        for ct in added_caller_tests {
            if !selected.contains(&ct) {
                selected.push(ct);
            }
        }

        if all_repo_tests.len() <= 8 {
            for p in all_repo_tests {
                if !selected.contains(p) {
                    selected.push(p.clone());
                }
            }
        } else {
            let mut scored: Vec<(i32, &String)> = all_repo_tests
                .into_iter()
                .map(|p| {
                    let lower = p.to_ascii_lowercase();
                    let mut score = 0i32;
                    for stem in &stems {
                        if lower.contains(stem) {
                            score += 50;
                        }
                    }
                    if lower.contains("core") || lower.contains("basic") || lower.contains("option") {
                        score += 5;
                    }
                    (score, p)
                })
                .collect();
            scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1)));
            for (_, p) in scored.into_iter().take(6) {
                if !selected.contains(p) {
                    selected.push(p.clone());
                }
            }
        }

        if selected.is_empty() {
            return String::new();
        }
        self.command_for_test_paths(&selected)
    }

    async fn resolve_arena_benchmark_command(
        &self,
        wt: &IsolatedWorktree,
        scratch_dir: &Path,
        bench_spec: &BenchmarkHarnessSpec,
        sanitized_repro_cmd: &str,
        has_repro_patch: bool,
        repro_patch_file: &Path,
    ) -> anyhow::Result<(String, bool, String)> {
        // 1. Try explicit script_content written outside git worktree.
        if let Some(script) = bench_spec
            .script_content
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let (ext, runner) = match self.runtime {
                RepoRuntime::Python => ("py", "python"),
                RepoRuntime::NodeTs => ("mjs", "node"),
                _ => ("sh", "sh"),
            };
            let script_path = scratch_dir.join(format!("epicenter_bench.{ext}"));
            tokio::fs::write(&script_path, ensure_trailing_newline(script)).await?;
            let cmd = format!("{runner} {}", shell_quote(&path_str(&script_path)));

            reset_worktree(wt.path()).await?;
            let probe = self
                .run_command_in_worktree(wt, &cmd, Duration::from_secs(10))
                .await?;
            if !probe.timed_out && probe.exit_code == Some(0) {
                return Ok((cmd, false, "custom micro-benchmark script".to_string()));
            }
            debug!(
                exit_code = ?probe.exit_code,
                err = %tail_chars(&probe.combined_output, 240),
                "Arena script_content probe failed on baseline; falling back"
            );
        }

        // 2. Try bench_spec.command directly if distinct from repro command.
        let spec_cmd = bench_spec.command.trim();
        if !spec_cmd.is_empty() && spec_cmd != sanitized_repro_cmd {
            let safe_cmd = self.sanitize_test_command(spec_cmd, None);
            reset_worktree(wt.path()).await?;
            let probe = self
                .run_command_in_worktree(wt, &safe_cmd, Duration::from_secs(10))
                .await?;
            if !probe.timed_out && probe.exit_code == Some(0) {
                return Ok((safe_cmd.clone(), false, format!("benchmark `{safe_cmd}`")));
            }
        }

        // 3. Fallback: time the locked repro test command (with locked_repro_patch applied).
        if has_repro_patch {
            reset_worktree(wt.path()).await?;
            let _ = apply_patch_file_in_dir(wt.path(), repro_patch_file).await;
        }
        Ok((
            sanitized_repro_cmd.to_string(),
            has_repro_patch,
            format!("repro test `{sanitized_repro_cmd}`"),
        ))
    }

    async fn capture_coverage_in_worktree(
        &self,
        wt: &IsolatedWorktree,
        scratch_dir: &Path,
        cand_idx: usize,
        parsed_cand: &[FileDiff],
        locked_paths: &[String],
    ) -> Option<CoverageMap> {
        match self.runtime {
            RepoRuntime::Python => {
                let mut targets: Vec<String> = locked_paths.to_vec();
                for f in parsed_cand {
                    let norm = normalize_rel_path(&f.path);
                    if is_test_like_path(&norm)
                        && norm.ends_with(".py")
                        && !f.is_deleted_file
                        && !targets.contains(&norm)
                    {
                        targets.push(norm);
                    }
                }
                // Add up to 3 relevant repo test files for realistic Ring 1/2 coverage.
                let reg_cmd = self.infer_regression_command(parsed_cand, locked_paths);
                if let Some(rest) = reg_cmd.strip_prefix("pytest -q ") {
                    for tok in rest.split_whitespace().take(3) {
                        let clean = tok.trim_matches('\'').trim_matches('"').to_string();
                        if !clean.is_empty() && !targets.contains(&clean) {
                            targets.push(clean);
                        }
                    }
                }

                let target_args = if targets.is_empty() {
                    String::new()
                } else {
                    targets
                        .iter()
                        .map(|p| shell_quote(p))
                        .collect::<Vec<_>>()
                        .join(" ")
                };

                let cov_json_path = scratch_dir.join(format!("cov_{cand_idx}.json"));
                let cov_data_path = scratch_dir.join(format!("cov_{cand_idx}.data"));
                let cmd = format!(
                    "COVERAGE_FILE={} coverage run --source=. -m pytest -q {target_args} && COVERAGE_FILE={} coverage json -o {}",
                    shell_quote(&path_str(&cov_data_path)),
                    shell_quote(&path_str(&cov_data_path)),
                    shell_quote(&path_str(&cov_json_path)),
                );
                let out = self
                    .run_command_in_worktree(wt, &cmd, COVERAGE_TIMEOUT)
                    .await
                    .ok()?;
                if out.timed_out || !cov_json_path.exists() {
                    return None;
                }
                let raw = tokio::fs::read_to_string(&cov_json_path).await.ok()?;
                CoverageMap::from_coverage_json(&raw).ok()
            }
            RepoRuntime::NodeTs => None,
            RepoRuntime::Rust | RepoRuntime::Go | RepoRuntime::Unknown => None,
        }
    }
}

// ============================================================================
// Internal Helpers
// ============================================================================

fn repo_lock(key: &str) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    let map = LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = map.lock().expect("repo lock registry poisoned");
    guard
        .entry(key.to_string())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

fn env_lock(key: &str) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    let map = LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = map.lock().expect("env lock registry poisoned");
    guard
        .entry(key.to_string())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

fn derive_repo_identity(repo_url: &str) -> (String, String, String) {
    if let Some(id) = parse_github_repo_url(repo_url) {
        let key = format!(
            "{}__{}",
            sanitize_fs_component(&id.owner),
            sanitize_fs_component(&id.repo)
        );
        return (id.owner, id.repo, key);
    }
    let clean = repo_url
        .trim()
        .strip_prefix("file://")
        .unwrap_or(repo_url.trim())
        .trim_end_matches('/');
    let name = Path::new(clean)
        .file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.strip_suffix(".git").unwrap_or(s))
        .unwrap_or("repo")
        .to_string();
    let hash = fnv1a_64(clean.as_bytes());
    let key = format!("local__{}_{:08x}", sanitize_fs_component(&name), hash as u32);
    ("local".to_string(), name, key)
}

fn resolve_git_clone_source(repo_url: &str) -> String {
    let trimmed = repo_url.trim();
    if trimmed.starts_with("file://") {
        return trimmed.to_string();
    }
    let as_path = Path::new(trimmed);
    if as_path.exists() {
        let canonical = std::fs::canonicalize(as_path).unwrap_or_else(|_| as_path.to_path_buf());
        return format!("file://{}", canonical.display());
    }
    normalize_clone_url(trimmed)
}

fn sanitize_fs_component(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "repo".to_string()
    } else {
        cleaned
    }
}

fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

async fn ensure_bare_mirror(mirror_dir: &Path, clone_url: &str) -> anyhow::Result<()> {
    if !mirror_dir.join("HEAD").exists() {
        if mirror_dir.exists() {
            let _ = tokio::fs::remove_dir_all(mirror_dir).await;
        }
        tokio::fs::create_dir_all(mirror_dir).await?;
        let init = run_git(
            mirror_dir,
            &["init", "--bare", "-q", &path_str(mirror_dir)],
            Duration::from_secs(15),
        )
        .await?;
        if init.exit_code != Some(0) {
            anyhow::bail!(
                "git init --bare failed: {}",
                tail_chars(&init.combined_output, 300)
            );
        }
        let remote = run_git(
            mirror_dir,
            &[
                "--git-dir",
                &path_str(mirror_dir),
                "remote",
                "add",
                "origin",
                clone_url,
            ],
            Duration::from_secs(15),
        )
        .await?;
        if remote.exit_code != Some(0) {
            anyhow::bail!(
                "git remote add origin failed: {}",
                tail_chars(&remote.combined_output, 300)
            );
        }
    } else {
        let _ = run_git(
            mirror_dir,
            &[
                "--git-dir",
                &path_str(mirror_dir),
                "remote",
                "set-url",
                "origin",
                clone_url,
            ],
            Duration::from_secs(15),
        )
        .await;
    }
    Ok(())
}

async fn resolve_and_fetch_commit(
    mirror_dir: &Path,
    commit_sha: Option<&str>,
    git_ref: Option<&str>,
) -> anyhow::Result<String> {
    let git_dir = path_str(mirror_dir);

    if let Some(sha) = commit_sha.map(str::trim).filter(|s| !s.is_empty()) {
        // Fast path: commit already present in local bare mirror (0ms network!).
        if let Some(full) = verify_commit_present(mirror_dir, sha).await {
            return Ok(full);
        }

        // Attempt 1: shallow single-commit fetch (works on GitHub and modern git remotes).
        let _ = run_git(
            mirror_dir,
            &["--git-dir", &git_dir, "fetch", "--depth", "1", "origin", sha],
            GIT_FETCH_TIMEOUT,
        )
        .await;
        if let Some(full) = verify_commit_present(mirror_dir, sha).await {
            return Ok(full);
        }

        // Attempt 2: unshallow direct SHA fetch.
        let _ = run_git(
            mirror_dir,
            &["--git-dir", &git_dir, "fetch", "origin", sha],
            GIT_FETCH_TIMEOUT,
        )
        .await;
        if let Some(full) = verify_commit_present(mirror_dir, sha).await {
            return Ok(full);
        }

        // Attempt 3: fetch full branches/tags history (needed for short SHAs or older servers).
        let full_fetch = run_git(
            mirror_dir,
            &[
                "--git-dir",
                &git_dir,
                "fetch",
                "origin",
                "+refs/heads/*:refs/remotes/origin/*",
                "+refs/tags/*:refs/tags/*",
            ],
            Duration::from_secs(120),
        )
        .await?;
        if let Some(full) = verify_commit_present(mirror_dir, sha).await {
            return Ok(full);
        }

        anyhow::bail!(
            "unable to resolve commit '{sha}' in mirror '{}': {}",
            mirror_dir.display(),
            tail_chars(&full_fetch.combined_output, 300)
        );
    }

    if let Some(r) = git_ref.map(str::trim).filter(|s| !s.is_empty()) {
        let fetch = run_git(
            mirror_dir,
            &["--git-dir", &git_dir, "fetch", "--depth", "1", "origin", r],
            GIT_FETCH_TIMEOUT,
        )
        .await?;
        if fetch.exit_code == Some(0) {
            if let Some(full) = verify_commit_present(mirror_dir, "FETCH_HEAD").await {
                return Ok(full);
            }
        }
        let _ = run_git(
            mirror_dir,
            &[
                "--git-dir",
                &git_dir,
                "fetch",
                "origin",
                "+refs/heads/*:refs/remotes/origin/*",
                "+refs/tags/*:refs/tags/*",
            ],
            Duration::from_secs(90),
        )
        .await;
        for candidate in [format!("refs/remotes/origin/{r}"), r.to_string()] {
            if let Some(full) = verify_commit_present(mirror_dir, &candidate).await {
                return Ok(full);
            }
        }
        anyhow::bail!("unable to resolve git ref '{r}'");
    }

    // Default branch / HEAD
    let head_fetch = run_git(
        mirror_dir,
        &["--git-dir", &git_dir, "fetch", "--depth", "1", "origin", "HEAD"],
        GIT_FETCH_TIMEOUT,
    )
    .await?;
    if head_fetch.exit_code == Some(0) {
        if let Some(full) = verify_commit_present(mirror_dir, "FETCH_HEAD").await {
            return Ok(full);
        }
    }

    let fallback = run_git(
        mirror_dir,
        &[
            "--git-dir",
            &git_dir,
            "fetch",
            "--depth",
            "1",
            "origin",
            "+refs/heads/*:refs/remotes/origin/*",
        ],
        GIT_FETCH_TIMEOUT,
    )
    .await?;
    for candidate in [
        "FETCH_HEAD",
        "refs/remotes/origin/main",
        "refs/remotes/origin/master",
        "HEAD",
    ] {
        if let Some(full) = verify_commit_present(mirror_dir, candidate).await {
            return Ok(full);
        }
    }

    anyhow::bail!(
        "unable to resolve repository HEAD: {}",
        tail_chars(&fallback.combined_output, 300)
    )
}

async fn verify_commit_present(mirror_dir: &Path, rev: &str) -> Option<String> {
    let spec = format!("{rev}^{{commit}}");
    let out = run_git(
        mirror_dir,
        &[
            "--git-dir",
            &path_str(mirror_dir),
            "rev-parse",
            "--verify",
            "-q",
            &spec,
        ],
        Duration::from_secs(10),
    )
    .await
    .ok()?;
    if out.exit_code == Some(0) {
        let sha = out.stdout.trim().to_string();
        if !sha.is_empty() {
            return Some(sha);
        }
    }
    None
}

fn collect_file_tree(root: &Path) -> Vec<String> {
    let mut files = Vec::new();
    for entry in WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            name != ".git" && name != "node_modules" && name != "target" && name != "__pycache__"
        })
        .filter_map(Result::ok)
    {
        if !entry.file_type().is_file() {
            continue;
        }
        if let Ok(rel) = entry.path().strip_prefix(root) {
            let norm = normalize_rel_path(&rel.to_string_lossy());
            if !norm.is_empty() && !is_ignored_path(&norm) {
                files.push(norm);
            }
        }
    }
    files.sort();
    files
}

fn find_uv_binary() -> Option<PathBuf> {
    let mut candidates = vec![
        PathBuf::from("/opt/homebrew/bin/uv"),
        PathBuf::from("/usr/local/bin/uv"),
    ];
    if let Ok(home) = std::env::var("HOME") {
        candidates.push(PathBuf::from(&home).join(".local/bin/uv"));
        candidates.push(PathBuf::from(&home).join(".cargo/bin/uv"));
    }
    for path in candidates {
        if path.is_file() {
            return Some(path);
        }
    }
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let p = dir.join("uv");
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

async fn create_stdlib_venv(venv_dir: &Path) -> anyhow::Result<()> {
    let out = run_program(
        venv_dir.parent().unwrap_or_else(|| Path::new("/tmp")),
        "python3",
        &["-m", "venv", &path_str(venv_dir)],
        &[],
        ENV_SETUP_TIMEOUT,
    )
    .await?;
    if out.exit_code != Some(0) {
        anyhow::bail!(
            "python3 -m venv failed: {}",
            tail_chars(&out.combined_output, 300)
        );
    }
    Ok(())
}

fn discover_local_python_packages(worktree_dir: &Path) -> HashSet<String> {
    let mut names = HashSet::new();
    let skip: HashSet<&str> = [
        "tests", "test", "docs", "doc", "scripts", "examples", "build", "dist", "bench",
        "benchmarks",
    ]
    .into_iter()
    .collect();

    for base in [worktree_dir.to_path_buf(), worktree_dir.join("src")] {
        let Ok(entries) = std::fs::read_dir(&base) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if name.starts_with('.') || skip.contains(name) {
                continue;
            }
            if path.is_dir() && path.join("__init__.py").exists() {
                names.insert(name.to_string());
            } else if path.is_file() && name.ends_with(".py") && name != "setup.py" && name != "conftest.py" {
                if let Some(stem) = name.strip_suffix(".py") {
                    names.insert(stem.to_string());
                }
            }
        }
    }
    names
}

fn strip_editable_and_local_links(
    venv_dir: &Path,
    setup_checkout_dir: &Path,
    local_pkgs: &HashSet<String>,
) -> usize {
    let mut removed = 0usize;
    let lib_dir = venv_dir.join("lib");
    let Ok(py_dirs) = std::fs::read_dir(&lib_dir) else {
        return 0;
    };
    let setup_str = setup_checkout_dir.to_string_lossy().to_string();
    let canonical_setup = std::fs::canonicalize(setup_checkout_dir)
        .ok()
        .map(|p| p.to_string_lossy().to_string());

    for py_dir in py_dirs.flatten() {
        let site_packages = py_dir.path().join("site-packages");
        let Ok(entries) = std::fs::read_dir(&site_packages) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(fname) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };

            let should_remove = if fname.starts_with("__editable__")
                || fname.starts_with("_editable")
                || fname.ends_with(".egg-link")
            {
                true
            } else if fname.ends_with(".pth")
                && !fname.starts_with("_virtualenv")
                && fname != "distutils-precedence.pth"
            {
                std::fs::read_to_string(&path)
                    .map(|c| {
                        c.contains(&setup_str)
                            || canonical_setup
                                .as_deref()
                                .map(|cs| c.contains(cs))
                                .unwrap_or(false)
                            || c.contains("epicenter-wt-")
                            || c.contains("epicenter-cache")
                            || c.contains("__editable__")
                    })
                    .unwrap_or(false)
            } else if path.is_dir() && local_pkgs.contains(fname) {
                true
            } else if path.is_file() {
                fname
                    .strip_suffix(".py")
                    .map(|stem| local_pkgs.contains(stem))
                    .unwrap_or(false)
            } else {
                false
            };

            if should_remove {
                let res = if path.is_dir() {
                    std::fs::remove_dir_all(&path)
                } else {
                    std::fs::remove_file(&path)
                };
                if res.is_ok() {
                    removed += 1;
                }
            }
        }
    }
    removed
}

fn has_npm_dependencies(repo_dir: &Path) -> bool {
    let Ok(raw) = std::fs::read_to_string(repo_dir.join("package.json")) else {
        return false;
    };
    let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return false;
    };
    for field in ["dependencies", "devDependencies", "peerDependencies"] {
        if let Some(obj) = val.get(field).and_then(|v| v.as_object()) {
            if !obj.is_empty() {
                return true;
            }
        }
    }
    false
}

fn is_safe_rel_path(path: &str) -> bool {
    if path.is_empty() || path.starts_with('/') {
        return false;
    }
    for comp in Path::new(path).components() {
        match comp {
            Component::Normal(seg) => {
                if seg == ".git" {
                    return false;
                }
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return false,
        }
    }
    true
}

fn paths_match_locked(candidate_norm: &str, locked_norm: &str) -> bool {
    candidate_norm == locked_norm
        || candidate_norm.ends_with(&format!("/{locked_norm}"))
        || locked_norm.ends_with(&format!("/{candidate_norm}"))
}

fn file_diff_touches_path(file: &FileDiff, target_norm: &str) -> bool {
    let candidates = [
        Some(file.path.as_str()),
        file.old_path.as_deref(),
        file.new_path.as_deref(),
    ];
    candidates
        .into_iter()
        .flatten()
        .map(normalize_rel_path)
        .any(|p| paths_match_locked(&p, target_norm))
}

fn file_diff_only_matches(file: &FileDiff, expected_norm: &str) -> bool {
    let p = normalize_rel_path(&file.path);
    if !paths_match_locked(&p, expected_norm) {
        return false;
    }
    if let Some(ref op) = file.old_path {
        let on = normalize_rel_path(op);
        if on != "/dev/null" && !paths_match_locked(&on, expected_norm) {
            return false;
        }
    }
    if let Some(ref np) = file.new_path {
        let nn = normalize_rel_path(np);
        if nn != "/dev/null" && !paths_match_locked(&nn, expected_norm) {
            return false;
        }
    }
    true
}

fn apply_text_replacement(
    original: &str,
    old_text: &str,
    new_text: &str,
) -> Result<String, String> {
    if old_text.is_empty() {
        return Err("old_text must not be empty".to_string());
    }
    let exact_count = original.matches(old_text).count();
    if exact_count == 1 {
        return Ok(original.replacen(old_text, new_text, 1));
    }
    if exact_count > 1 {
        return Err(format!(
            "old_text matched {exact_count} locations; include 2-3 surrounding context lines to disambiguate"
        ));
    }

    // Fallback 1: normalize CRLF -> LF.
    let norm_orig = original.replace("\r\n", "\n");
    let norm_old = old_text.replace("\r\n", "\n");
    let norm_new = new_text.replace("\r\n", "\n");
    if norm_orig.matches(&norm_old).count() == 1 {
        return Ok(norm_orig.replacen(&norm_old, &norm_new, 1));
    }

    // Fallback 2: line-by-line trailing-whitespace-tolerant unique match.
    let orig_lines: Vec<&str> = norm_orig.lines().collect();
    let old_lines: Vec<&str> = norm_old.lines().collect();
    if !old_lines.is_empty() && old_lines.len() <= orig_lines.len() {
        let mut match_indices = Vec::new();
        for start in 0..=(orig_lines.len() - old_lines.len()) {
            let matches = (0..old_lines.len()).all(|offset| {
                orig_lines[start + offset].trim_end() == old_lines[offset].trim_end()
            });
            if matches {
                match_indices.push(start);
            }
        }
        if match_indices.len() == 1 {
            let start = match_indices[0];
            let mut out_lines: Vec<String> = Vec::with_capacity(orig_lines.len());
            out_lines.extend(orig_lines[..start].iter().map(|s| (*s).to_string()));
            out_lines.extend(norm_new.lines().map(|s| s.to_string()));
            out_lines.extend(
                orig_lines[start + old_lines.len()..]
                    .iter()
                    .map(|s| (*s).to_string()),
            );
            let mut joined = out_lines.join("\n");
            if norm_orig.ends_with('\n') {
                joined.push('\n');
            }
            return Ok(joined);
        }
    }

    // Provide a verbatim nearby snippet from the real file when a line anchor exists so retry loops
    // never have to guess exact indentation or context.
    let anchor = old_lines
        .iter()
        .map(|l| l.trim())
        .find(|l| l.len() >= 8)
        .or_else(|| old_lines.first().map(|l| l.trim()))
        .unwrap_or("");
    let hint = if !anchor.is_empty() {
        orig_lines
            .iter()
            .position(|l| l.trim().contains(anchor))
            .map(|idx| {
                let s = idx.saturating_sub(2);
                let e = (idx + 10).min(orig_lines.len());
                format!(
                    "\nClosest verbatim snippet in file (lines {}-{}):\n{}",
                    s + 1,
                    e,
                    orig_lines[s..e].join("\n")
                )
            })
            .unwrap_or_default()
    } else {
        String::new()
    };

    Err(format!(
        "old_text not found verbatim in file (first line: {:?}){hint}",
        old_text.lines().next().unwrap_or("")
    ))
}

/// Strictly filters repository files to genuine runtime-executable regression tests so
/// static type-checking scripts (e.g. `tests/typing/typing_*.py`), package initializers
/// (`tests/.../__init__.py`), fixtures, and benchmarks are never passed directly to `pytest`
/// or `vitest`/`jest`.
pub(crate) fn is_runtime_regression_test(runtime: RepoRuntime, path: &str) -> bool {
    let norm = normalize_rel_path(path);
    if !is_test_like_path(&norm) {
        return false;
    }
    let lower = norm.to_ascii_lowercase();
    let file = lower.rsplit('/').next().unwrap_or(&lower);

    // Exclude static type-check suites, benchmarks, fixtures, docs, and helper directories.
    const EXCLUDED_DIRS: &[&str] = &[
        "typing/",
        "/typing/",
        "typecheck/",
        "/typecheck/",
        "type_checks/",
        "/type_checks/",
        "mypy/",
        "/mypy/",
        "benchmarks/",
        "/benchmarks/",
        "bench/",
        "/bench/",
        "fixtures/",
        "/fixtures/",
        "helpers/",
        "/helpers/",
        "docs/",
        "/docs/",
    ];
    if EXCLUDED_DIRS.iter().any(|seg| lower.contains(seg)) {
        return false;
    }

    match runtime {
        RepoRuntime::Python => {
            if !lower.ends_with(".py") || file == "__init__.py" || file == "conftest.py" {
                return false;
            }
            // Match pytest's default test module discovery convention.
            file.starts_with("test_") || file.ends_with("_test.py")
        }
        RepoRuntime::NodeTs => {
            if lower.ends_with(".d.ts") {
                return false;
            }
            let has_js_ext = lower.ends_with(".ts")
                || lower.ends_with(".tsx")
                || lower.ends_with(".js")
                || lower.ends_with(".jsx")
                || lower.ends_with(".mjs")
                || lower.ends_with(".cjs");
            has_js_ext && (file.contains(".test.") || file.contains(".spec."))
        }
        _ => false,
    }
}

fn detect_broken_test_or_env_error(
    runtime: RepoRuntime,
    command: &str,
    exit_code: Option<i32>,
    combined_output: &str,
) -> Option<String> {
    if let Some(code) = exit_code {
        if code == 126 || code == 127 {
            return Some(format!("command not found or not executable (exit {code})"));
        }
        if runtime == RepoRuntime::Python {
            let lower_cmd = command.to_ascii_lowercase();
            let is_pytest = lower_cmd.contains("pytest");
            if is_pytest && matches!(code, 2..=5) {
                return Some(format!(
                    "pytest exited with error/collection code {code} (expected exit code 1 for genuine test assertion failure)"
                ));
            }
        }
    }

    const BROKEN_MARKERS: &[&str] = &[
        "SyntaxError",
        "IndentationError",
        "ImportError",
        "ModuleNotFoundError",
        "INTERNALERROR",
        "collected 0 items",
        "file or directory not found",
        "command not found",
        "ERROR collecting",
        "No module named",
        "Cannot find module",
        "No test files found",
    ];

    for marker in BROKEN_MARKERS {
        if combined_output.contains(marker) {
            return Some(format!("output contains '{marker}'"));
        }
    }
    None
}

async fn apply_patch_in_dir(repo_dir: &Path, patch: &str) -> anyhow::Result<CommandOutput> {
    let tmp = TempDir::new()?;
    let patch_file = tmp.path().join("epicenter.patch");
    tokio::fs::write(&patch_file, ensure_trailing_newline(patch)).await?;
    apply_patch_file_in_dir(repo_dir, &patch_file).await
}

async fn apply_patch_file_in_dir(
    repo_dir: &Path,
    patch_file: &Path,
) -> anyhow::Result<CommandOutput> {
    run_git(
        repo_dir,
        &[
            "-C",
            &path_str(repo_dir),
            "apply",
            "--whitespace=nowarn",
            &path_str(patch_file),
        ],
        Duration::from_secs(15),
    )
    .await
}

async fn reset_worktree(repo_dir: &Path) -> anyhow::Result<()> {
    let checkout = run_git(
        repo_dir,
        &["-C", &path_str(repo_dir), "checkout", "--", "."],
        Duration::from_secs(15),
    )
    .await?;
    if checkout.exit_code != Some(0) {
        anyhow::bail!(
            "git checkout -- . failed: {}",
            tail_chars(&checkout.combined_output, 240)
        );
    }
    let clean = run_git(
        repo_dir,
        &["-C", &path_str(repo_dir), "clean", "-fd", "-e", "node_modules"],
        Duration::from_secs(15),
    )
    .await?;
    if clean.exit_code != Some(0) {
        anyhow::bail!(
            "git clean -fd failed: {}",
            tail_chars(&clean.combined_output, 240)
        );
    }
    Ok(())
}

async fn run_git(cwd: &Path, args: &[&str], timeout: Duration) -> anyhow::Result<CommandOutput> {
    run_program(cwd, "git", args, &[("GIT_TERMINAL_PROMPT".to_string(), "0".to_string())], timeout)
        .await
}

async fn run_shell_command(
    cwd: &Path,
    command: &str,
    env: &[(String, String)],
    timeout: Duration,
) -> anyhow::Result<CommandOutput> {
    run_program(cwd, "sh", &["-c", command], env, timeout).await
}

async fn run_program(
    cwd: &Path,
    program: &str,
    args: &[&str],
    env: &[(String, String)],
    timeout: Duration,
) -> anyhow::Result<CommandOutput> {
    let started = Instant::now();
    let mut cmd = Command::new(program);
    cmd.current_dir(cwd)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    for (k, v) in env {
        cmd.env(k, v);
    }

    let child = cmd.spawn()?;
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => {
            let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let combined_output = if stderr.is_empty() {
                stdout.clone()
            } else if stdout.is_empty() {
                stderr.clone()
            } else {
                format!("{stdout}\n{stderr}")
            };
            Ok(CommandOutput {
                exit_code: output.status.code(),
                timed_out: false,
                stdout,
                stderr,
                combined_output,
                elapsed_ms,
            })
        }
        Ok(Err(err)) => Err(err.into()),
        Err(_) => Ok(CommandOutput {
            exit_code: None,
            timed_out: true,
            stdout: String::new(),
            stderr: format!("Command timed out after {}s", timeout.as_secs()),
            combined_output: format!("Command timed out after {}s", timeout.as_secs()),
            elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
        }),
    }
}

fn ensure_trailing_newline(s: &str) -> String {
    let norm = s.replace("\r\n", "\n");
    if norm.is_empty() || norm.ends_with('\n') {
        norm
    } else {
        format!("{norm}\n")
    }
}

fn path_str(p: &Path) -> String {
    p.to_string_lossy().to_string()
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn tail_chars(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.trim().to_string();
    }
    let mut start = s.len().saturating_sub(max_bytes);
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    s[start..].trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use epicenter_common::{MediaKind, MultimodalFinding};
    use std::process::Command as StdCommand;

    /// Helper: initializes a local git repository on disk with the given files and returns
    /// `(TempDir, repo_path_string, head_commit_sha)`.
    fn create_local_git_repo(files: &[(&str, &str)]) -> (TempDir, String, String) {
        let dir = tempfile::Builder::new()
            .prefix("epicenter-test-repo-")
            .tempdir()
            .expect("create tempdir");
        let root = dir.path();

        let status = StdCommand::new("git")
            .args(["init", "-q"])
            .current_dir(root)
            .status()
            .expect("git init");
        assert!(status.success());

        for (k, v) in [("user.email", "test@epicenter.dev"), ("user.name", "Epicenter Test")] {
            let st = StdCommand::new("git")
                .args(["config", k, v])
                .current_dir(root)
                .status()
                .expect("git config");
            assert!(st.success());
        }

        for (rel, content) in files {
            let full = root.join(rel);
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent).expect("create parent dirs");
            }
            std::fs::write(&full, content).expect("write repo file");
        }

        let st = StdCommand::new("git")
            .args(["add", "."])
            .current_dir(root)
            .status()
            .expect("git add");
        assert!(st.success());

        let st = StdCommand::new("git")
            .args(["commit", "-q", "-m", "initial commit"])
            .current_dir(root)
            .status()
            .expect("git commit");
        assert!(st.success());

        let sha_out = StdCommand::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(root)
            .output()
            .expect("git rev-parse");
        assert!(sha_out.status.success());
        let sha = String::from_utf8(sha_out.stdout)
            .expect("utf8 sha")
            .trim()
            .to_string();

        let url = root.to_string_lossy().to_string();
        (dir, url, sha)
    }

    fn sample_buggy_python_repo() -> (TempDir, String, String) {
        create_local_git_repo(&[
            (
                "pyproject.toml",
                "[build-system]\nrequires = [\"setuptools\"]\nbuild-backend = \"setuptools.build_meta\"\n\n[project]\nname = \"calc\"\nversion = \"0.1.0\"\n\n[tool.setuptools.packages.find]\nwhere = [\"src\"]\n",
            ),
            ("src/calc/__init__.py", "from .core import add, discount_price, sentinel\n"),
            (
                "src/calc/core.py",
                "def add(a: int, b: int) -> int:\n\
                 \x20   return a + b\n\
                 \n\
                 def discount_price(price: float, pct: float) -> float:\n\
                 \x20   # BUG: applies pct without clamping negative percentages or converting 0..100 properly\n\
                 \x20   if pct < 0:\n\
                 \x20       return price\n\
                 \x20   return price - pct\n\
                 \n\
                 def sentinel() -> int:\n\
                 \x20   return 42\n",
            ),
            (
                "tests/test_core.py",
                "from calc.core import add, sentinel\n\
                 \n\
                 def test_add():\n\
                 \x20   assert add(2, 3) == 5\n\
                 \n\
                 def test_sentinel():\n\
                 \x20   assert sentinel() == 42\n",
            ),
        ])
    }

    #[tokio::test]
    async fn runtime_detection_identifies_python_nodets_rust_go_and_never_defaults_ts_to_pytest() {
        let cache = TempDir::new().unwrap();
        let verifier = HostVerifier::with_cache_root(cache.path().to_path_buf());

        let (_py_dir, py_url, py_sha) = create_local_git_repo(&[
            ("pyproject.toml", "[project]\nname = \"demo\"\nversion = \"0.1.0\"\n"),
            ("app.py", "def f(): return 1\n"),
        ]);
        let py_prep = verifier
            .prepare_checkout(&py_url, Some(&py_sha), None)
            .await
            .unwrap();
        assert_eq!(py_prep.runtime, RepoRuntime::Python);
        assert!(py_prep.default_test_command(None).contains("pytest"));

        let (_ts_dir, ts_url, ts_sha) = create_local_git_repo(&[
            (
                "package.json",
                "{\"name\":\"ts-app\",\"devDependencies\":{\"vitest\":\"^2.0.0\"},\"scripts\":{\"test\":\"vitest run\"}}",
            ),
            ("src/index.ts", "export const x = 1;\n"),
        ]);
        let ts_prep = verifier
            .prepare_checkout(&ts_url, Some(&ts_sha), None)
            .await
            .unwrap();
        assert_eq!(ts_prep.runtime, RepoRuntime::NodeTs);
        assert_eq!(ts_prep.node_runner, NodeTestRunner::Vitest);
        assert!(!ts_prep.default_test_command(None).contains("pytest"));
        assert!(ts_prep
            .default_test_command(Some("tests/foo.test.ts"))
            .contains("vitest"));
        // Sanitizing an accidentally proposed `pytest` command MUST override it for TS!
        let sanitized = ts_prep.sanitize_test_command("pytest -q tests/foo.test.ts", Some("tests/foo.test.ts"));
        assert!(!sanitized.contains("pytest"));
        assert!(sanitized.contains("vitest"));

        let (_rs_dir, rs_url, rs_sha) = create_local_git_repo(&[
            ("Cargo.toml", "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
            ("src/lib.rs", "pub fn ok() -> bool { true }\n"),
        ]);
        let rs_prep = verifier
            .prepare_checkout(&rs_url, Some(&rs_sha), None)
            .await
            .unwrap();
        assert_eq!(rs_prep.runtime, RepoRuntime::Rust);
        assert_eq!(rs_prep.default_test_command(None), "cargo test");
        assert_eq!(rs_prep.sanitize_test_command("pytest -q", None), "cargo test");

        let (_go_dir, go_url, go_sha) = create_local_git_repo(&[
            ("go.mod", "module example.com/demo\n\ngo 1.22\n"),
            ("main.go", "package main\nfunc main() {}\n"),
        ]);
        let go_prep = verifier
            .prepare_checkout(&go_url, Some(&go_sha), None)
            .await
            .unwrap();
        assert_eq!(go_prep.runtime, RepoRuntime::Go);
        assert_eq!(go_prep.default_test_command(None), "go test ./...");
        assert_eq!(go_prep.sanitize_test_command("pytest -q", None), "go test ./...");
    }

    #[tokio::test]
    async fn host_verifier_full_lifecycle_red_gate_candidates_tampering_isolation_and_arena() {
        let (_repo_dir, repo_url, sha) = sample_buggy_python_repo();
        let cache = TempDir::new().unwrap();
        let verifier = HostVerifier::with_cache_root(cache.path().to_path_buf());
        let prepared = verifier
            .prepare_repo(&repo_url, Some(&sha), None)
            .await
            .expect("prepare_repo succeeds");

        assert_eq!(prepared.runtime, RepoRuntime::Python);
        assert!(prepared.env.python_venv_dir.is_some());

        // 1. Local snapshot loading without network
        let snap = prepared
            .load_repo_snapshot(&["src/calc/core.py".to_string()])
            .await
            .unwrap();
        assert!(snap.sources.contains_key("src/calc/core.py"));
        assert!(snap.sources.contains_key("tests/test_core.py"));
        assert!(snap.prompt_test_examples(&["src/calc/core.py".into()], 4000).contains("test_add"));

        // 2. Worktree isolation: edits in worktree A immediately visible in A and NEVER leak to B
        let wt_a = prepared.checkout_worktree().await.unwrap();
        let wt_b = prepared.checkout_worktree().await.unwrap();
        let core_a = wt_a.path().join("src/calc/core.py");
        let orig_core = std::fs::read_to_string(&core_a).unwrap();
        std::fs::write(&core_a, orig_core.replace("return 42", "return 99999")).unwrap();

        let out_a = prepared
            .run_command_in_worktree(
                &wt_a,
                "python -c 'import calc.core; print(calc.core.sentinel())'",
                Duration::from_secs(15),
            )
            .await
            .unwrap();
        let out_b = prepared
            .run_command_in_worktree(
                &wt_b,
                "python -c 'import calc.core; print(calc.core.sentinel())'",
                Duration::from_secs(15),
            )
            .await
            .unwrap();
        assert_eq!(out_a.stdout.trim(), "99999", "worktree A must import its own modified source");
        assert_eq!(out_b.stdout.trim(), "42", "worktree B must remain completely isolated from A");

        let pytest_a = prepared
            .run_command_in_worktree(&wt_a, "pytest -q tests/test_core.py", Duration::from_secs(15))
            .await
            .unwrap();
        let pytest_b = prepared
            .run_command_in_worktree(&wt_b, "pytest -q tests/test_core.py", Duration::from_secs(15))
            .await
            .unwrap();
        assert_eq!(pytest_a.exit_code, Some(1), "pytest in worktree A must observe sentinel=99999 failure");
        assert_eq!(pytest_b.exit_code, Some(0), "pytest in worktree B must still pass");
        drop(wt_a);
        drop(wt_b);

        // 3. Stage 2 Red-Gate: genuine failing repro accepted
        let repro_path = "tests/test_epicenter_repro.py";
        let repro_content = "from calc.core import discount_price\n\n\
            def test_discount_percentage():\n    \
                assert discount_price(200.0, 25.0) == 150.0\n";
        let repro_patch = prepared
            .generate_new_file_patch(repro_path, repro_content)
            .await
            .unwrap();
        let repro_verdict = prepared
            .verify_repro_fails_on_head(repro_path, &repro_patch, "pytest -q tests/test_epicenter_repro.py")
            .await
            .unwrap();
        assert!(repro_verdict.patch_applied);
        assert!(repro_verdict.touches_only_repro_file);
        assert!(repro_verdict.verified_fails_on_head);
        assert_eq!(repro_verdict.exit_code, Some(1));
        assert!(!repro_verdict.is_broken_test_or_env_error);

        // 4. Stage 2 Red-Gate rejects passing test on unmodified HEAD
        let passing_patch = prepared
            .generate_new_file_patch(
                repro_path,
                "from calc.core import add\n\ndef test_always_passes():\n    assert add(1, 2) == 3\n",
            )
            .await
            .unwrap();
        let pass_verdict = prepared
            .verify_repro_fails_on_head(repro_path, &passing_patch, "pytest -q tests/test_epicenter_repro.py")
            .await
            .unwrap();
        assert!(!pass_verdict.verified_fails_on_head);
        assert_eq!(pass_verdict.exit_code, Some(0));

        // 5. Stage 2 Red-Gate rejects SyntaxError, ImportError, and empty/0-collected tests
        let syntax_patch = prepared
            .generate_new_file_patch(repro_path, "def test_bad(:\n    assert False\n")
            .await
            .unwrap();
        let syntax_verdict = prepared
            .verify_repro_fails_on_head(repro_path, &syntax_patch, "pytest -q tests/test_epicenter_repro.py")
            .await
            .unwrap();
        assert!(!syntax_verdict.verified_fails_on_head);
        assert!(syntax_verdict.is_broken_test_or_env_error);

        let import_patch = prepared
            .generate_new_file_patch(
                repro_path,
                "import totally_nonexistent_pkg_98765\n\ndef test_imp():\n    assert False\n",
            )
            .await
            .unwrap();
        let import_verdict = prepared
            .verify_repro_fails_on_head(repro_path, &import_patch, "pytest -q tests/test_epicenter_repro.py")
            .await
            .unwrap();
        assert!(!import_verdict.verified_fails_on_head);
        assert!(import_verdict.is_broken_test_or_env_error);

        let empty_test_patch = prepared
            .generate_new_file_patch(repro_path, "x = 1\n")
            .await
            .unwrap();
        let empty_verdict = prepared
            .verify_repro_fails_on_head(repro_path, &empty_test_patch, "pytest -q tests/test_epicenter_repro.py")
            .await
            .unwrap();
        assert!(!empty_verdict.verified_fails_on_head);
        assert!(empty_verdict.is_broken_test_or_env_error);

        // 6. Stage 2 Red-Gate rejects patches touching production files
        let bad_scope_diff = format!(
            "{}\ndiff --git a/src/calc/core.py b/src/calc/core.py\n--- a/src/calc/core.py\n+++ b/src/calc/core.py\n@@ -1,2 +1,2 @@\n-def add(a: int, b: int) -> int:\n+def add(a: int, b: int) -> int: # edited\n",
            repro_patch
        );
        let scope_verdict = prepared
            .verify_repro_fails_on_head(repro_path, &bad_scope_diff, "pytest -q tests/test_epicenter_repro.py")
            .await
            .unwrap();
        assert!(!scope_verdict.touches_only_repro_file);
        assert!(!scope_verdict.verified_fails_on_head);

        // 7. Stage 4 anti-tampering rejection (both via structured edits AND raw unified diff)
        let tamper_edit = prepared
            .synthesize_diff_from_edits(
                &[FileEdit {
                    path: repro_path.to_string(),
                    old_text: "150.0".to_string(),
                    new_text: "175.0".to_string(),
                }],
                &[],
                Some(repro_path),
            )
            .await
            .unwrap();
        assert!(tamper_edit.tampered);

        let tamper_raw_diff = "diff --git a/tests/test_epicenter_repro.py b/tests/test_epicenter_repro.py\n\
            --- a/tests/test_epicenter_repro.py\n\
            +++ b/tests/test_epicenter_repro.py\n\
            @@ -1,3 +1,3 @@\n\
            -    assert discount_price(200.0, 25.0) == 150.0\n\
            +    assert True\n";
        let tamper_verdict = prepared
            .verify_candidate_patch(
                tamper_raw_diff,
                Some(repro_path),
                &repro_patch,
                "pytest -q tests/test_epicenter_repro.py",
            )
            .await
            .unwrap();
        assert!(tamper_verdict.tampered);
        assert!(!tamper_verdict.repro_passed);
        assert!(!tamper_verdict.regression_suite_passed);

        // 8. Valid fix passes both locked repro AND module regression suite with exit code 0
        let valid_synth = prepared
            .synthesize_diff_from_edits(
                &[FileEdit {
                    path: "src/calc/core.py".to_string(),
                    old_text: "    return price - pct".to_string(),
                    new_text: "    return price * (1.0 - pct / 100.0)".to_string(),
                }],
                &[],
                Some(repro_path),
            )
            .await
            .unwrap();
        assert!(!valid_synth.tampered);
        assert!(valid_synth.errors.is_empty());
        assert!(valid_synth.git_diff.contains("price * (1.0 - pct / 100.0)"));

        let valid_verdict = prepared
            .verify_candidate_patch(
                &valid_synth.git_diff,
                Some(repro_path),
                &repro_patch,
                "pytest -q tests/test_epicenter_repro.py",
            )
            .await
            .unwrap();
        assert!(!valid_verdict.tampered);
        assert!(valid_verdict.patch_applied);
        assert!(valid_verdict.repro_passed);
        assert!(valid_verdict.regression_suite_passed);
        assert!(valid_verdict.test_output_tail.contains("passed"));

        // 9. Broken fix fails with real captured failure output (breaks regression test!)
        let broken_synth = prepared
            .synthesize_diff_from_edits(
                &[
                    FileEdit {
                        path: "src/calc/core.py".to_string(),
                        old_text: "    return price - pct".to_string(),
                        new_text: "    return price * (1.0 - pct / 100.0)".to_string(),
                    },
                    FileEdit {
                        path: "src/calc/core.py".to_string(),
                        old_text: "    return a + b".to_string(),
                        new_text: "    return a - b".to_string(),
                    },
                ],
                &[],
                Some(repro_path),
            )
            .await
            .unwrap();
        let broken_verdict = prepared
            .verify_candidate_patch(
                &broken_synth.git_diff,
                Some(repro_path),
                &repro_patch,
                "pytest -q tests/test_epicenter_repro.py",
            )
            .await
            .unwrap();
        assert!(broken_verdict.repro_passed);
        assert!(!broken_verdict.regression_suite_passed);
        assert!(broken_verdict.test_output_tail.contains("FAILED") || broken_verdict.test_output_tail.contains("AssertionError"));

        // 10. Ground-truth grading API exposed for downstream crates
        let gt_ok = prepared
            .grade_against_ground_truth(&valid_synth.git_diff, &repro_patch)
            .await
            .unwrap();
        assert!(gt_ok.patch_applied);
        assert!(gt_ok.resolved_ground_truth);
        assert!(gt_ok.regression_free);
        assert!(gt_ok.coverage_map.is_some());

        let gt_tamper = prepared
            .grade_against_ground_truth(tamper_raw_diff, &repro_patch)
            .await
            .unwrap();
        assert!(gt_tamper.tampered_with_ground_truth);
        assert!(!gt_tamper.resolved_ground_truth);

        // 11. Single-Host Interleaved Arena produces real Instant timings & genuine CoverageMap
        let bench_spec = BenchmarkHarnessSpec {
            name: "calc discount microbench".to_string(),
            command: "python bench.py".to_string(),
            setup_command: None,
            trials: 3,
            warmup_trials: 1,
            metric_unit: "ms".to_string(),
            entrypoint_contract: None,
            script_content: Some(
                "from calc.core import discount_price\n\
                 total = 0.0\n\
                 for i in range(5000):\n\
                     total += discount_price(200.0, 25.0)\n\
                 assert total > 0\n"
                    .to_string(),
            ),
        };
        let (tx, mut rx) = broadcast::channel(32);
        let sid = Uuid::new_v4();
        let arena_res = prepared
            .run_interleaved_arena(
                &bench_spec,
                Some(repro_path),
                &repro_patch,
                "pytest -q tests/test_epicenter_repro.py",
                &[ArenaCandidateInput {
                    strategy_id: "s1-fix".to_string(),
                    git_diff: valid_synth.git_diff.clone(),
                }],
                Some(sid),
                Some(&tx),
            )
            .await
            .unwrap();

        assert_eq!(arena_res.trials_run, 3);
        assert_eq!(arena_res.baseline_samples_ms.len(), 3);
        assert!(arena_res.baseline_samples_ms.iter().all(|&v| v > 0.0 && v.is_finite()));
        assert_eq!(arena_res.candidates.len(), 1);
        let c0 = &arena_res.candidates[0];
        assert_eq!(c0.candidate_samples_ms.len(), 3);
        assert!(c0.candidate_samples_ms.iter().all(|&v| v > 0.0 && v.is_finite()));

        // Verify genuine CoverageMap captured for src/calc/core.py
        let cov = c0.coverage_map.as_ref().expect("real coverage map captured");
        let pct = cov
            .symbol_coverage_pct("src/calc/core.py", 1, 11)
            .expect("core.py coverage present");
        assert!(pct > 0.0, "expected positive coverage for src/calc/core.py, got {pct}");

        // Verify live trial events streamed
        let mut streamed_trials = 0usize;
        while let Ok(evt) = rx.try_recv() {
            if matches!(evt, LiveEvent::ArenaTrialSample { .. }) {
                streamed_trials += 1;
            }
        }
        assert_eq!(streamed_trials, 3);

        // 12. Multimodal findings formatting helper
        assert_eq!(crate::intake::format_multimodal_findings(&[]), "None");
        let formatted = crate::intake::format_multimodal_findings(&[MultimodalFinding {
            source_name: "trace.mp4".into(),
            media_kind: MediaKind::ScreenRecording,
            timestamp_hint: Some("00:12".into()),
            summary: "Discount displays $175 instead of $150".into(),
            extracted_symbols: vec!["discount_price".into()],
        }]);
        assert!(formatted.contains("trace.mp4"));
        assert!(formatted.contains("00:12"));
        assert!(formatted.contains("discount_price"));
    }

    #[tokio::test]
    async fn node_repo_verifies_red_gate_and_fix_using_node_test_never_pytest() {
        let (_dir, repo_url, sha) = create_local_git_repo(&[
            (
                "package.json",
                "{\"name\":\"js-demo\",\"type\":\"module\",\"scripts\":{\"test\":\"node --test\"}}\n",
            ),
            (
                "src/math.mjs",
                "export function clamp(val, min, max) {\n  return val < min ? min : val;\n}\n\
                 export function double(n) {\n  return n * 2;\n}\n",
            ),
            (
                "test/double.test.mjs",
                "import test from 'node:test';\nimport assert from 'node:assert/strict';\nimport { double } from '../src/math.mjs';\n\
                 test('double works', () => { assert.equal(double(4), 8); });\n",
            ),
        ]);

        let cache = TempDir::new().unwrap();
        let verifier = HostVerifier::with_cache_root(cache.path().to_path_buf());
        let prepared = verifier
            .prepare_repo(&repo_url, Some(&sha), None)
            .await
            .unwrap();
        assert_eq!(prepared.runtime, RepoRuntime::NodeTs);
        assert_eq!(prepared.node_runner, NodeTestRunner::NodeTest);

        let repro_path = "test/clamp_repro.test.mjs";
        let repro_content = "import test from 'node:test';\nimport assert from 'node:assert/strict';\nimport { clamp } from '../src/math.mjs';\n\
            test('clamp enforces upper bound', () => { assert.equal(clamp(15, 0, 10), 10); });\n";
        let repro_patch = prepared
            .generate_new_file_patch(repro_path, repro_content)
            .await
            .unwrap();

        // Even if an accidental `pytest` command is passed, sanitizer overrides it to `node --test`!
        let rg = prepared
            .verify_repro_fails_on_head(repro_path, &repro_patch, "pytest -q")
            .await
            .unwrap();
        assert!(rg.command_used.starts_with("node --test"));
        assert!(rg.verified_fails_on_head);

        let fix = prepared
            .synthesize_diff_from_edits(
                &[FileEdit {
                    path: "src/math.mjs".into(),
                    old_text: "  return val < min ? min : val;".into(),
                    new_text: "  return val < min ? min : (val > max ? max : val);".into(),
                }],
                &[],
                Some(repro_path),
            )
            .await
            .unwrap();
        assert!(fix.errors.is_empty());

        let verdict = prepared
            .verify_candidate_patch(&fix.git_diff, Some(repro_path), &repro_patch, &rg.command_used)
            .await
            .unwrap();
        assert!(verdict.repro_passed);
        assert!(verdict.regression_suite_passed);
    }

    #[tokio::test]
    async fn concurrent_prepare_repo_shares_locks_without_git_or_venv_races() {
        let (_dir, repo_url, sha) = sample_buggy_python_repo();
        let cache = TempDir::new().unwrap();
        let verifier = HostVerifier::with_cache_root(cache.path().to_path_buf());

        let handles: Vec<_> = (0..4)
            .map(|_| {
                let v = verifier.clone();
                let u = repo_url.clone();
                let s = sha.clone();
                tokio::spawn(async move { v.prepare_repo(&u, Some(&s), None).await })
            })
            .collect();

        for h in handles {
            let prep = h.await.unwrap().expect("concurrent prepare_repo succeeds");
            assert_eq!(prep.resolved_sha, sha);
            assert!(prep.env.python_venv_dir.is_some());
        }
    }
}

