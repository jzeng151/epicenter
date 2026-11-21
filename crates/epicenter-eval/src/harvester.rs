use crate::verify::{
    check_url_http_200, detect_placeholder_media_url, detect_synthetic_diff_indices,
    is_valid_and_non_synthetic_sha,
};
use anyhow::{anyhow, bail, Context, Result};
use epicenter_common::{EvalIssueCase, EvalSplit};
use epicenter_graph::normalize_rel_path;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, USER_AGENT};
use serde::Deserialize;
use std::collections::HashSet;
use std::time::Duration;
use tracing::{debug, info, warn};

/// Default external Python & TypeScript open-source repositories targeted by the 5-Sigma harvester.
pub const DEFAULT_TARGET_REPOS: &[&str] = &[
    "pallets/click",
    "pallets/flask",
    "pallets/werkzeug",
    "encode/httpx",
    "psf/requests",
    "urllib3/urllib3",
    "pydantic/pydantic",
    "tiangolo/fastapi",
    "colinhacks/zod",
    "honojs/hono",
    "Textualize/rich",
    "pytest-dev/pytest",
];

/// Configuration for batch harvesting merged bug-fix PRs from GitHub.
#[derive(Debug, Clone)]
pub struct HarvestConfig {
    pub repos: Vec<String>,
    pub max_prs_per_repo: usize,
    pub max_total_cases: usize,
    pub calibration_count: usize,
    pub prefer_multimodal: bool,
    pub github_token: Option<String>,
}

impl Default for HarvestConfig {
    fn default() -> Self {
        Self {
            repos: DEFAULT_TARGET_REPOS.iter().map(|s| s.to_string()).collect(),
            max_prs_per_repo: 3,
            max_total_cases: 20,
            calibration_count: 5,
            prefer_multimodal: true,
            github_token: std::env::var("GITHUB_TOKEN")
                .ok()
                .filter(|s| !s.trim().is_empty()),
        }
    }
}

/// Result of splitting a unified PR diff into non-test source hunks vs hidden ground-truth test hunks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitDiffResult {
    pub source_patch: String,
    pub ground_truth_test_patch: String,
    pub source_files: Vec<String>,
    pub test_files: Vec<String>,
}

impl SplitDiffResult {
    pub fn has_both_source_and_tests(&self) -> bool {
        !self.source_files.is_empty() && !self.test_files.is_empty()
    }

    pub fn code_source_files(&self) -> Vec<&String> {
        self.source_files
            .iter()
            .filter(|p| is_production_code_path(p))
            .collect()
    }
}

/// Returns `true` if `path` is a real production source code file (Python, TS/JS, Rust, Go).
pub fn is_production_code_path(path: &str) -> bool {
    let lower = normalize_rel_path(path).to_ascii_lowercase();
    if lower.is_empty() || is_test_file_path(&lower) {
        return false;
    }
    if lower.starts_with("docs/")
        || lower.starts_with(".github/")
        || lower.starts_with("examples/")
        || lower.starts_with("scripts/")
    {
        return false;
    }
    lower.ends_with(".py")
        || lower.ends_with(".ts")
        || lower.ends_with(".tsx")
        || lower.ends_with(".mts")
        || lower.ends_with(".cts")
        || lower.ends_with(".js")
        || lower.ends_with(".jsx")
        || lower.ends_with(".mjs")
        || lower.ends_with(".cjs")
        || lower.ends_with(".rs")
        || lower.ends_with(".go")
}

/// Returns `true` if `path` is a test or spec file that belongs in the hidden ground-truth test patch.
pub fn is_test_file_path(path: &str) -> bool {
    let norm = normalize_rel_path(path).to_ascii_lowercase();
    if norm.is_empty() || norm == "/dev/null" {
        return false;
    }

    let segments: Vec<&str> = norm.split('/').filter(|s| !s.is_empty()).collect();
    if segments.is_empty() {
        return false;
    }

    // Check directory segments (all segments except the filename).
    for &dir in &segments[..segments.len().saturating_sub(1)] {
        if matches!(
            dir,
            "tests" | "test" | "__tests__" | "spec" | "specs" | "testing" | "test_suite"
        ) {
            return true;
        }
    }

    let filename = segments[segments.len() - 1];
    if filename == "conftest.py" {
        return true;
    }
    if filename.starts_with("test_") && filename.ends_with(".py") {
        return true;
    }
    if filename.ends_with("_test.py")
        || filename.ends_with("_test.go")
        || filename.ends_with("_test.rs")
    {
        return true;
    }

    const TS_JS_TEST_SUFFIXES: &[&str] = &[
        ".test.ts",
        ".spec.ts",
        ".test.tsx",
        ".spec.tsx",
        ".test.mts",
        ".spec.mts",
        ".test.js",
        ".spec.js",
        ".test.jsx",
        ".spec.jsx",
        ".test.mjs",
        ".spec.mjs",
        ".test.cjs",
        ".spec.cjs",
    ];
    TS_JS_TEST_SUFFIXES
        .iter()
        .any(|suffix| filename.ends_with(suffix))
}

/// Splits a unified PR diff into `(source_patch, ground_truth_test_patch)` strictly separating
/// test/spec hunks from production source hunks.
pub fn split_pr_diff_source_and_tests(raw_diff: &str) -> SplitDiffResult {
    let mut source_blocks: Vec<String> = Vec::new();
    let mut test_blocks: Vec<String> = Vec::new();
    let mut source_files: Vec<String> = Vec::new();
    let mut test_files: Vec<String> = Vec::new();

    let mut current_block = String::new();
    let mut current_path: Option<String> = None;

    let flush = |block: &mut String,
                 path_opt: &mut Option<String>,
                 source_blocks: &mut Vec<String>,
                 test_blocks: &mut Vec<String>,
                 source_files: &mut Vec<String>,
                 test_files: &mut Vec<String>| {
        if block.trim().is_empty() {
            block.clear();
            *path_opt = None;
            return;
        }
        let path = path_opt.take().unwrap_or_default();
        if path.is_empty() {
            block.clear();
            return;
        }
        if is_test_file_path(&path) {
            test_blocks.push(std::mem::take(block));
            if !test_files.contains(&path) {
                test_files.push(path);
            }
        } else {
            source_blocks.push(std::mem::take(block));
            if !source_files.contains(&path) {
                source_files.push(path);
            }
        }
    };

    for line in raw_diff.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            flush(
                &mut current_block,
                &mut current_path,
                &mut source_blocks,
                &mut test_blocks,
                &mut source_files,
                &mut test_files,
            );
            current_path = extract_diff_git_path(rest);
        } else if current_path.is_none() {
            if let Some(p) = line.strip_prefix("+++ ") {
                let p = p.trim();
                if p != "/dev/null" {
                    current_path = Some(normalize_rel_path(p));
                }
            } else if let Some(p) = line.strip_prefix("--- ") {
                let p = p.trim();
                if p != "/dev/null" {
                    current_path = Some(normalize_rel_path(p));
                }
            }
        }

        current_block.push_str(line);
        current_block.push('\n');
    }

    flush(
        &mut current_block,
        &mut current_path,
        &mut source_blocks,
        &mut test_blocks,
        &mut source_files,
        &mut test_files,
    );

    SplitDiffResult {
        source_patch: source_blocks.concat(),
        ground_truth_test_patch: test_blocks.concat(),
        source_files,
        test_files,
    }
}

fn extract_diff_git_path(header_rest: &str) -> Option<String> {
    let mut parts = header_rest.split_whitespace();
    let old_part = parts.next()?;
    let new_part = parts.next();
    let chosen = match new_part {
        Some(np) if np != "/dev/null" => np,
        _ => old_part,
    };
    let normalized = normalize_rel_path(chosen);
    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

/// Returns `true` if a URL is a badge, shield, avatar, GitHub UI asset, or sponsor banner that
/// must NOT be counted as real bug-report multimodal evidence.
pub fn is_ignored_media_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    const IGNORED_SUBSTRINGS: &[&str] = &[
        "shields.io",
        "img.shields.io",
        "badgen.net",
        "badge.fury.io",
        "codecov.io",
        "coveralls.io",
        "travis-ci.",
        "circleci.com",
        "actions/workflows/",
        "/badge.svg",
        "badge.png",
        "avatars.githubusercontent.com",
        "gravatar.com",
        "github.githubassets.com",
        "assets-cdn.github.com",
        "opengraph.githubassets.com",
        "github.com/images/",
        "github.com/identicons/",
        "opencollective.com",
        "patreon.com",
        "buymeacoffee.com",
        "ko-fi.com",
        "star-history.com",
        "contrib.rocks",
        "sourcery.ai",
        "coderabbit.ai",
        "codecov",
        "favicon",
        "github-logo",
        "logo.svg",
    ];
    if IGNORED_SUBSTRINGS.iter().any(|p| lower.contains(p)) {
        return true;
    }

    // Ignore raw .svg URLs (overwhelmingly badges/icons) unless explicitly hosted on GitHub user attachments.
    let without_query = lower.split(['?', '#']).next().unwrap_or(&lower);
    if without_query.ends_with(".svg")
        && !lower.contains("github.com/user-attachments/assets/")
        && !lower.contains("user-images.githubusercontent.com/")
    {
        return true;
    }

    false
}

/// Extracts candidate image, GIF, diagram, and video URLs from GitHub issue/PR Markdown and HTML,
/// rigorously ignoring badges, shields, avatars, and GitHub logo/UI URLs.
pub fn extract_media_urls(markdown: &str) -> Vec<String> {
    let mut urls: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    let mut push_url = |raw: &str| {
        let cleaned = clean_extracted_url(raw);
        if is_http_url(&cleaned)
            && !is_ignored_media_url(&cleaned)
            && seen.insert(cleaned.clone())
        {
            urls.push(cleaned);
        }
    };

    // 1. Markdown image syntax: ![alt](url) or ![alt](url "title")
    let bytes = markdown.as_bytes();
    let mut idx = 0;
    while idx + 2 < bytes.len() {
        if bytes[idx] == b'!' && bytes[idx + 1] == b'[' {
            if let Some(close_bracket) = markdown[idx + 2..].find(']') {
                let after_bracket = idx + 2 + close_bracket + 1;
                if after_bracket < bytes.len() && bytes[after_bracket] == b'(' {
                    if let Some(close_paren) = markdown[after_bracket + 1..].find(')') {
                        let inner = &markdown[after_bracket + 1..after_bracket + 1 + close_paren];
                        if let Some(first_token) = inner.split_whitespace().next() {
                            push_url(first_token);
                        }
                        idx = after_bracket + 1 + close_paren + 1;
                        continue;
                    }
                }
            }
        }
        idx += 1;
    }

    // 2. HTML <img ... src="..."> / <video ... src="..."> / <source ... src="...">
    let lower = markdown.to_ascii_lowercase();
    for tag in ["<img", "<video", "<source"] {
        let mut search_from = 0;
        while let Some(pos) = lower[search_from..].find(tag) {
            let abs_start = search_from + pos;
            if let Some(tag_end_rel) = lower[abs_start..].find('>') {
                let tag_slice = &markdown[abs_start..abs_start + tag_end_rel];
                let tag_lower = &lower[abs_start..abs_start + tag_end_rel];
                if let Some(src_pos) = tag_lower.find("src=") {
                    let val_start = src_pos + 4;
                    let rest = tag_slice[val_start..].trim_start();
                    let extracted = if let Some(stripped) = rest.strip_prefix('"') {
                        stripped.split('"').next().unwrap_or("")
                    } else if let Some(stripped) = rest.strip_prefix('\'') {
                        stripped.split('\'').next().unwrap_or("")
                    } else {
                        rest.split_whitespace().next().unwrap_or("")
                    };
                    push_url(extracted);
                }
                search_from = abs_start + tag_end_rel + 1;
            } else {
                break;
            }
        }
    }

    // 3. Standalone GitHub user-attachments / asset links or direct image/video URLs.
    for token in markdown.split_whitespace() {
        let trimmed = token.trim_matches(|c: char| {
            matches!(c, '(' | ')' | '[' | ']' | '<' | '>' | '"' | '\'' | ',' | ';')
        });
        if let Some(http_idx) = trimmed
            .find("https://")
            .or_else(|| trimmed.find("http://"))
        {
            let candidate = clean_extracted_url(&trimmed[http_idx..]);
            if looks_like_media_url(&candidate) {
                push_url(&candidate);
            }
        }
    }

    urls
}

fn clean_extracted_url(raw: &str) -> String {
    let trimmed = raw
        .trim()
        .trim_start_matches('<')
        .trim_end_matches(|c: char| {
            matches!(c, ')' | ']' | '>' | '"' | '\'' | ',' | ';' | '.')
        });
    trimmed.to_string()
}

fn is_http_url(s: &str) -> bool {
    s.starts_with("https://") || s.starts_with("http://")
}

fn looks_like_media_url(url: &str) -> bool {
    if !is_http_url(url) || is_ignored_media_url(url) {
        return false;
    }
    let lower = url.to_ascii_lowercase();
    if lower.contains("github.com/user-attachments/assets/")
        || lower.contains("user-images.githubusercontent.com/")
        || lower.contains("private-user-images.githubusercontent.com/")
    {
        return true;
    }
    let without_query = lower.split(['?', '#']).next().unwrap_or(&lower);
    const MEDIA_EXTS: &[&str] = &[
        ".png", ".jpg", ".jpeg", ".gif", ".webp", ".mp4", ".mov", ".webm", ".mkv",
    ];
    MEDIA_EXTS.iter().any(|ext| without_query.ends_with(ext))
}

/// Verifies candidate media URLs over HTTP, keeping ONLY URLs that return HTTP 200 OK with an
/// image/ or video/ Content-Type and no placeholder patterns.
pub async fn verify_reachable_media_urls(
    media_client: &reqwest::Client,
    urls: &[String],
) -> Vec<String> {
    let mut verified = Vec::new();
    for url in urls {
        if is_ignored_media_url(url) || detect_placeholder_media_url(url).is_some() {
            continue;
        }
        match check_url_http_200(media_client, url).await {
            Ok(ct_opt) => {
                let is_media_ct = match ct_opt.as_deref() {
                    Some(ct) => {
                        let lower = ct.to_ascii_lowercase();
                        (lower.starts_with("image/") && !lower.contains("svg"))
                            || lower.starts_with("video/")
                    }
                    None => true,
                };
                if is_media_ct {
                    verified.push(url.clone());
                } else {
                    debug!(
                        url = %url,
                        content_type = ?ct_opt,
                        "discarding non-image/video URL from media_urls"
                    );
                }
            }
            Err(err) => {
                debug!(url = %url, error = %err, "discarding unreachable media URL");
            }
        }
    }
    verified
}

/// Parses a GitHub Pull Request URL into `(owner, repo, pr_number)`.
pub fn parse_pr_url(pr_url: &str) -> Result<(String, String, u64)> {
    let trimmed = pr_url.trim();
    let without_scheme = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .unwrap_or(trimmed);
    let path_only = without_scheme.split(['?', '#']).next().unwrap_or("");
    let segments: Vec<&str> = path_only.split('/').filter(|s| !s.is_empty()).collect();

    let start_idx = if segments
        .first()
        .is_some_and(|s| s.eq_ignore_ascii_case("github.com") || s.eq_ignore_ascii_case("www.github.com"))
    {
        1
    } else {
        bail!("expected a github.com pull request URL, got '{pr_url}'");
    };

    if segments.len() < start_idx + 4 {
        bail!("invalid GitHub PR URL format: '{pr_url}'");
    }
    let owner = segments[start_idx].to_string();
    let repo = segments[start_idx + 1].to_string();
    let kind = segments[start_idx + 2];
    if !kind.eq_ignore_ascii_case("pull") && !kind.eq_ignore_ascii_case("pulls") {
        bail!("expected '/pull/<number>' in GitHub URL, got '{pr_url}'");
    }
    let number: u64 = segments[start_idx + 3]
        .parse()
        .with_context(|| format!("invalid PR number in '{pr_url}'"))?;

    Ok((owner, repo, number))
}

/// Extracts a linked GitHub issue number from keywords (`Fixes #123`, `Closes: #456`, `Resolves #789`)
/// or full GitHub issue URLs (`https://github.com/owner/repo/issues/123`).
pub fn extract_linked_issue_number(body: &str) -> Option<u64> {
    extract_linked_issue_number_for_repo(body, None, None)
}

/// Extracts a linked GitHub issue number, optionally scoped to `owner/repo` when matching full issue URLs.
pub fn extract_linked_issue_number_for_repo(
    body: &str,
    owner: Option<&str>,
    repo: Option<&str>,
) -> Option<u64> {
    let lower = body.to_ascii_lowercase();

    // 1. Keyword-based links: Fixes #123, Closes: #456, Resolves https://github.com/o/r/issues/789
    for keyword in [
        "fixes", "fix", "fixed", "closes", "close", "closed", "resolves", "resolve", "resolved",
    ] {
        let mut offset = 0usize;
        while let Some(rel) = lower[offset..].find(keyword) {
            let abs = offset + rel;
            let prev_ok = abs == 0
                || !lower.as_bytes()[abs - 1].is_ascii_alphanumeric();
            let after_kw = abs + keyword.len();
            let next_ok = after_kw < lower.len()
                && matches!(lower.as_bytes()[after_kw], b' ' | b':' | b'\t');
            if prev_ok && next_ok {
                let rest = lower[after_kw..].trim_start_matches([' ', '\t', ':']);
                if let Some(after_hash) = rest.strip_prefix('#') {
                    let digits: String = after_hash
                        .chars()
                        .take_while(|c| c.is_ascii_digit())
                        .collect();
                    if let Ok(num) = digits.parse::<u64>() {
                        if num > 0 {
                            return Some(num);
                        }
                    }
                } else if rest.starts_with("https://github.com/")
                    || rest.starts_with("http://github.com/")
                {
                    if let Some(num) = parse_issue_url_number(rest, owner, repo) {
                        return Some(num);
                    }
                } else if let (Some(o), Some(r)) = (owner, repo) {
                    let prefix = format!("{}/{}#", o.to_ascii_lowercase(), r.to_ascii_lowercase());
                    if let Some(after_slug) = rest.strip_prefix(&prefix) {
                        let digits: String = after_slug
                            .chars()
                            .take_while(|c| c.is_ascii_digit())
                            .collect();
                        if let Ok(num) = digits.parse::<u64>() {
                            if num > 0 {
                                return Some(num);
                            }
                        }
                    }
                }
            }
            offset = after_kw;
        }
    }

    // 2. Full GitHub issue URL anywhere in the body (scoped to same owner/repo if provided).
    for token in body.split_whitespace() {
        let cleaned = clean_extracted_url(token.trim_matches(|c: char| {
            matches!(c, '(' | ')' | '[' | ']' | '<' | '>' | '"' | '\'' | ',')
        }));
        if let Some(idx) = cleaned
            .find("https://github.com/")
            .or_else(|| cleaned.find("http://github.com/"))
        {
            if let Some(num) = parse_issue_url_number(&cleaned[idx..], owner, repo) {
                return Some(num);
            }
        }
    }

    None
}

fn parse_issue_url_number(url_prefix: &str, owner: Option<&str>, repo: Option<&str>) -> Option<u64> {
    let token = url_prefix
        .split_whitespace()
        .next()
        .unwrap_or("")
        .split(['?', '#'])
        .next()
        .unwrap_or("");
    let without_scheme = token
        .strip_prefix("https://")
        .or_else(|| token.strip_prefix("http://"))?;
    let segs: Vec<&str> = without_scheme.split('/').filter(|s| !s.is_empty()).collect();
    // ["github.com", owner, repo, "issues", number]
    if segs.len() < 5 || !segs[0].eq_ignore_ascii_case("github.com") {
        return None;
    }
    if !segs[3].eq_ignore_ascii_case("issues") {
        return None;
    }
    if let (Some(o), Some(r)) = (owner, repo) {
        if !segs[1].eq_ignore_ascii_case(o) || !segs[2].eq_ignore_ascii_case(r) {
            return None;
        }
    }
    let digits: String = segs[4].chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse::<u64>().ok().filter(|&n| n > 0)
}

/// Infers the default test runner command for a Python, TypeScript, Rust, or Go repository.
pub fn infer_test_command(repo_url: &str, test_files: &[String]) -> String {
    let lower_repo = repo_url.to_ascii_lowercase();
    let is_ts = lower_repo.contains("zod")
        || lower_repo.contains("hono")
        || test_files.iter().any(|f| {
            f.ends_with(".ts")
                || f.ends_with(".tsx")
                || f.ends_with(".js")
                || f.ends_with(".jsx")
                || f.ends_with(".mjs")
        });

    if is_ts {
        if test_files.is_empty() {
            "npx vitest run".to_string()
        } else {
            format!("npx vitest run {}", test_files.join(" "))
        }
    } else if test_files.iter().any(|f| f.ends_with(".rs")) {
        "cargo test".to_string()
    } else if test_files.iter().any(|f| f.ends_with(".go")) {
        "go test ./...".to_string()
    } else if test_files.is_empty() {
        "pytest -q".to_string()
    } else {
        format!("pytest -q {}", test_files.join(" "))
    }
}

/// Constructs an `EvalIssueCase` from fetched PR metadata and raw unified diff.
#[allow(clippy::too_many_arguments)]
pub fn build_case_from_pr_parts(
    owner: &str,
    repo: &str,
    pr_number: u64,
    issue_number: u64,
    title: &str,
    issue_body: &str,
    parent_commit: &str,
    raw_diff: &str,
    split: EvalSplit,
) -> Result<EvalIssueCase> {
    let media_urls = extract_media_urls(issue_body);
    build_case_from_pr_parts_with_media(
        owner,
        repo,
        pr_number,
        issue_number,
        title,
        issue_body,
        parent_commit,
        raw_diff,
        media_urls,
        split,
    )
}

/// Constructs an `EvalIssueCase` using an explicit pre-verified list of `media_urls`.
#[allow(clippy::too_many_arguments)]
pub fn build_case_from_pr_parts_with_media(
    owner: &str,
    repo: &str,
    pr_number: u64,
    issue_number: u64,
    title: &str,
    issue_body: &str,
    parent_commit: &str,
    raw_diff: &str,
    media_urls: Vec<String>,
    split: EvalSplit,
) -> Result<EvalIssueCase> {
    let split_diff = split_pr_diff_source_and_tests(raw_diff);
    if !split_diff.has_both_source_and_tests() {
        bail!(
            "PR {owner}/{repo}#{pr_number} must modify both source ({} files) and test ({} files) files",
            split_diff.source_files.len(),
            split_diff.test_files.len()
        );
    }

    let prefix = match split {
        EvalSplit::Calibration => "cal",
        EvalSplit::LockedUnseen => "unseen",
        EvalSplit::LiveJudge => "judge",
    };

    Ok(EvalIssueCase {
        id: format!("{prefix}-{repo}-{pr_number}"),
        repo_url: format!("https://github.com/{owner}/{repo}"),
        issue_number,
        pr_number: Some(pr_number),
        title: title.trim().to_string(),
        issue_body: issue_body.trim().to_string(),
        parent_commit: parent_commit.trim().to_string(),
        ground_truth_test_patch: split_diff.ground_truth_test_patch,
        has_multimodal_media: !media_urls.is_empty(),
        media_urls,
        split,
    })
}

#[derive(Debug, Deserialize)]
struct GithubPullBase {
    sha: String,
}

#[derive(Debug, Deserialize)]
struct GithubLabel {
    name: String,
}

#[derive(Debug, Deserialize)]
struct GithubPullResponse {
    number: u64,
    title: String,
    body: Option<String>,
    merged_at: Option<String>,
    base: GithubPullBase,
    #[serde(default)]
    labels: Vec<GithubLabel>,
}

#[derive(Debug, Deserialize)]
struct GithubIssueResponse {
    title: String,
    body: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GithubCommentResponse {
    body: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GithubSearchResponse {
    #[serde(default)]
    items: Vec<GithubSearchItem>,
}

#[derive(Debug, Deserialize)]
struct GithubSearchItem {
    number: u64,
    #[serde(default)]
    repository_url: String,
}

fn repo_slug_from_api_url(repository_url: &str) -> Option<(String, String)> {
    // Expected: https://api.github.com/repos/{owner}/{repo}
    let parts: Vec<&str> = repository_url.trim_end_matches('/').split('/').collect();
    if parts.len() >= 2 {
        Some((
            parts[parts.len() - 2].to_string(),
            parts[parts.len() - 1].to_string(),
        ))
    } else {
        None
    }
}

#[derive(Debug, Deserialize)]
struct GithubTimelineEvent {
    #[serde(default)]
    event: String,
    #[serde(default)]
    source: Option<GithubTimelineSource>,
}

#[derive(Debug, Deserialize)]
struct GithubTimelineSource {
    #[serde(default)]
    issue: Option<GithubTimelineIssue>,
}

#[derive(Debug, Deserialize)]
struct GithubTimelineIssue {
    number: u64,
    #[serde(default)]
    repository_url: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    pull_request: Option<GithubTimelinePullRef>,
}

#[derive(Debug, Deserialize)]
struct GithubTimelinePullRef {
    merged_at: Option<String>,
}

fn build_github_client(github_token: Option<&str>) -> Result<reqwest::Client> {
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static("epicenter-eval/0.1"));
    headers.insert(
        ACCEPT,
        HeaderValue::from_static("application/vnd.github+json"),
    );
    if let Some(token) = github_token.map(str::trim).filter(|t| !t.is_empty()) {
        let auth = HeaderValue::from_str(&format!("Bearer {token}"))
            .context("invalid GITHUB_TOKEN header value")?;
        headers.insert(AUTHORIZATION, auth);
    }
    reqwest::Client::builder()
        .default_headers(headers)
        .timeout(Duration::from_secs(30))
        .build()
        .context("failed to build GitHub HTTP client")
}

fn build_media_verify_client() -> Result<reqwest::Client> {
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static("epicenter-eval/0.1"));
    headers.insert(ACCEPT, HeaderValue::from_static("*/*"));
    reqwest::Client::builder()
        .default_headers(headers)
        .timeout(Duration::from_secs(15))
        .build()
        .context("failed to build media verification HTTP client")
}

fn looks_like_bugfix_pr(pr: &GithubPullResponse) -> bool {
    let title_lower = pr.title.to_ascii_lowercase();
    for skip_prefix in [
        "chore(deps)",
        "build(deps)",
        "deps:",
        "bump ",
        "release ",
        "docs:",
        "doc:",
        "ci:",
        "[pre-commit",
        "chore: release",
        "chore: bump",
    ] {
        if title_lower.starts_with(skip_prefix) {
            return false;
        }
    }
    if title_lower.contains("dependabot")
        || title_lower.contains("pre-commit autoupdate")
        || title_lower.contains("translations")
        || title_lower.contains("translate ")
    {
        return false;
    }

    let label_bug = pr.labels.iter().any(|l| {
        let n = l.name.to_ascii_lowercase();
        n.contains("bug") || n.contains("fix") || n.contains("regression") || n.contains("defect")
    });
    let title_bug = title_lower.contains("fix")
        || title_lower.contains("bug")
        || title_lower.contains("crash")
        || title_lower.contains("regression")
        || title_lower.contains("handle ")
        || title_lower.contains("prevent ")
        || title_lower.contains("preserve ")
        || title_lower.contains("avoid ")
        || title_lower.contains("error");
    let body_links_issue = pr
        .body
        .as_deref()
        .and_then(extract_linked_issue_number)
        .is_some();
    label_bug || title_bug || body_links_issue
}

async fn fetch_pr_context_and_verified_media(
    gh_client: &reqwest::Client,
    media_client: &reqwest::Client,
    owner: &str,
    repo: &str,
    pr: &GithubPullResponse,
    explicit_issue_num: Option<u64>,
) -> (u64, String, String, Vec<String>) {
    let pr_body = pr.body.clone().unwrap_or_default();
    let linked_issue = explicit_issue_num
        .or_else(|| extract_linked_issue_number_for_repo(&pr_body, Some(owner), Some(repo)));

    let mut media_candidates = extract_media_urls(&pr_body);

    let (issue_number, combined_title, combined_body) = if let Some(issue_num) = linked_issue {
        let issue_url = format!("https://api.github.com/repos/{owner}/{repo}/issues/{issue_num}");
        match gh_client.get(&issue_url).send().await {
            Ok(resp) if resp.status().is_success() => {
                match resp.json::<GithubIssueResponse>().await {
                    Ok(issue) => {
                        let issue_text = issue.body.unwrap_or_default();
                        for u in extract_media_urls(&issue_text) {
                            if !media_candidates.contains(&u) {
                                media_candidates.push(u);
                            }
                        }

                        // Also check first few issue comments for screenshots/repro media if none found yet.
                        if media_candidates.is_empty() {
                            let comments_url = format!(
                                "https://api.github.com/repos/{owner}/{repo}/issues/{issue_num}/comments?per_page=5"
                            );
                            if let Ok(c_resp) = gh_client.get(&comments_url).send().await {
                                if c_resp.status().is_success() {
                                    if let Ok(comments) =
                                        c_resp.json::<Vec<GithubCommentResponse>>().await
                                    {
                                        for c in comments {
                                            if let Some(cb) = c.body {
                                                for u in extract_media_urls(&cb) {
                                                    if !media_candidates.contains(&u) {
                                                        media_candidates.push(u);
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        let body = if pr_body.trim().is_empty() {
                            issue_text
                        } else {
                            format!(
                                "{}\n\n---\nPR Context (#{}):\n{}",
                                issue_text, pr.number, pr_body
                            )
                        };
                        (issue_num, issue.title, body)
                    }
                    Err(_) => (issue_num, pr.title.clone(), pr_body),
                }
            }
            _ => (issue_num, pr.title.clone(), pr_body),
        }
    } else {
        (pr.number, pr.title.clone(), pr_body)
    };

    let verified_media = verify_reachable_media_urls(media_client, &media_candidates).await;
    (issue_number, combined_title, combined_body, verified_media)
}

/// Evaluates a single candidate PR by number, fetching its metadata, unified diff, linked issue,
/// and live HTTP-200 verified media URLs.
#[allow(clippy::too_many_arguments)]
async fn try_harvest_pr_number(
    gh_client: &reqwest::Client,
    media_client: &reqwest::Client,
    owner: &str,
    repo: &str,
    pr_number: u64,
    explicit_issue_num: Option<u64>,
    strict_self_contained: bool,
    split: EvalSplit,
) -> Result<EvalIssueCase> {
    let api_url = format!("https://api.github.com/repos/{owner}/{repo}/pulls/{pr_number}");
    let pr: GithubPullResponse = gh_client
        .get(&api_url)
        .send()
        .await
        .with_context(|| format!("failed to query GitHub PR API {api_url}"))?
        .error_for_status()
        .with_context(|| format!("GitHub PR API returned error for {api_url}"))?
        .json()
        .await
        .context("failed to decode GitHub PR JSON")?;

    let Some(ref merged_at) = pr.merged_at else {
        bail!("PR {owner}/{repo}#{pr_number} is not merged");
    };
    if strict_self_contained && merged_at.as_str() < "2023-01-01" {
        bail!(
            "PR {owner}/{repo}#{pr_number} merged too long ago ({merged_at}); requiring >= 2023-01-01 for modern runtime compatibility"
        );
    }
    if let Err(reason) = is_valid_and_non_synthetic_sha(&pr.base.sha) {
        bail!("PR {owner}/{repo}#{pr_number} has invalid base SHA: {reason}");
    }
    if strict_self_contained && !looks_like_bugfix_pr(&pr) {
        bail!("PR {owner}/{repo}#{pr_number} does not look like a bug-fix PR");
    }

    let raw_diff = gh_client
        .get(&api_url)
        .header(ACCEPT, "application/vnd.github.v3.diff")
        .send()
        .await
        .with_context(|| format!("failed to fetch unified diff for {api_url}"))?
        .error_for_status()
        .with_context(|| format!("GitHub diff request failed for {api_url}"))?
        .text()
        .await
        .context("failed to read GitHub PR diff body")?;

    if raw_diff.contains("GIT binary patch") || raw_diff.contains("Binary files ") {
        bail!("PR {owner}/{repo}#{pr_number} contains binary diff hunks");
    }

    let split_diff = split_pr_diff_source_and_tests(&raw_diff);
    if !split_diff.has_both_source_and_tests() {
        bail!(
            "PR {owner}/{repo}#{pr_number} lacks source+test split (source={}, tests={})",
            split_diff.source_files.len(),
            split_diff.test_files.len()
        );
    }
    if !detect_synthetic_diff_indices(&split_diff.ground_truth_test_patch).is_empty() {
        bail!("PR {owner}/{repo}#{pr_number} has degenerate blob index in test diff");
    }

    if strict_self_contained {
        if raw_diff.len() > 35_000 {
            bail!(
                "PR {owner}/{repo}#{pr_number} diff too large ({} bytes)",
                raw_diff.len()
            );
        }
        let code_sources = split_diff.code_source_files();
        if code_sources.is_empty() || code_sources.len() > 3 {
            bail!(
                "PR {owner}/{repo}#{pr_number} modifies {} production code files (want 1..=3)",
                code_sources.len()
            );
        }
        if split_diff.source_files.len() > 4 {
            bail!(
                "PR {owner}/{repo}#{pr_number} modifies {} non-test files (want <= 4)",
                split_diff.source_files.len()
            );
        }
        if split_diff.test_files.is_empty() || split_diff.test_files.len() > 3 {
            bail!(
                "PR {owner}/{repo}#{pr_number} modifies {} test files (want 1..=3)",
                split_diff.test_files.len()
            );
        }
    }

    let (issue_number, combined_title, combined_body, verified_media) =
        fetch_pr_context_and_verified_media(
            gh_client,
            media_client,
            owner,
            repo,
            &pr,
            explicit_issue_num,
        )
        .await;

    build_case_from_pr_parts_with_media(
        owner,
        repo,
        pr.number,
        issue_number,
        &combined_title,
        &combined_body,
        &pr.base.sha,
        &raw_diff,
        verified_media,
        split,
    )
}

/// Converts any merged GitHub PR URL into a `LiveJudge` `EvalIssueCase` with separated
/// `parent_commit`, hidden `ground_truth_test_patch`, and live HTTP-200 verified `media_urls`.
pub async fn harvest_single_pr(
    pr_url: &str,
    github_token: Option<&str>,
) -> Result<EvalIssueCase> {
    let (owner, repo, pr_number) = parse_pr_url(pr_url)?;
    let gh_client = build_github_client(github_token)?;
    let media_client = build_media_verify_client()?;
    try_harvest_pr_number(
        &gh_client,
        &media_client,
        &owner,
        &repo,
        pr_number,
        None,
        false,
        EvalSplit::LiveJudge,
    )
    .await
}

/// Searches target repositories in small batches (staying well within GitHub's 30 req/min Search
/// API limit) for merged PRs and closed issues containing real GitHub user-attachment/image URLs.
/// Returns `(owner, repo, pr_number, linked_issue_number)` tuples.
async fn discover_multimodal_candidates_batched(
    gh_client: &reqwest::Client,
    repos: &[String],
) -> Vec<(String, String, u64, Option<u64>)> {
    let mut candidates: Vec<(String, String, u64, Option<u64>)> = Vec::new();
    let mut seen_prs: HashSet<(String, String, u64)> = HashSet::new();

    for chunk in repos.chunks(6) {
        let repo_qualifiers = chunk
            .iter()
            .filter(|r| r.contains('/'))
            .map(|r| format!("repo:{r}"))
            .collect::<Vec<_>>()
            .join(" ");
        if repo_qualifiers.is_empty() {
            continue;
        }

        // 1. Search recent merged PRs directly containing GitHub attachment/image URLs.
        for query_term in ["user-attachments/assets", "user-images.githubusercontent.com"] {
            let q = format!(
                "{repo_qualifiers} is:pr is:merged merged:>=2023-01-01 \"{query_term}\""
            );
            if let Ok(r) = gh_client
                .get("https://api.github.com/search/issues")
                .query(&[
                    ("q", q.as_str()),
                    ("sort", "updated"),
                    ("order", "desc"),
                    ("per_page", "30"),
                ])
                .send()
                .await
            {
                if r.status().is_success() {
                    if let Ok(search) = r.json::<GithubSearchResponse>().await {
                        for item in search.items {
                            if let Some((owner, repo)) =
                                repo_slug_from_api_url(&item.repository_url)
                            {
                                if seen_prs.insert((owner.clone(), repo.clone(), item.number)) {
                                    candidates.push((owner, repo, item.number, None));
                                }
                            }
                        }
                    }
                } else {
                    debug!(status = %r.status(), term = query_term, "batched PR media search non-2xx");
                }
            }
            tokio::time::sleep(Duration::from_millis(350)).await;
        }

        // 2. Search recent closed issues containing GitHub attachment URLs and inspect timelines for merged fixing PRs.
        let issue_q = format!(
            "{repo_qualifiers} is:issue is:closed created:>=2023-01-01 \"user-attachments/assets\""
        );
        if let Ok(r) = gh_client
            .get("https://api.github.com/search/issues")
            .query(&[
                ("q", issue_q.as_str()),
                ("sort", "updated"),
                ("order", "desc"),
                ("per_page", "25"),
            ])
            .send()
            .await
        {
            if r.status().is_success() {
                if let Ok(search) = r.json::<GithubSearchResponse>().await {
                    for issue_item in search.items {
                        let Some((owner, repo)) =
                            repo_slug_from_api_url(&issue_item.repository_url)
                        else {
                            continue;
                        };
                        let timeline_url = format!(
                            "https://api.github.com/repos/{owner}/{repo}/issues/{}/timeline?per_page=30",
                            issue_item.number
                        );
                        if let Ok(t_resp) = gh_client.get(&timeline_url).send().await {
                            if t_resp.status().is_success() {
                                if let Ok(events) = t_resp.json::<Vec<GithubTimelineEvent>>().await
                                {
                                    for ev in events {
                                        if ev.event == "cross-referenced" {
                                            if let Some(issue_ref) = ev.source.and_then(|s| s.issue)
                                            {
                                                let same_repo = repo_slug_from_api_url(
                                                    &issue_ref.repository_url,
                                                )
                                                .is_some_and(|(o, r)| {
                                                    o.eq_ignore_ascii_case(&owner)
                                                        && r.eq_ignore_ascii_case(&repo)
                                                });
                                                let is_recent_merged_pr = issue_ref
                                                    .pull_request
                                                    .as_ref()
                                                    .and_then(|p| p.merged_at.as_deref())
                                                    .is_some_and(|m| m >= "2023-01-01");
                                                let pr_body_links_this_issue = issue_ref
                                                    .body
                                                    .as_deref()
                                                    .and_then(|b| {
                                                        extract_linked_issue_number_for_repo(
                                                            b,
                                                            Some(&owner),
                                                            Some(&repo),
                                                        )
                                                    })
                                                    == Some(issue_item.number);
                                                if same_repo
                                                    && is_recent_merged_pr
                                                    && pr_body_links_this_issue
                                                    && seen_prs.insert((
                                                        owner.clone(),
                                                        repo.clone(),
                                                        issue_ref.number,
                                                    ))
                                                {
                                                    candidates.push((
                                                        owner.clone(),
                                                        repo.clone(),
                                                        issue_ref.number,
                                                        Some(issue_item.number),
                                                    ));
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            } else {
                debug!(status = %r.status(), "batched issue media search non-2xx");
            }
        }
        tokio::time::sleep(Duration::from_millis(350)).await;
    }

    candidates
}

/// Queries GitHub REST API across target OSS Python & TypeScript repositories for clean,
/// self-contained merged bug-fix PRs (1–3 code files, 1–3 test files, real `base.sha`, linked
/// issues, and live HTTP-200 verified `media_urls`).
pub async fn harvest_repositories(config: &HarvestConfig) -> Result<Vec<EvalIssueCase>> {
    let gh_client = build_github_client(config.github_token.as_deref())?;
    let media_client = build_media_verify_client()?;
    let mut harvested: Vec<EvalIssueCase> = Vec::new();
    let mut seen_repo_prs: HashSet<(String, u64)> = HashSet::new();
    let mut repo_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    // Pass 1 (if prefer_multimodal): actively hunt for real merged bug-fix PRs with HTTP-200 screenshots/videos.
    if config.prefer_multimodal {
        info!(repos = config.repos.len(), "hunting real multimodal merged bug-fix PRs via batched GitHub search");
        let mm_candidates =
            discover_multimodal_candidates_batched(&gh_client, &config.repos).await;
        info!(candidates = mm_candidates.len(), "found multimodal PR candidates to validate");

        for (owner, repo, pr_num, issue_num) in mm_candidates {
            if harvested.len() >= config.max_total_cases {
                break;
            }
            let repo_slug = format!("{owner}/{repo}");
            let current_repo_count = *repo_counts.get(&repo_slug).unwrap_or(&0);
            if current_repo_count >= config.max_prs_per_repo {
                continue;
            }
            if !seen_repo_prs.insert((repo_slug.clone(), pr_num)) {
                continue;
            }
            match try_harvest_pr_number(
                &gh_client,
                &media_client,
                &owner,
                &repo,
                pr_num,
                issue_num,
                true,
                EvalSplit::LockedUnseen,
            )
            .await
            {
                Ok(case) if case.has_multimodal_media => {
                    info!(
                        id = %case.id,
                        media_urls = case.media_urls.len(),
                        "harvested verified multimodal bug-fix PR"
                    );
                    harvested.push(case);
                    *repo_counts.entry(repo_slug).or_insert(0) += 1;
                }
                Ok(_) => {
                    debug!(repo = %repo_slug, pr = pr_num, "candidate had no HTTP-200 verified media after filtering");
                }
                Err(err) => {
                    debug!(repo = %repo_slug, pr = pr_num, reason = %err, "skipping multimodal candidate");
                }
            }
        }
    }

    // Pass 2: round-robin across Python & TypeScript repos (1 PR per repo per round) so every
    // language and repository family gets balanced coverage.
    for round in 0..config.max_prs_per_repo {
        if harvested.len() >= config.max_total_cases {
            break;
        }
        let target_for_round = round + 1;

        for repo_slug in &config.repos {
            if harvested.len() >= config.max_total_cases {
                break;
            }
            let Some((owner, repo)) = repo_slug.split_once('/') else {
                warn!(repo = %repo_slug, "skipping invalid repo slug (expected owner/repo)");
                continue;
            };

            let existing_for_repo = *repo_counts.get(repo_slug).unwrap_or(&0);
            if existing_for_repo >= target_for_round || existing_for_repo >= config.max_prs_per_repo
            {
                continue;
            }

            let mut added_this_round = false;
            for page in 1..=2u32 {
                if added_this_round || harvested.len() >= config.max_total_cases {
                    break;
                }
                let pulls_url = format!(
                    "https://api.github.com/repos/{owner}/{repo}/pulls?state=closed&sort=created&direction=desc&per_page=50&page={page}"
                );
                info!(repo = %repo_slug, round = target_for_round, page, "querying merged bug-fix PRs");

                let resp = match gh_client.get(&pulls_url).send().await {
                    Ok(r) => r,
                    Err(err) => {
                        warn!(repo = %repo_slug, error = %err, "failed to reach GitHub API for repo");
                        break;
                    }
                };
                if !resp.status().is_success() {
                    warn!(repo = %repo_slug, status = %resp.status(), "GitHub API returned non-2xx");
                    break;
                }
                let prs: Vec<GithubPullResponse> = match resp.json().await {
                    Ok(v) => v,
                    Err(err) => {
                        warn!(repo = %repo_slug, error = %err, "failed to parse PR list JSON");
                        break;
                    }
                };
                if prs.is_empty() {
                    break;
                }

                for pr in prs {
                    if added_this_round || harvested.len() >= config.max_total_cases {
                        break;
                    }
                    if pr.merged_at.is_none() || !looks_like_bugfix_pr(&pr) {
                        continue;
                    }
                    if !seen_repo_prs.insert((repo_slug.clone(), pr.number)) {
                        continue;
                    }

                    match try_harvest_pr_number(
                        &gh_client,
                        &media_client,
                        owner,
                        repo,
                        pr.number,
                        None,
                        true,
                        EvalSplit::LockedUnseen,
                    )
                    .await
                    {
                        Ok(case) => {
                            info!(
                                id = %case.id,
                                multimodal = case.has_multimodal_media,
                                "harvested verified bug-fix PR"
                            );
                            harvested.push(case);
                            *repo_counts.entry(repo_slug.clone()).or_insert(0) += 1;
                            added_this_round = true;
                        }
                        Err(err) => {
                            debug!(repo = %repo_slug, pr = pr.number, reason = %err, "skipping PR");
                        }
                    }
                }
            }
        }
    }

    if harvested.is_empty() {
        return Err(anyhow!(
            "harvested 0 cases from {:?} (check network access or GITHUB_TOKEN rate limits)",
            config.repos
        ));
    }

    // Assign splits cleanly: pick diverse repos (and mix of multimodal + non-multimodal) for Calibration.
    let cal_target = config.calibration_count.min(harvested.len());
    let mut cal_indices: HashSet<usize> = HashSet::new();
    let mut seen_cal_repos: HashSet<String> = HashSet::new();
    let mut cal_mm_count = 0usize;
    let max_cal_mm = cal_target.div_ceil(2);

    // First select up to max_cal_mm multimodal cases from distinct repos for Calibration.
    for (idx, c) in harvested.iter().enumerate() {
        if cal_indices.len() >= cal_target || cal_mm_count >= max_cal_mm {
            break;
        }
        if c.has_multimodal_media && seen_cal_repos.insert(c.repo_url.clone()) {
            cal_indices.insert(idx);
            cal_mm_count += 1;
        }
    }
    // Fill remaining calibration slots from distinct repos (preferring non-multimodal so unseen keeps multimodal cases too).
    for (idx, c) in harvested.iter().enumerate() {
        if cal_indices.len() >= cal_target {
            break;
        }
        if !c.has_multimodal_media && seen_cal_repos.insert(c.repo_url.clone()) {
            cal_indices.insert(idx);
        }
    }
    for idx in 0..harvested.len() {
        if cal_indices.len() >= cal_target {
            break;
        }
        cal_indices.insert(idx);
    }

    let mut calibration_cases = Vec::new();
    let mut unseen_cases = Vec::new();
    for (idx, mut case) in harvested.into_iter().enumerate() {
        let repo_name = case
            .repo_url
            .rsplit('/')
            .next()
            .unwrap_or("repo")
            .to_string();
        let pr_num = case.pr_number.unwrap_or(case.issue_number);
        if cal_indices.contains(&idx) {
            case.split = EvalSplit::Calibration;
            case.id = format!("cal-{repo_name}-{pr_num}");
            calibration_cases.push(case);
        } else {
            case.split = EvalSplit::LockedUnseen;
            case.id = format!("unseen-{repo_name}-{pr_num}");
            unseen_cases.push(case);
        }
    }

    calibration_cases.extend(unseen_cases);
    Ok(calibration_cases)
}
