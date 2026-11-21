use anyhow::{anyhow, bail, Context, Result};
use epicenter_common::{CreatePrRequest, CreatePrResponse};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, USER_AGENT};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use uuid::Uuid;

const GITHUB_API_BASE: &str = "https://api.github.com";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoSlug {
    pub owner: String,
    pub repo: String,
}

impl RepoSlug {
    pub fn canonical_url(&self) -> String {
        format!("https://github.com/{}/{}", self.owner, self.repo)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrSlug {
    pub owner: String,
    pub repo: String,
    pub number: u64,
}

impl PrSlug {
    pub fn repo_slug(&self) -> RepoSlug {
        RepoSlug {
            owner: self.owner.clone(),
            repo: self.repo.clone(),
        }
    }
}

/// Parses a GitHub repository URL or `owner/repo` slug into `(owner, repo)`.
pub fn parse_github_repo_url(repo_url: &str) -> Result<RepoSlug> {
    let trimmed = repo_url.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        bail!("repository URL is empty");
    }

    let path_part = if let Some(rest) = trimmed.strip_prefix("git@github.com:") {
        rest
    } else if let Some(idx) = trimmed.find("github.com/") {
        &trimmed[idx + "github.com/".len()..]
    } else if !trimmed.contains("://") && trimmed.matches('/').count() == 1 {
        trimmed
    } else {
        bail!("unsupported GitHub repository URL: '{repo_url}'");
    };

    let mut segments = path_part.split('/').filter(|s| !s.is_empty());
    let owner = segments
        .next()
        .ok_or_else(|| anyhow!("missing owner in repo URL '{repo_url}'"))?;
    let raw_repo = segments
        .next()
        .ok_or_else(|| anyhow!("missing repo name in repo URL '{repo_url}'"))?;
    let repo = raw_repo.strip_suffix(".git").unwrap_or(raw_repo);

    if owner.is_empty() || repo.is_empty() {
        bail!("invalid GitHub repository slug in '{repo_url}'");
    }

    Ok(RepoSlug {
        owner: owner.to_string(),
        repo: repo.to_string(),
    })
}

/// Parses a GitHub Pull Request URL (`https://github.com/{owner}/{repo}/pull/{number}[/...]`).
pub fn parse_github_pr_url(pr_url: &str) -> Result<PrSlug> {
    let trimmed = pr_url.trim();
    let without_fragment = trimmed.split('#').next().unwrap_or(trimmed);
    let without_query = without_fragment
        .split('?')
        .next()
        .unwrap_or(without_fragment)
        .trim_end_matches('/');

    let idx = without_query
        .find("github.com/")
        .ok_or_else(|| anyhow!("PR URL must point to github.com: '{pr_url}'"))?;
    let path_part = &without_query[idx + "github.com/".len()..];
    let parts: Vec<&str> = path_part.split('/').filter(|s| !s.is_empty()).collect();

    if parts.len() < 4 || (parts[2] != "pull" && parts[2] != "pulls") {
        bail!(
            "expected GitHub PR URL like https://github.com/owner/repo/pull/123, got '{pr_url}'"
        );
    }

    let number: u64 = parts[3]
        .parse()
        .with_context(|| format!("invalid PR number '{}' in '{pr_url}'", parts[3]))?;

    Ok(PrSlug {
        owner: parts[0].to_string(),
        repo: parts[1].to_string(),
        number,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffLine {
    Context(String),
    Remove(String),
    Add(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffHunk {
    pub old_start: usize,
    pub old_count: usize,
    pub new_start: usize,
    pub new_count: usize,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePatch {
    pub old_path: Option<String>,
    pub new_path: Option<String>,
    pub is_new_file: bool,
    pub is_deleted_file: bool,
    pub hunks: Vec<DiffHunk>,
}

impl FilePatch {
    pub fn target_path(&self) -> Option<&str> {
        if self.is_deleted_file {
            self.old_path.as_deref()
        } else {
            self.new_path.as_deref().or(self.old_path.as_deref())
        }
    }
}

fn strip_diff_prefix(path: &str) -> Option<String> {
    let cleaned = path.trim().trim_matches('"');
    if cleaned == "/dev/null" || cleaned.is_empty() {
        return None;
    }
    let stripped = cleaned
        .strip_prefix("a/")
        .or_else(|| cleaned.strip_prefix("b/"))
        .unwrap_or(cleaned);
    Some(stripped.to_string())
}

fn parse_hunk_range(token: &str) -> Result<(usize, usize)> {
    let trimmed = token.trim_start_matches(['-', '+']);
    if let Some((start_str, count_str)) = trimmed.split_once(',') {
        let start = start_str
            .parse::<usize>()
            .with_context(|| format!("invalid hunk start '{start_str}'"))?;
        let count = count_str
            .parse::<usize>()
            .with_context(|| format!("invalid hunk count '{count_str}'"))?;
        Ok((start, count))
    } else {
        let start = trimmed
            .parse::<usize>()
            .with_context(|| format!("invalid hunk start '{trimmed}'"))?;
        Ok((start, 1))
    }
}

fn parse_hunk_header(line: &str) -> Result<DiffHunk> {
    // Format: @@ -old_start[,old_count] +new_start[,new_count] @@[ optional header]
    let rest = line
        .strip_prefix("@@ ")
        .ok_or_else(|| anyhow!("invalid hunk header: '{line}'"))?;
    let end_idx = rest
        .find(" @@")
        .ok_or_else(|| anyhow!("unterminated hunk header: '{line}'"))?;
    let range_spec = &rest[..end_idx];
    let mut parts = range_spec.split_whitespace();
    let old_part = parts
        .next()
        .ok_or_else(|| anyhow!("missing old range in hunk header: '{line}'"))?;
    let new_part = parts
        .next()
        .ok_or_else(|| anyhow!("missing new range in hunk header: '{line}'"))?;
    let (old_start, old_count) = parse_hunk_range(old_part)?;
    let (new_start, new_count) = parse_hunk_range(new_part)?;

    Ok(DiffHunk {
        old_start,
        old_count,
        new_start,
        new_count,
        lines: Vec::new(),
    })
}

/// Parses a unified `git diff` into per-file patches and hunks.
pub fn parse_unified_diff(diff: &str) -> Result<Vec<FilePatch>> {
    let mut patches: Vec<FilePatch> = Vec::new();
    let mut current_patch: Option<FilePatch> = None;
    let mut current_hunk: Option<DiffHunk> = None;

    let flush_hunk = |patch: &mut Option<FilePatch>, hunk: &mut Option<DiffHunk>| {
        if let (Some(p), Some(h)) = (patch.as_mut(), hunk.take()) {
            p.hunks.push(h);
        }
    };

    let flush_patch = |patches: &mut Vec<FilePatch>,
                       patch: &mut Option<FilePatch>,
                       hunk: &mut Option<DiffHunk>| {
        flush_hunk(patch, hunk);
        if let Some(p) = patch.take() {
            if p.target_path().is_some() {
                patches.push(p);
            }
        }
    };

    for raw_line in diff.lines() {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);

        if line.starts_with("diff --git ") {
            flush_patch(&mut patches, &mut current_patch, &mut current_hunk);
            let mut parts = line["diff --git ".len()..].split_whitespace();
            let old_p = parts.next().and_then(strip_diff_prefix);
            let new_p = parts.next().and_then(strip_diff_prefix);
            current_patch = Some(FilePatch {
                old_path: old_p,
                new_path: new_p,
                is_new_file: false,
                is_deleted_file: false,
                hunks: Vec::new(),
            });
            continue;
        }

        if line.starts_with("new file mode") {
            if let Some(p) = current_patch.as_mut() {
                p.is_new_file = true;
            }
            continue;
        }

        if line.starts_with("deleted file mode") {
            if let Some(p) = current_patch.as_mut() {
                p.is_deleted_file = true;
            }
            continue;
        }

        if line.starts_with("--- ") {
            if current_patch.is_none() {
                current_patch = Some(FilePatch {
                    old_path: None,
                    new_path: None,
                    is_new_file: false,
                    is_deleted_file: false,
                    hunks: Vec::new(),
                });
            }
            if let Some(p) = current_patch.as_mut() {
                let raw_path = &line[4..];
                let parsed = strip_diff_prefix(raw_path.split('\t').next().unwrap_or(raw_path));
                if parsed.is_none() {
                    p.is_new_file = true;
                } else {
                    p.old_path = parsed;
                }
            }
            continue;
        }

        if line.starts_with("+++ ") {
            if let Some(p) = current_patch.as_mut() {
                let raw_path = &line[4..];
                let parsed = strip_diff_prefix(raw_path.split('\t').next().unwrap_or(raw_path));
                if parsed.is_none() {
                    p.is_deleted_file = true;
                } else {
                    p.new_path = parsed;
                }
            }
            continue;
        }

        if line.starts_with("@@ ") {
            flush_hunk(&mut current_patch, &mut current_hunk);
            if current_patch.is_none() {
                continue;
            }
            current_hunk = Some(parse_hunk_header(line)?);
            continue;
        }

        if let Some(hunk) = current_hunk.as_mut() {
            if let Some(rest) = line.strip_prefix(' ') {
                hunk.lines.push(DiffLine::Context(rest.to_string()));
            } else if let Some(rest) = line.strip_prefix('+') {
                hunk.lines.push(DiffLine::Add(rest.to_string()));
            } else if let Some(rest) = line.strip_prefix('-') {
                hunk.lines.push(DiffLine::Remove(rest.to_string()));
            } else if line.starts_with("\\ No newline at end of file") {
                // Metadata marker; ignore line content.
            } else if line.is_empty() {
                // Some diff formatters strip leading space on blank context lines.
                hunk.lines.push(DiffLine::Context(String::new()));
            }
        }
    }

    flush_patch(&mut patches, &mut current_patch, &mut current_hunk);
    Ok(patches)
}

/// Applies parsed unified diff hunks to `original` file content with fuzzy offset matching.
pub fn apply_hunks(original: &str, hunks: &[DiffHunk]) -> Result<String> {
    if hunks.is_empty() {
        return Ok(original.to_string());
    }

    let had_trailing_newline = original.ends_with('\n') || original.is_empty();
    let mut lines: Vec<String> = if original.is_empty() {
        Vec::new()
    } else {
        original
            .lines()
            .map(|l| l.strip_suffix('\r').unwrap_or(l).to_string())
            .collect()
    };

    let mut cumulative_delta: isize = 0;

    for (hunk_idx, hunk) in hunks.iter().enumerate() {
        let expected_old: Vec<&str> = hunk
            .lines
            .iter()
            .filter_map(|dl| match dl {
                DiffLine::Context(s) | DiffLine::Remove(s) => Some(s.as_str()),
                DiffLine::Add(_) => None,
            })
            .collect();

        let replacement: Vec<String> = hunk
            .lines
            .iter()
            .filter_map(|dl| match dl {
                DiffLine::Context(s) | DiffLine::Add(s) => Some(s.clone()),
                DiffLine::Remove(_) => None,
            })
            .collect();

        // Handle pure addition into an empty/new file.
        if expected_old.is_empty() && lines.is_empty() {
            lines = replacement;
            cumulative_delta += lines.len() as isize;
            continue;
        }

        let approx_start = if hunk.old_start == 0 {
            0usize
        } else {
            ((hunk.old_start as isize - 1) + cumulative_delta).max(0) as usize
        };

        let matched_pos = find_hunk_position(&lines, &expected_old, approx_start).ok_or_else(
            || {
                anyhow!(
                    "failed to apply hunk #{} (old_start={}): context lines did not match target file",
                    hunk_idx + 1,
                    hunk.old_start
                )
            },
        )?;

        let old_len = expected_old.len();
        let new_len = replacement.len();
        lines.splice(matched_pos..(matched_pos + old_len), replacement);
        cumulative_delta += (new_len as isize) - (old_len as isize);
    }

    if lines.is_empty() {
        return Ok(String::new());
    }
    let mut result = lines.join("\n");
    if had_trailing_newline {
        result.push('\n');
    }
    Ok(result)
}

fn find_hunk_position(lines: &[String], expected_old: &[&str], approx_start: usize) -> Option<usize> {
    if expected_old.is_empty() {
        return Some(approx_start.min(lines.len()));
    }
    if expected_old.len() > lines.len() {
        return None;
    }

    let matches_at = |pos: usize| -> bool {
        if pos + expected_old.len() > lines.len() {
            return false;
        }
        expected_old
            .iter()
            .enumerate()
            .all(|(i, expected)| lines[pos + i] == *expected)
    };

    let clamped = approx_start.min(lines.len().saturating_sub(expected_old.len()));
    if matches_at(clamped) {
        return Some(clamped);
    }

    // Fuzzy search outward up to 60 lines in either direction.
    for offset in 1..=60usize {
        if let Some(back) = clamped.checked_sub(offset) {
            if matches_at(back) {
                return Some(back);
            }
        }
        let forward = clamped + offset;
        if forward + expected_old.len() <= lines.len() && matches_at(forward) {
            return Some(forward);
        }
    }

    None
}

/// Applies a unified diff to an in-memory file map (`path -> content`), returning updated files
/// (`Some(new_content)` for added/modified files, `None` for deleted files).
pub fn apply_unified_diff_to_map(
    base_files: &BTreeMap<String, String>,
    diff: &str,
) -> Result<BTreeMap<String, Option<String>>> {
    let patches = parse_unified_diff(diff)?;
    let mut updated = BTreeMap::new();

    for patch in patches {
        let Some(path) = patch.target_path().map(str::to_string) else {
            continue;
        };
        if patch.is_deleted_file {
            updated.insert(path, None);
            continue;
        }
        let original = if patch.is_new_file {
            ""
        } else {
            base_files.get(&path).map(String::as_str).unwrap_or("")
        };
        let patched = apply_hunks(original, &patch.hunks)
            .with_context(|| format!("failed to apply diff to '{path}'"))?;
        updated.insert(path, Some(patched));
    }

    Ok(updated)
}

/// Returns `true` if `path` looks like a unit/integration test or spec file.
pub fn is_test_file(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let filename = lower.rsplit('/').next().unwrap_or(&lower);
    lower.starts_with("tests/")
        || lower.starts_with("test/")
        || lower.contains("/tests/")
        || lower.contains("/test/")
        || lower.contains("/__tests__/")
        || lower.starts_with("__tests__/")
        || lower.contains("/spec/")
        || filename.starts_with("test_")
        || filename.ends_with("_test.py")
        || filename.ends_with("_test.rs")
        || filename.ends_with("_test.go")
        || filename.contains(".test.")
        || filename.contains(".spec.")
}

/// Extracts image, GIF, and video URLs from GitHub issue/PR Markdown & HTML text.
pub fn extract_media_urls(text: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut urls = Vec::new();

    let mut push_if_media = |raw: &str| {
        let candidate = raw
            .trim()
            .trim_matches(|c| matches!(c, '"' | '\'' | '(' | ')' | '<' | '>' | ',' | ';'));
        if !(candidate.starts_with("https://") || candidate.starts_with("http://")) {
            return;
        }
        let lower = candidate.to_ascii_lowercase();
        let path_only = lower.split('?').next().unwrap_or(&lower);
        let is_media = path_only.ends_with(".png")
            || path_only.ends_with(".jpg")
            || path_only.ends_with(".jpeg")
            || path_only.ends_with(".gif")
            || path_only.ends_with(".webp")
            || path_only.ends_with(".svg")
            || path_only.ends_with(".mp4")
            || path_only.ends_with(".mov")
            || path_only.ends_with(".webm")
            || lower.contains("github.com/user-attachments/assets/")
            || lower.contains("user-images.githubusercontent.com/");

        if is_media && seen.insert(candidate.to_string()) {
            urls.push(candidate.to_string());
        }
    };

    // Scan Markdown image syntax `![alt](url)` and HTML `src="url"` plus bare URLs.
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if text[i..].starts_with("https://") || text[i..].starts_with("http://") {
            let start = i;
            while i < bytes.len() {
                let b = bytes[i];
                if b.is_ascii_whitespace() || matches!(b, b')' | b'"' | b'\'' | b'>' | b'<') {
                    break;
                }
                i += 1;
            }
            push_if_media(&text[start..i]);
        } else {
            i += 1;
        }
    }

    urls
}

fn sanitize_branch_segment(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "candidate".to_string()
    } else {
        trimmed.chars().take(32).collect()
    }
}

/// Fetched GitHub Pull Request metadata and separated source/test patches for Live Judge evaluation.
#[derive(Debug, Clone)]
pub struct FetchedPrContext {
    pub slug: PrSlug,
    pub repo_url: String,
    pub title: String,
    pub body: String,
    pub parent_commit: String,
    pub ground_truth_test_patch: String,
    pub modified_source_files: Vec<String>,
    pub base_source_files: BTreeMap<String, String>,
    pub media_urls: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct GitHubRepoMeta {
    default_branch: String,
    #[serde(default)]
    permissions: Option<GitHubPermissions>,
}

#[derive(Debug, Deserialize)]
struct GitHubPermissions {
    #[serde(default)]
    push: bool,
}

#[derive(Debug, Deserialize)]
struct GitHubRefResponse {
    object: GitHubRefObject,
}

#[derive(Debug, Deserialize)]
struct GitHubRefObject {
    sha: String,
}

#[derive(Debug, Deserialize)]
struct GitHubCommitResponse {
    tree: GitHubTreeRef,
}

#[derive(Debug, Deserialize)]
struct GitHubTreeRef {
    sha: String,
}

#[derive(Debug, Deserialize)]
struct GitHubUserResponse {
    login: String,
}

#[derive(Debug, Deserialize)]
struct GitHubCreatedPr {
    html_url: String,
    number: u64,
}

#[derive(Debug, Deserialize)]
struct GitHubPrDetail {
    title: String,
    #[serde(default)]
    body: Option<String>,
    base: GitHubPrBranch,
}

#[derive(Debug, Deserialize)]
struct GitHubPrBranch {
    sha: String,
}

#[derive(Debug, Deserialize)]
struct GitHubPrFile {
    filename: String,
    status: String,
    #[serde(default)]
    patch: Option<String>,
}

#[derive(Debug, Serialize)]
struct GitTreeEntry {
    path: String,
    mode: &'static str,
    #[serde(rename = "type")]
    entry_type: &'static str,
    sha: Option<String>,
}

/// GitHub REST API client supporting Git Blobs/Trees/Commits/Refs PR creation and PR inspection.
#[derive(Clone)]
pub struct GitHubClient {
    http: reqwest::Client,
    token: Option<String>,
}

impl GitHubClient {
    pub fn from_env() -> Self {
        let token = std::env::var("GITHUB_TOKEN")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        Self::new(token)
    }

    pub fn new(token: Option<String>) -> Self {
        let http = reqwest::Client::builder()
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { http, token }
    }

    pub fn has_token(&self) -> bool {
        self.token.as_deref().is_some_and(|t| !t.is_empty())
    }

    pub fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    fn default_headers(&self, accept: &str) -> Result<HeaderMap> {
        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, HeaderValue::from_static("epicenter-server/0.1.0"));
        headers.insert(
            ACCEPT,
            HeaderValue::from_str(accept).context("invalid Accept header")?,
        );
        headers.insert(
            "X-GitHub-Api-Version",
            HeaderValue::from_static("2022-11-28"),
        );
        if let Some(tok) = &self.token {
            let val = HeaderValue::from_str(&format!("Bearer {tok}"))
                .context("invalid GITHUB_TOKEN header value")?;
            headers.insert(AUTHORIZATION, val);
        }
        Ok(headers)
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T> {
        let resp = self
            .http
            .get(url)
            .headers(self.default_headers("application/vnd.github+json")?)
            .send()
            .await
            .with_context(|| format!("GitHub GET failed: {url}"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!("GitHub GET {url} returned {status}: {text}");
        }
        serde_json::from_str(&text)
            .with_context(|| format!("failed to parse GitHub JSON from {url}"))
    }

    async fn post_json<B: Serialize, T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        body: &B,
    ) -> Result<T> {
        let resp = self
            .http
            .post(url)
            .headers(self.default_headers("application/vnd.github+json")?)
            .json(body)
            .send()
            .await
            .with_context(|| format!("GitHub POST failed: {url}"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!("GitHub POST {url} returned {status}: {text}");
        }
        serde_json::from_str(&text)
            .with_context(|| format!("failed to parse GitHub JSON from {url}"))
    }

    async fn fetch_raw_file(
        &self,
        owner: &str,
        repo: &str,
        path: &str,
        git_ref: &str,
    ) -> Result<Option<String>> {
        let encoded_path = path
            .split('/')
            .map(url_encode_segment)
            .collect::<Vec<_>>()
            .join("/");
        let url = format!(
            "{GITHUB_API_BASE}/repos/{owner}/{repo}/contents/{encoded_path}?ref={git_ref}"
        );
        let resp = self
            .http
            .get(&url)
            .headers(self.default_headers("application/vnd.github.raw+json")?)
            .send()
            .await
            .with_context(|| format!("GitHub raw file fetch failed: {url}"))?;
        if resp.status().as_u16() == 404 {
            return Ok(None);
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("GitHub raw file fetch {url} returned {status}: {text}");
        }
        Ok(Some(resp.text().await.unwrap_or_default()))
    }

    /// Creates a branch, commits unified diff hunks via the Git Data API (Blobs/Trees/Commits/Refs),
    /// forks automatically if push permissions are lacking, and opens a GitHub Pull Request.
    pub async fn create_pr_from_diff(
        &self,
        repo_url: &str,
        strategy_id: &str,
        session_id: Uuid,
        git_diff: &str,
        audit_markdown: &str,
        req: &CreatePrRequest,
    ) -> Result<CreatePrResponse> {
        if !self.has_token() {
            bail!("GITHUB_TOKEN is not configured on the server; use the zero-auth Export bundle instead");
        }
        if git_diff.trim().is_empty() {
            bail!("candidate '{strategy_id}' has an empty git diff");
        }

        let slug = parse_github_repo_url(repo_url)?;
        let patches = parse_unified_diff(git_diff)?;
        if patches.is_empty() {
            bail!("no valid file patches found in candidate '{strategy_id}' diff");
        }

        let repo_meta: GitHubRepoMeta = self
            .get_json(&format!("{GITHUB_API_BASE}/repos/{}/{}", slug.owner, slug.repo))
            .await?;
        let target_branch = req
            .target_branch
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(&repo_meta.default_branch)
            .to_string();

        let ref_resp: GitHubRefResponse = self
            .get_json(&format!(
                "{GITHUB_API_BASE}/repos/{}/{}/git/ref/heads/{target_branch}",
                slug.owner, slug.repo
            ))
            .await?;
        let base_sha = ref_resp.object.sha;

        let commit_resp: GitHubCommitResponse = self
            .get_json(&format!(
                "{GITHUB_API_BASE}/repos/{}/{}/git/commits/{base_sha}",
                slug.owner, slug.repo
            ))
            .await?;
        let base_tree_sha = commit_resp.tree.sha;

        // Decide whether we push directly to the repo or fork it first.
        let can_push = repo_meta.permissions.as_ref().is_some_and(|p| p.push);
        let (push_owner, push_repo, use_fork_head) = if can_push {
            (slug.owner.clone(), slug.repo.clone(), false)
        } else {
            let fork_url = format!("{GITHUB_API_BASE}/repos/{}/{}/forks", slug.owner, slug.repo);
            let _fork_resp: Value = self.post_json(&fork_url, &json!({})).await?;
            let me: GitHubUserResponse = self.get_json(&format!("{GITHUB_API_BASE}/user")).await?;
            (me.login, slug.repo.clone(), true)
        };

        // Apply each file patch against base_sha and create Git Blobs.
        let mut tree_entries = Vec::with_capacity(patches.len());
        for patch in &patches {
            let Some(path) = patch.target_path() else {
                continue;
            };
            if patch.is_deleted_file {
                tree_entries.push(GitTreeEntry {
                    path: path.to_string(),
                    mode: "100644",
                    entry_type: "blob",
                    sha: None,
                });
                continue;
            }

            let base_content = if patch.is_new_file {
                String::new()
            } else {
                self.fetch_raw_file(&slug.owner, &slug.repo, path, &base_sha)
                    .await?
                    .unwrap_or_default()
            };
            let new_content = apply_hunks(&base_content, &patch.hunks)
                .with_context(|| format!("failed to apply patch hunks for '{path}'"))?;

            let blob_resp: Value = self
                .post_json(
                    &format!("{GITHUB_API_BASE}/repos/{push_owner}/{push_repo}/git/blobs"),
                    &json!({
                        "content": new_content,
                        "encoding": "utf-8"
                    }),
                )
                .await?;
            let blob_sha = blob_resp["sha"]
                .as_str()
                .ok_or_else(|| anyhow!("GitHub blob response missing sha"))?
                .to_string();

            tree_entries.push(GitTreeEntry {
                path: path.to_string(),
                mode: "100644",
                entry_type: "blob",
                sha: Some(blob_sha),
            });
        }

        // Create Git Tree.
        let tree_resp: Value = self
            .post_json(
                &format!("{GITHUB_API_BASE}/repos/{push_owner}/{push_repo}/git/trees"),
                &json!({
                    "base_tree": base_tree_sha,
                    "tree": tree_entries
                }),
            )
            .await?;
        let new_tree_sha = tree_resp["sha"]
            .as_str()
            .ok_or_else(|| anyhow!("GitHub tree response missing sha"))?
            .to_string();

        // Create Git Commit.
        let short_id = &session_id.to_string()[..8];
        let branch_name = format!(
            "epicenter/{}-{}",
            sanitize_branch_segment(strategy_id),
            short_id
        );
        let pr_title = req
            .title
            .clone()
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| format!("Epicenter ({strategy_id}): verified patch"));

        let commit_create_resp: Value = self
            .post_json(
                &format!("{GITHUB_API_BASE}/repos/{push_owner}/{push_repo}/git/commits"),
                &json!({
                    "message": pr_title,
                    "tree": new_tree_sha,
                    "parents": [base_sha]
                }),
            )
            .await?;
        let new_commit_sha = commit_create_resp["sha"]
            .as_str()
            .ok_or_else(|| anyhow!("GitHub commit response missing sha"))?
            .to_string();

        // Create (or update) branch reference.
        let ref_create_url = format!("{GITHUB_API_BASE}/repos/{push_owner}/{push_repo}/git/refs");
        let ref_res = self
            .http
            .post(&ref_create_url)
            .headers(self.default_headers("application/vnd.github+json")?)
            .json(&json!({
                "ref": format!("refs/heads/{branch_name}"),
                "sha": new_commit_sha
            }))
            .send()
            .await
            .context("failed to create GitHub branch ref")?;

        if !ref_res.status().is_success() {
            // If branch already exists (422), force-update it.
            let patch_url = format!(
                "{GITHUB_API_BASE}/repos/{push_owner}/{push_repo}/git/refs/heads/{branch_name}"
            );
            let patch_res = self
                .http
                .patch(&patch_url)
                .headers(self.default_headers("application/vnd.github+json")?)
                .json(&json!({
                    "sha": new_commit_sha,
                    "force": true
                }))
                .send()
                .await
                .context("failed to update existing GitHub branch ref")?;
            if !patch_res.status().is_success() {
                let status = patch_res.status();
                let body = patch_res.text().await.unwrap_or_default();
                bail!("failed to create/update branch '{branch_name}': {status} {body}");
            }
        }

        // Open Pull Request.
        let head = if use_fork_head {
            format!("{push_owner}:{branch_name}")
        } else {
            branch_name.clone()
        };

        let created_pr: GitHubCreatedPr = self
            .post_json(
                &format!("{GITHUB_API_BASE}/repos/{}/{}/pulls", slug.owner, slug.repo),
                &json!({
                    "title": pr_title,
                    "head": head,
                    "base": target_branch,
                    "body": audit_markdown
                }),
            )
            .await?;

        Ok(CreatePrResponse {
            pr_url: created_pr.html_url,
            branch_name,
            number: created_pr.number,
        })
    }

    /// Fetches merged PR metadata, base SHA, separated hidden ground-truth test patch,
    /// extracted media URLs, and base source contents for Live Judge evaluation.
    pub async fn fetch_pr_context(&self, pr_url: &str) -> Result<FetchedPrContext> {
        let slug = parse_github_pr_url(pr_url)?;
        let pr_detail: GitHubPrDetail = self
            .get_json(&format!(
                "{GITHUB_API_BASE}/repos/{}/{}/pulls/{}",
                slug.owner, slug.repo, slug.number
            ))
            .await?;

        let files: Vec<GitHubPrFile> = self
            .get_json(&format!(
                "{GITHUB_API_BASE}/repos/{}/{}/pulls/{}/files?per_page=100",
                slug.owner, slug.repo, slug.number
            ))
            .await?;

        let mut test_patch_buf = String::new();
        let mut modified_source_files = Vec::new();
        let mut base_source_files = BTreeMap::new();

        for file in files {
            if is_test_file(&file.filename) {
                if let Some(patch) = file.patch {
                    let old_header = if file.status == "added" {
                        "/dev/null".to_string()
                    } else {
                        format!("a/{}", file.filename)
                    };
                    let new_header = if file.status == "removed" {
                        "/dev/null".to_string()
                    } else {
                        format!("b/{}", file.filename)
                    };
                    test_patch_buf.push_str(&format!(
                        "diff --git a/{f} b/{f}\n--- {old}\n+++ {new}\n{patch}\n",
                        f = file.filename,
                        old = old_header,
                        new = new_header,
                        patch = patch
                    ));
                }
            } else {
                modified_source_files.push(file.filename.clone());
                // Best-effort fetch of up to 16 base source files for blast-radius graph analysis.
                if base_source_files.len() < 16 && file.status != "added" {
                    if let Ok(Some(content)) = self
                        .fetch_raw_file(&slug.owner, &slug.repo, &file.filename, &pr_detail.base.sha)
                        .await
                    {
                        base_source_files.insert(file.filename, content);
                    }
                }
            }
        }

        let body = pr_detail.body.unwrap_or_default();
        let media_urls = extract_media_urls(&body);

        Ok(FetchedPrContext {
            repo_url: slug.repo_slug().canonical_url(),
            slug,
            title: pr_detail.title,
            body,
            parent_commit: pr_detail.base.sha,
            ground_truth_test_patch: test_patch_buf,
            modified_source_files,
            base_source_files,
            media_urls,
        })
    }
}

fn url_encode_segment(segment: &str) -> String {
    let mut encoded = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            _ => {
                encoded.push_str(&format!("%{:02X}", byte));
            }
        }
    }
    encoded
}
