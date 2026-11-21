use crate::harvester::is_test_file_path;
use anyhow::{anyhow, bail, Context, Result};
use epicenter_common::EvalIssueCase;
use epicenter_graph::parse_unified_diff;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::time::Duration;

/// Single integrity violation discovered while auditing a dataset case.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyFailure {
    pub case_id: String,
    pub field: String,
    pub reason: String,
}

/// Per-case verification outcome summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaseVerifyStatus {
    pub case_id: String,
    pub repo_url: String,
    pub pr_number: Option<u64>,
    pub parent_commit: String,
    pub media_count: usize,
    pub passed: bool,
    pub errors: Vec<String>,
}

/// Aggregate result of auditing a dataset JSON (offline static checks + optional live GitHub checks).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyReport {
    pub total_cases: usize,
    pub passed_cases: usize,
    pub failed_cases: usize,
    pub multimodal_cases: usize,
    pub verified_media_urls: usize,
    pub checked_github: bool,
    pub failures: Vec<VerifyFailure>,
    pub case_statuses: Vec<CaseVerifyStatus>,
}

impl VerifyReport {
    pub fn is_ok(&self) -> bool {
        self.total_cases > 0 && self.failures.is_empty()
    }
}

/// Known synthetic / placeholder SHA patterns that never occur organically across curated fixtures.
const SYNTHETIC_SHA_SUBSTRINGS: &[&str] = &[
    "1a2b3c4d",
    "2b3c4d5e",
    "3c4d5e6f",
    "4d5e6f7a",
    "5e6f7a8b",
    "6f7a8b9c",
    "7a8b9c0d",
    "8b9c0d1e",
    "9c0d1e2f",
    "9c3d4e5f",
    "e1f2a3b4",
    "a1b2c3d4",
    "01234567",
    "12345678",
    "23456789",
    "abcdef01",
    "deadbeef",
    "cafebabe",
    "feedface",
    "badc0ffe",
    "00000000",
    "ffffffff",
];

/// Placeholder URL substrings found in fabricated fixtures.
const PLACEHOLDER_MEDIA_SUBSTRINGS: &[&str] = &[
    "abcdef123456",
    "1111-2222",
    "aaaa-bbbb",
    "aaaa-1111",
    "bbbb-2222",
    "0123456789",
    "12345678",
    "3456-4789",
    "c3d4-4e5f",
    "00000000-0000",
    "deadbeef",
    "example.com",
    "example.org",
    "placeholder",
];

/// Validates that a commit SHA is a genuine 40-character hexadecimal SHA and does not match
/// synthetic sequential or repeating hex patterns.
pub fn is_valid_and_non_synthetic_sha(sha: &str) -> std::result::Result<(), String> {
    let trimmed = sha.trim();
    if trimmed.len() != 40 || !trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!(
            "invalid 40-char hex commit SHA: '{trimmed}' (len={})",
            trimmed.len()
        ));
    }

    let lower = trimmed.to_ascii_lowercase();
    for pat in SYNTHETIC_SHA_SUBSTRINGS {
        if lower.contains(pat) {
            return Err(format!(
                "synthetic commit SHA pattern '{pat}' detected in '{trimmed}'"
            ));
        }
    }

    // Check for >= 7 identical consecutive hex characters (e.g. "aaaaaaa", "1111111").
    let bytes = lower.as_bytes();
    let mut run_len = 1usize;
    for i in 1..bytes.len() {
        if bytes[i] == bytes[i - 1] {
            run_len += 1;
            if run_len >= 7 {
                return Err(format!(
                    "synthetic repeating hex run detected in commit SHA '{trimmed}'"
                ));
            }
        } else {
            run_len = 1;
        }
    }

    // Check for repeating 2-char hex pair (e.g., "12121212", "abababab") of length >= 8.
    for start in 0..bytes.len().saturating_sub(7) {
        let a = bytes[start];
        let b = bytes[start + 1];
        if bytes[start + 2] == a
            && bytes[start + 3] == b
            && bytes[start + 4] == a
            && bytes[start + 5] == b
            && bytes[start + 6] == a
            && bytes[start + 7] == b
        {
            return Err(format!(
                "synthetic repeating 2-char hex pattern detected in commit SHA '{trimmed}'"
            ));
        }
    }

    // Check for sequential nibble stride (+1 mod 16) over >= 8 consecutive chars
    // or interleaved pair stride (+1 or +2 mod 16 on both even and odd lanes over >= 10 chars,
    // which catches fabricated SHAs like 1a2b3c4d5e6f or 2a4c6e8f0b).
    let nibbles: Vec<u8> = bytes
        .iter()
        .map(|&b| match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'f' => b - b'a' + 10,
            _ => 0,
        })
        .collect();

    let mut seq_run = 1usize;
    for i in 1..nibbles.len() {
        if (nibbles[i - 1] + 1) % 16 == nibbles[i] {
            seq_run += 1;
            if seq_run >= 8 {
                return Err(format!(
                    "synthetic sequential hex run detected in commit SHA '{trimmed}'"
                ));
            }
        } else {
            seq_run = 1;
        }
    }

    for stride in [1u8, 2u8] {
        let mut interleaved_run = 2usize;
        for i in 2..nibbles.len() {
            if (nibbles[i - 2] + stride) % 16 == nibbles[i] {
                interleaved_run += 1;
                if interleaved_run >= 10 {
                    return Err(format!(
                        "synthetic interleaved hex stride (+{stride}) detected in commit SHA '{trimmed}'"
                    ));
                }
            } else {
                interleaved_run = 2;
            }
        }
    }

    Ok(())
}

/// Inspects `index <old>..<new>` lines in a unified diff and reports any synthetic blob hashes
/// (e.g., `1111111..2222222`, `3333333..4444444`, repeated digits, `1212121`, `1234567`).
/// Real git new/deleted file hashes (`0000000..<real_hash>` or `<real_hash>..0000000`) are allowed.
pub fn detect_synthetic_diff_indices(patch: &str) -> Vec<String> {
    let mut problems = Vec::new();
    for line in patch.lines() {
        let Some(rest) = line.strip_prefix("index ") else {
            continue;
        };
        let Some(range_token) = rest.split_whitespace().next() else {
            continue;
        };
        let Some((old_hash, new_hash)) = range_token.split_once("..") else {
            continue;
        };

        let old_zero = is_all_zeros(old_hash);
        let new_zero = is_all_zeros(new_hash);
        if old_zero && new_zero {
            problems.push(format!("synthetic all-zero index range '{range_token}'"));
            continue;
        }

        for (label, hash) in [("old", old_hash), ("new", new_hash)] {
            if is_all_zeros(hash) {
                continue;
            }
            if let Some(reason) = detect_synthetic_blob_hash(hash) {
                problems.push(format!(
                    "synthetic {label} blob index '{hash}' in '{range_token}': {reason}"
                ));
            }
        }
    }
    problems
}

fn is_all_zeros(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b == b'0')
}

fn detect_synthetic_blob_hash(hash: &str) -> Option<&'static str> {
    let lower = hash.to_ascii_lowercase();
    if lower.len() < 7 || !lower.chars().all(|c| c.is_ascii_hexdigit()) {
        return Some("malformed blob hash");
    }

    let bytes = lower.as_bytes();
    // All identical chars: 1111111, 2222222, aaaaaaa, etc.
    if bytes.iter().all(|&b| b == bytes[0]) {
        return Some("repeated-character blob hash");
    }

    // Repeating 2-char pair: 1010101, 1212121, 2323232, abababa.
    if bytes.len() >= 6
        && bytes
            .iter()
            .enumerate()
            .all(|(i, &b)| b == bytes[i % 2])
    {
        return Some("repeating 2-char blob hash");
    }

    // Pure ascending sequential hex: 1234567, 2345678, abcdef0.
    let nibbles: Vec<u8> = bytes
        .iter()
        .map(|&b| match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'f' => b - b'a' + 10,
            _ => 0,
        })
        .collect();
    if nibbles.len() >= 7
        && nibbles
            .windows(2)
            .all(|w| (w[0] + 1) % 16 == w[1])
    {
        return Some("sequential hex blob hash");
    }

    None
}

/// Returns `Some(reason)` if `url` looks like a synthetic/placeholder media URL.
pub fn detect_placeholder_media_url(url: &str) -> Option<String> {
    let trimmed = url.trim();
    if !trimmed.starts_with("https://") && !trimmed.starts_with("http://") {
        return Some(format!("media URL does not use http(s): '{trimmed}'"));
    }
    let lower = trimmed.to_ascii_lowercase();
    for pat in PLACEHOLDER_MEDIA_SUBSTRINGS {
        if lower.contains(pat) {
            return Some(format!(
                "placeholder pattern '{pat}' found in media URL '{trimmed}'"
            ));
        }
    }

    // Check UUID-shaped segments (e.g. GitHub user-attachments/assets/<uuid>) for synthetic chunks.
    if let Some(last_seg) = lower
        .split(['?', '#'])
        .next()
        .and_then(|p| p.rsplit('/').next())
    {
        let parts: Vec<&str> = last_seg.split('-').collect();
        if parts.len() == 5 && parts.iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_hexdigit())) {
            for part in &parts {
                if part.len() >= 4 && part.bytes().all(|b| b == part.as_bytes()[0]) {
                    return Some(format!(
                        "synthetic repeated UUID segment '{part}' in media URL '{trimmed}'"
                    ));
                }
            }
            if last_seg.contains("abcdef") || last_seg.contains("012345") || last_seg.contains("1234-5678") {
                return Some(format!(
                    "synthetic sequential UUID segment in media URL '{trimmed}'"
                ));
            }
        }
    }

    None
}

/// Parses `https://github.com/<owner>/<repo>` (with optional `.git` or trailing slash).
pub fn parse_github_repo_slug(repo_url: &str) -> Result<(String, String)> {
    let trimmed = repo_url.trim().trim_end_matches('/');
    let without_scheme = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .unwrap_or(trimmed);
    let parts: Vec<&str> = without_scheme.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() < 3
        || (!parts[0].eq_ignore_ascii_case("github.com")
            && !parts[0].eq_ignore_ascii_case("www.github.com"))
    {
        bail!("expected https://github.com/<owner>/<repo>, got '{repo_url}'");
    }
    let owner = parts[1].to_string();
    let repo = parts[2].trim_end_matches(".git").to_string();
    if owner.is_empty() || repo.is_empty() {
        bail!("invalid GitHub repo slug in '{repo_url}'");
    }
    Ok((owner, repo))
}

/// Runs deterministic offline integrity checks across all dataset cases.
pub fn verify_cases_static(cases: &[EvalIssueCase]) -> VerifyReport {
    let mut failures = Vec::new();
    let mut case_statuses = Vec::with_capacity(cases.len());
    let mut seen_ids = HashSet::new();
    let mut multimodal_cases = 0usize;

    if cases.is_empty() {
        failures.push(VerifyFailure {
            case_id: "<dataset>".to_string(),
            field: "cases".to_string(),
            reason: "dataset contains 0 cases".to_string(),
        });
    }

    for case in cases {
        let mut errors = Vec::new();

        if case.id.trim().is_empty() {
            errors.push("case id is empty".to_string());
            failures.push(VerifyFailure {
                case_id: "<empty>".to_string(),
                field: "id".to_string(),
                reason: "case id is empty".to_string(),
            });
        } else if !seen_ids.insert(case.id.clone()) {
            let reason = format!("duplicate case id '{}'", case.id);
            errors.push(reason.clone());
            failures.push(VerifyFailure {
                case_id: case.id.clone(),
                field: "id".to_string(),
                reason,
            });
        }

        if let Err(err) = parse_github_repo_slug(&case.repo_url) {
            let reason = err.to_string();
            errors.push(reason.clone());
            failures.push(VerifyFailure {
                case_id: case.id.clone(),
                field: "repo_url".to_string(),
                reason,
            });
        }

        if case.pr_number.is_none() {
            let reason = "pr_number is missing; real PR benchmark cases must reference a merged PR".to_string();
            errors.push(reason.clone());
            failures.push(VerifyFailure {
                case_id: case.id.clone(),
                field: "pr_number".to_string(),
                reason,
            });
        }

        if let Err(reason) = is_valid_and_non_synthetic_sha(&case.parent_commit) {
            errors.push(reason.clone());
            failures.push(VerifyFailure {
                case_id: case.id.clone(),
                field: "parent_commit".to_string(),
                reason,
            });
        }

        let parsed_gt = parse_unified_diff(&case.ground_truth_test_patch);
        if parsed_gt.is_empty() {
            let reason = "ground_truth_test_patch is empty or unparseable".to_string();
            errors.push(reason.clone());
            failures.push(VerifyFailure {
                case_id: case.id.clone(),
                field: "ground_truth_test_patch".to_string(),
                reason,
            });
        } else {
            for f in &parsed_gt {
                if !is_test_file_path(&f.path) {
                    let reason = format!(
                        "ground_truth_test_patch modifies non-test file '{}'",
                        f.path
                    );
                    errors.push(reason.clone());
                    failures.push(VerifyFailure {
                        case_id: case.id.clone(),
                        field: "ground_truth_test_patch".to_string(),
                        reason,
                    });
                }
            }
        }

        for idx_err in detect_synthetic_diff_indices(&case.ground_truth_test_patch) {
            errors.push(idx_err.clone());
            failures.push(VerifyFailure {
                case_id: case.id.clone(),
                field: "ground_truth_test_patch".to_string(),
                reason: idx_err,
            });
        }

        if case.has_multimodal_media {
            multimodal_cases += 1;
            if case.media_urls.is_empty() {
                let reason = "has_multimodal_media is true but media_urls is empty".to_string();
                errors.push(reason.clone());
                failures.push(VerifyFailure {
                    case_id: case.id.clone(),
                    field: "media_urls".to_string(),
                    reason,
                });
            }
        } else if !case.media_urls.is_empty() {
            let reason = "has_multimodal_media is false but media_urls is non-empty".to_string();
            errors.push(reason.clone());
            failures.push(VerifyFailure {
                case_id: case.id.clone(),
                field: "media_urls".to_string(),
                reason,
            });
        }

        for url in &case.media_urls {
            if let Some(reason) = detect_placeholder_media_url(url) {
                errors.push(reason.clone());
                failures.push(VerifyFailure {
                    case_id: case.id.clone(),
                    field: "media_urls".to_string(),
                    reason,
                });
            }
        }

        let passed = errors.is_empty();
        case_statuses.push(CaseVerifyStatus {
            case_id: case.id.clone(),
            repo_url: case.repo_url.clone(),
            pr_number: case.pr_number,
            parent_commit: case.parent_commit.clone(),
            media_count: case.media_urls.len(),
            passed,
            errors,
        });
    }

    let passed_cases = case_statuses.iter().filter(|s| s.passed).count();
    let failed_cases = case_statuses.len().saturating_sub(passed_cases);

    VerifyReport {
        total_cases: cases.len(),
        passed_cases,
        failed_cases,
        multimodal_cases,
        verified_media_urls: 0,
        checked_github: false,
        failures,
        case_statuses,
    }
}

#[derive(Debug, Deserialize)]
struct GithubPullVerifyResponse {
    merged_at: Option<String>,
    #[serde(default)]
    state: String,
}

fn build_github_api_client(github_token: Option<&str>) -> Result<reqwest::Client> {
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static("epicenter-eval/0.1"));
    headers.insert(
        ACCEPT,
        HeaderValue::from_static("application/vnd.github+json"),
    );
    if let Some(token) = github_token.map(str::trim).filter(|t| !t.is_empty()) {
        let auth = HeaderValue::from_str(&format!("Bearer {token}"))
            .context("invalid GITHUB_TOKEN header")?;
        headers.insert(AUTHORIZATION, auth);
    }
    reqwest::Client::builder()
        .default_headers(headers)
        .timeout(Duration::from_secs(20))
        .build()
        .context("failed to build GitHub API HTTP client")
}

/// Verifies that a URL returns HTTP 200 OK (using HEAD with fallback to streaming GET).
/// Returns `Ok(Option<content_type>)` on HTTP 200 OK.
pub async fn check_url_http_200(
    client: &reqwest::Client,
    url: &str,
) -> Result<Option<String>> {
    // Try HEAD first (cheap for large videos/GIFs).
    if let Ok(resp) = client.head(url).send().await {
        if resp.status() == StatusCode::OK {
            let ct = resp
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());
            return Ok(ct);
        }
    }

    // Some GitHub asset redirect targets / CDNs reject HEAD (403/405); check GET status without reading body.
    let resp = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("HTTP GET failed for '{url}'"))?;
    let status = resp.status();
    let ct = resp
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    drop(resp);

    if status != StatusCode::OK {
        bail!("expected HTTP 200 OK for '{url}', got {status}");
    }
    Ok(ct)
}

/// Runs full dataset verification (static checks + optional live GitHub REST API & media HTTP-200 verification).
pub async fn verify_cases(
    cases: &[EvalIssueCase],
    check_github: bool,
    github_token: Option<&str>,
) -> Result<VerifyReport> {
    let mut report = verify_cases_static(cases);
    if !check_github {
        return Ok(report);
    }

    report.checked_github = true;
    let gh_client = build_github_api_client(github_token)?;
    let media_client = reqwest::Client::builder()
        .user_agent("epicenter-eval/0.1")
        .timeout(Duration::from_secs(15))
        .build()
        .context("failed to build media verification HTTP client")?;

    let mut verified_media_urls = 0usize;

    for (idx, case) in cases.iter().enumerate() {
        let status_entry = &mut report.case_statuses[idx];
        let Ok((owner, repo)) = parse_github_repo_slug(&case.repo_url) else {
            continue;
        };

        // 1. Verify parent_commit exists in GitHub repo.
        let commit_url = format!(
            "https://api.github.com/repos/{owner}/{repo}/commits/{}",
            case.parent_commit
        );
        match gh_client.get(&commit_url).send().await {
            Ok(resp) if resp.status() == StatusCode::OK => {}
            Ok(resp) => {
                let reason = format!(
                    "parent_commit {} not found in {owner}/{repo} (HTTP {})",
                    case.parent_commit,
                    resp.status()
                );
                status_entry.errors.push(reason.clone());
                status_entry.passed = false;
                report.failures.push(VerifyFailure {
                    case_id: case.id.clone(),
                    field: "parent_commit".to_string(),
                    reason,
                });
            }
            Err(err) => {
                let reason = format!("GitHub commit check network error: {err}");
                status_entry.errors.push(reason.clone());
                status_entry.passed = false;
                report.failures.push(VerifyFailure {
                    case_id: case.id.clone(),
                    field: "parent_commit".to_string(),
                    reason,
                });
            }
        }

        // 2. Verify pr_number is a real merged PR in GitHub repo.
        if let Some(pr_num) = case.pr_number {
            let pr_url = format!("https://api.github.com/repos/{owner}/{repo}/pulls/{pr_num}");
            match gh_client.get(&pr_url).send().await {
                Ok(resp) if resp.status() == StatusCode::OK => {
                    match resp.json::<GithubPullVerifyResponse>().await {
                        Ok(pr) => {
                            if pr.merged_at.is_none() {
                                let reason = format!(
                                    "PR {owner}/{repo}#{pr_num} exists (state={}) but is NOT merged",
                                    pr.state
                                );
                                status_entry.errors.push(reason.clone());
                                status_entry.passed = false;
                                report.failures.push(VerifyFailure {
                                    case_id: case.id.clone(),
                                    field: "pr_number".to_string(),
                                    reason,
                                });
                            }
                        }
                        Err(err) => {
                            let reason = format!(
                                "failed to parse GitHub PR response for {owner}/{repo}#{pr_num}: {err}"
                            );
                            status_entry.errors.push(reason.clone());
                            status_entry.passed = false;
                            report.failures.push(VerifyFailure {
                                case_id: case.id.clone(),
                                field: "pr_number".to_string(),
                                reason,
                            });
                        }
                    }
                }
                Ok(resp) => {
                    let reason = format!(
                        "PR {owner}/{repo}#{pr_num} not found on GitHub (HTTP {})",
                        resp.status()
                    );
                    status_entry.errors.push(reason.clone());
                    status_entry.passed = false;
                    report.failures.push(VerifyFailure {
                        case_id: case.id.clone(),
                        field: "pr_number".to_string(),
                        reason,
                    });
                }
                Err(err) => {
                    let reason = format!("GitHub PR check network error: {err}");
                    status_entry.errors.push(reason.clone());
                    status_entry.passed = false;
                    report.failures.push(VerifyFailure {
                        case_id: case.id.clone(),
                        field: "pr_number".to_string(),
                        reason,
                    });
                }
            }
        }

        // 3. Verify every media_url returns HTTP 200 OK.
        for media_url in &case.media_urls {
            match check_url_http_200(&media_client, media_url).await {
                Ok(_) => {
                    verified_media_urls += 1;
                }
                Err(err) => {
                    let reason = format!("media_url failed HTTP 200 check: {err}");
                    status_entry.errors.push(reason.clone());
                    status_entry.passed = false;
                    report.failures.push(VerifyFailure {
                        case_id: case.id.clone(),
                        field: "media_urls".to_string(),
                        reason,
                    });
                }
            }
        }
    }

    report.verified_media_urls = verified_media_urls;
    report.passed_cases = report.case_statuses.iter().filter(|s| s.passed).count();
    report.failed_cases = report.case_statuses.len().saturating_sub(report.passed_cases);

    Ok(report)
}

/// Formats a human-readable verification summary and returns `Err` if any case failed.
pub fn ensure_verify_passed(report: &VerifyReport) -> Result<()> {
    if report.is_ok() {
        Ok(())
    } else {
        let mut msg = format!(
            "Dataset verification FAILED: {}/{} cases passed ({} failures)\n",
            report.passed_cases,
            report.total_cases,
            report.failures.len()
        );
        for f in &report.failures {
            msg.push_str(&format!("  - [{}] {}: {}\n", f.case_id, f.field, f.reason));
        }
        Err(anyhow!(msg))
    }
}
