use anyhow::{anyhow, bail, Context, Result};
use epicenter_common::{EvalIssueCase, EvalSplit};
use epicenter_graph::normalize_rel_path;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, USER_AGENT};
use serde::Deserialize;
use std::collections::HashSet;
use tracing::{debug, info, warn};

/// Default external Python & TypeScript open-source repositories targeted by the 5-Sigma harvester.
pub const DEFAULT_TARGET_REPOS: &[&str] = &[
    "encode/httpx",
    "pydantic/pydantic",
    "tiangolo/fastapi",
    "colinhacks/zod",
    "honojs/hono",
    "psf/requests",
    "pallets/click",
];

/// Configuration for batch harvesting merged bug-fix PRs from GitHub.
#[derive(Debug, Clone)]
pub struct HarvestConfig {
    pub repos: Vec<String>,
    pub max_prs_per_repo: usize,
    pub max_total_cases: usize,
    pub calibration_count: usize,
    pub github_token: Option<String>,
}

impl Default for HarvestConfig {
    fn default() -> Self {
        Self {
            repos: DEFAULT_TARGET_REPOS.iter().map(|s| s.to_string()).collect(),
            max_prs_per_repo: 4,
            max_total_cases: 20,
            calibration_count: 5,
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

/// Extracts image, GIF, diagram, and video URLs from GitHub issue/PR Markdown and HTML.
pub fn extract_media_urls(markdown: &str) -> Vec<String> {
    let mut urls: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    let mut push_url = |raw: &str| {
        let cleaned = clean_extracted_url(raw);
        if is_http_url(&cleaned) && seen.insert(cleaned.clone()) {
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
    if !is_http_url(url) {
        return false;
    }
    let lower = url.to_ascii_lowercase();
    if lower.contains("github.com/user-attachments/assets/")
        || lower.contains("user-images.githubusercontent.com/")
        || lower.contains("private-user-images.githubusercontent.com/")
        || lower.contains("camo.githubusercontent.com/")
    {
        return true;
    }
    let without_query = lower.split(['?', '#']).next().unwrap_or(&lower);
    const MEDIA_EXTS: &[&str] = &[
        ".png", ".jpg", ".jpeg", ".gif", ".webp", ".svg", ".mp4", ".mov", ".webm", ".mkv",
    ];
    MEDIA_EXTS.iter().any(|ext| without_query.ends_with(ext))
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

    // Expected: ["github.com", owner, repo, "pull", number, ...]
    let start_idx = if segments.first().is_some_and(|s| s.eq_ignore_ascii_case("github.com")) {
        1
    } else if segments
        .first()
        .is_some_and(|s| s.eq_ignore_ascii_case("www.github.com"))
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

/// Extracts a linked GitHub issue number from keywords like `Fixes #123`, `Closes #456`, `Resolves #789`.
pub fn extract_linked_issue_number(body: &str) -> Option<u64> {
    let lower = body.to_ascii_lowercase();
    for keyword in ["fixes #", "fix #", "closes #", "close #", "resolves #", "resolve #"] {
        if let Some(pos) = lower.find(keyword) {
            let digits_start = pos + keyword.len();
            let digits: String = lower[digits_start..]
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
    None
}

/// Infers the default test runner command for a Python or TypeScript repository and test file list.
pub fn infer_test_command(repo_url: &str, test_files: &[String]) -> String {
    let lower_repo = repo_url.to_ascii_lowercase();
    let is_ts = lower_repo.contains("zod")
        || lower_repo.contains("hono")
        || test_files
            .iter()
            .any(|f| f.ends_with(".ts") || f.ends_with(".tsx") || f.ends_with(".js"));

    if is_ts {
        if test_files.is_empty() {
            "npx vitest run".to_string()
        } else {
            format!("npx vitest run {}", test_files.join(" "))
        }
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
    let split_diff = split_pr_diff_source_and_tests(raw_diff);
    if !split_diff.has_both_source_and_tests() {
        bail!(
            "PR {owner}/{repo}#{pr_number} must modify both source ({} files) and test ({} files) files",
            split_diff.source_files.len(),
            split_diff.test_files.len()
        );
    }

    let media_urls = extract_media_urls(issue_body);
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
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .context("failed to build GitHub HTTP client")
}

fn looks_like_bugfix_pr(pr: &GithubPullResponse) -> bool {
    let title_lower = pr.title.to_ascii_lowercase();
    let label_bug = pr.labels.iter().any(|l| {
        let n = l.name.to_ascii_lowercase();
        n.contains("bug") || n.contains("fix") || n.contains("regression") || n.contains("defect")
    });
    let title_bug = title_lower.contains("fix")
        || title_lower.contains("bug")
        || title_lower.contains("crash")
        || title_lower.contains("regression")
        || title_lower.contains("handle ")
        || title_lower.contains("prevent ");
    let body_links_issue = pr
        .body
        .as_deref()
        .and_then(extract_linked_issue_number)
        .is_some();
    label_bug || title_bug || body_links_issue
}

/// Converts any merged GitHub PR URL into a `LiveJudge` `EvalIssueCase` with separated
/// `parent_commit` and hidden `ground_truth_test_patch`.
pub async fn harvest_single_pr(
    pr_url: &str,
    github_token: Option<&str>,
) -> Result<EvalIssueCase> {
    let (owner, repo, pr_number) = parse_pr_url(pr_url)?;
    let client = build_github_client(github_token)?;

    let api_url = format!("https://api.github.com/repos/{owner}/{repo}/pulls/{pr_number}");
    let pr: GithubPullResponse = client
        .get(&api_url)
        .send()
        .await
        .with_context(|| format!("failed to query GitHub PR API {api_url}"))?
        .error_for_status()
        .with_context(|| format!("GitHub PR API returned error for {api_url}"))?
        .json()
        .await
        .context("failed to decode GitHub PR JSON")?;

    if pr.merged_at.is_none() {
        bail!("PR {owner}/{repo}#{pr_number} is not merged; 5-Sigma grading requires a merged ground-truth PR");
    }

    let raw_diff = client
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

    let pr_body = pr.body.unwrap_or_default();
    let linked_issue = extract_linked_issue_number(&pr_body);
    let (issue_number, combined_title, combined_body) = if let Some(issue_num) = linked_issue {
        let issue_url = format!("https://api.github.com/repos/{owner}/{repo}/issues/{issue_num}");
        match client.get(&issue_url).send().await {
            Ok(resp) if resp.status().is_success() => {
                match resp.json::<GithubIssueResponse>().await {
                    Ok(issue) => {
                        let body = format!(
                            "{}\n\n---\nPR Context (#{}):\n{}",
                            issue.body.unwrap_or_default(),
                            pr_number,
                            pr_body
                        );
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

    build_case_from_pr_parts(
        &owner,
        &repo,
        pr.number,
        issue_number,
        &combined_title,
        &combined_body,
        &pr.base.sha,
        &raw_diff,
        EvalSplit::LiveJudge,
    )
}

/// Queries GitHub REST API across target OSS Python & TypeScript repositories for merged
/// bug-fix PRs modifying both source and test files, splitting hidden test patches cleanly.
pub async fn harvest_repositories(config: &HarvestConfig) -> Result<Vec<EvalIssueCase>> {
    let client = build_github_client(config.github_token.as_deref())?;
    let mut harvested: Vec<EvalIssueCase> = Vec::new();

    for repo_slug in &config.repos {
        if harvested.len() >= config.max_total_cases {
            break;
        }
        let Some((owner, repo)) = repo_slug.split_once('/') else {
            warn!(repo = %repo_slug, "skipping invalid repo slug (expected owner/repo)");
            continue;
        };

        let pulls_url = format!(
            "https://api.github.com/repos/{owner}/{repo}/pulls?state=closed&sort=updated&direction=desc&per_page=30"
        );
        info!(repo = %repo_slug, "querying merged bug-fix PRs");

        let resp = match client.get(&pulls_url).send().await {
            Ok(r) => r,
            Err(err) => {
                warn!(repo = %repo_slug, error = %err, "failed to reach GitHub API for repo");
                continue;
            }
        };
        if !resp.status().is_success() {
            warn!(repo = %repo_slug, status = %resp.status(), "GitHub API returned non-2xx");
            continue;
        }
        let prs: Vec<GithubPullResponse> = match resp.json().await {
            Ok(v) => v,
            Err(err) => {
                warn!(repo = %repo_slug, error = %err, "failed to parse PR list JSON");
                continue;
            }
        };

        let mut repo_count = 0usize;
        for pr in prs {
            if repo_count >= config.max_prs_per_repo || harvested.len() >= config.max_total_cases {
                break;
            }
            if pr.merged_at.is_none() || !looks_like_bugfix_pr(&pr) {
                continue;
            }

            let diff_url =
                format!("https://api.github.com/repos/{owner}/{repo}/pulls/{}", pr.number);
            let raw_diff = match client
                .get(&diff_url)
                .header(ACCEPT, "application/vnd.github.v3.diff")
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => r.text().await.unwrap_or_default(),
                Ok(r) => {
                    debug!(pr = pr.number, status = %r.status(), "skipping PR diff error");
                    continue;
                }
                Err(err) => {
                    debug!(pr = pr.number, error = %err, "skipping PR diff network error");
                    continue;
                }
            };

            let body = pr.body.unwrap_or_default();
            let issue_num = extract_linked_issue_number(&body).unwrap_or(pr.number);
            let split = if harvested.len() < config.calibration_count {
                EvalSplit::Calibration
            } else {
                EvalSplit::LockedUnseen
            };

            match build_case_from_pr_parts(
                owner,
                repo,
                pr.number,
                issue_num,
                &pr.title,
                &body,
                &pr.base.sha,
                &raw_diff,
                split,
            ) {
                Ok(case) => {
                    info!(
                        id = %case.id,
                        multimodal = case.has_multimodal_media,
                        "harvested benchmark case"
                    );
                    harvested.push(case);
                    repo_count += 1;
                }
                Err(err) => {
                    debug!(pr = pr.number, reason = %err, "skipping PR without source+test split");
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

    Ok(harvested)
}
