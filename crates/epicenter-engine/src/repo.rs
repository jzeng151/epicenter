use epicenter_common::{LiveEvent, LogLevel};
use epicenter_graph::{normalize_rel_path, parse_unified_diff};
use serde::Deserialize;
use std::collections::BTreeMap;
use tokio::sync::broadcast;
use tracing::warn;
use uuid::Uuid;

/// Maximum number of source files fetched over GitHub raw content API per tournament session.
const MAX_FETCHED_FILES: usize = 45;
/// Maximum byte size of an individual source file fetched into memory.
pub(crate) const MAX_FILE_BYTES: usize = 120_000;
/// Maximum number of file tree entries forwarded to LLM prompts.
pub const MAX_TREE_PROMPT_ENTRIES: usize = 250;

const SOURCE_EXTENSIONS: &[&str] = &[
    "py", "ts", "tsx", "js", "jsx", "mjs", "cjs", "mts", "cts", "rs", "go", "java", "kt", "rb",
    "cs", "cpp", "cc", "c", "h", "hpp", "swift", "scala", "php",
];

const IGNORED_PATH_SEGMENTS: &[&str] = &[
    "node_modules/",
    "target/",
    "dist/",
    "build/",
    ".venv/",
    "venv/",
    "__pycache__/",
    "vendor/",
    ".git/",
    ".next/",
    "coverage/",
    "migrations/",
];

/// Parsed GitHub repository owner and name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubRepoId {
    pub owner: String,
    pub repo: String,
}

/// Snapshot of a GitHub repository's file tree and fetched source files.
#[derive(Debug, Clone, Default)]
pub struct RepoSnapshot {
    pub owner: String,
    pub repo: String,
    pub resolved_ref: String,
    pub file_tree: Vec<String>,
    pub sources: BTreeMap<String, String>,
}

impl RepoSnapshot {
    /// Returns a bounded, prompt-friendly listing of the repository's file tree.
    pub fn prompt_tree_listing(&self) -> String {
        if self.file_tree.is_empty() {
            return "(repository tree unavailable)".to_string();
        }
        let mut lines: Vec<&str> = self
            .file_tree
            .iter()
            .take(MAX_TREE_PROMPT_ENTRIES)
            .map(String::as_str)
            .collect();
        let truncated = self.file_tree.len().saturating_sub(lines.len());
        let mut out = lines.join("\n");
        if truncated > 0 {
            out.push_str(&format!("\n... ({truncated} more files)"));
        }
        lines.clear();
        out
    }

    /// Formats the most relevant source files (prioritizing `suspect_files`) into a bounded prompt excerpt.
    pub fn prompt_source_context(&self, suspect_files: &[String], max_chars: usize) -> String {
        if self.sources.is_empty() {
            return "(no source files fetched)".to_string();
        }
        let mut ordered: Vec<(&String, &String)> = Vec::new();
        let mut included = std::collections::HashSet::new();

        for suspect in suspect_files {
            let norm = normalize_rel_path(suspect);
            for (path, content) in &self.sources {
                if (path == &norm || path.ends_with(&format!("/{norm}")))
                    && included.insert(path.as_str())
                {
                    ordered.push((path, content));
                }
            }
        }
        for (path, content) in &self.sources {
            if !is_test_like_path(path) && included.insert(path.as_str()) {
                ordered.push((path, content));
            }
        }
        for (path, content) in &self.sources {
            if included.insert(path.as_str()) {
                ordered.push((path, content));
            }
        }

        let mut buf = String::new();
        for (path, content) in ordered {
            let section = format!("--- FILE: {path} ---\n{content}\n\n");
            if buf.len() + section.len() > max_chars {
                let remaining = max_chars.saturating_sub(buf.len());
                if remaining > 200 {
                    buf.push_str(&truncate_str(&section, remaining));
                    buf.push_str("\n...(truncated)\n");
                }
                break;
            }
            buf.push_str(&section);
        }
        buf
    }

    /// Formats 1–3 representative existing test files (plus `conftest.py` if present) so Stage 2
    /// Red-Gate sees real repository test conventions, imports, and fixtures.
    pub fn prompt_test_examples(&self, suspect_files: &[String], max_chars: usize) -> String {
        let stems: Vec<String> = suspect_files
            .iter()
            .filter_map(|s| {
                let norm = normalize_rel_path(s);
                let file = norm.rsplit('/').next()?;
                let stem = file.split('.').next()?;
                if stem.len() >= 3 {
                    Some(stem.to_ascii_lowercase())
                } else {
                    None
                }
            })
            .collect();

        let mut scored: Vec<(i32, &String, &String)> = self
            .sources
            .iter()
            .filter(|(p, _)| is_test_like_path(p) || p.ends_with("conftest.py"))
            .map(|(p, c)| {
                let lower = p.to_ascii_lowercase();
                let mut score = 10;
                if lower.ends_with("conftest.py") {
                    score += 80;
                }
                for stem in &stems {
                    if lower.contains(stem) {
                        score += 50;
                    }
                }
                if c.len() <= 8_000 {
                    score += 15;
                }
                (score, p, c)
            })
            .collect();

        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1)));
        if scored.is_empty() {
            return "(no existing test files loaded)".to_string();
        }

        let mut buf = String::new();
        for (_, path, content) in scored.into_iter().take(3) {
            let section = format!("--- TEST EXAMPLE: {path} ---\n{content}\n\n");
            if buf.len() + section.len() > max_chars {
                let remaining = max_chars.saturating_sub(buf.len());
                if remaining > 200 {
                    buf.push_str(&truncate_str(&section, remaining));
                    buf.push_str("\n...(truncated)\n");
                }
                break;
            }
            buf.push_str(&section);
        }
        buf
    }
}

pub(crate) fn is_test_like_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let file = lower.rsplit('/').next().unwrap_or(&lower);
    lower.starts_with("tests/")
        || lower.starts_with("test/")
        || lower.contains("/tests/")
        || lower.contains("/test/")
        || lower.contains("__tests__/")
        || file.starts_with("test_")
        || file.ends_with("_test.py")
        || file.ends_with("_test.go")
        || file.ends_with("_test.rs")
        || file.contains(".test.")
        || file.contains(".spec.")
}

/// Parses a GitHub repository URL (`https://github.com/owner/repo[.git]` or `git@github.com:owner/repo.git`).
pub fn parse_github_repo_url(repo_url: &str) -> Option<GitHubRepoId> {
    let trimmed = repo_url.trim().trim_end_matches('/');
    let rest = if let Some(s) = trimmed.strip_prefix("git@github.com:") {
        s
    } else if let Some(idx) = trimmed.find("github.com/") {
        &trimmed[idx + "github.com/".len()..]
    } else {
        return None;
    };

    let mut parts = rest.split('/').filter(|p| !p.is_empty());
    let owner = parts.next()?.to_string();
    let repo_raw = parts.next()?;
    let repo = repo_raw
        .strip_suffix(".git")
        .unwrap_or(repo_raw)
        .to_string();

    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some(GitHubRepoId { owner, repo })
}

/// Normalizes a repository URL into a cloneable HTTPS `.git` URL when pointing at GitHub.
pub fn normalize_clone_url(repo_url: &str) -> String {
    if let Some(id) = parse_github_repo_url(repo_url) {
        format!("https://github.com/{}/{}.git", id.owner, id.repo)
    } else {
        repo_url.trim().to_string()
    }
}

#[derive(Debug, Deserialize)]
struct GitHubRepoMeta {
    default_branch: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GitHubTreeResponse {
    #[serde(default)]
    tree: Vec<GitHubTreeEntry>,
}

#[derive(Debug, Deserialize)]
struct GitHubTreeEntry {
    path: String,
    #[serde(rename = "type")]
    entry_type: String,
    #[serde(default)]
    size: Option<usize>,
}

/// Fetches a repository's recursive file tree and key source files via GitHub REST + raw content APIs.
pub async fn fetch_github_repo_snapshot(
    repo_url: &str,
    commit_sha: Option<&str>,
    git_ref: Option<&str>,
    github_token: Option<&str>,
    priority_hints: &[String],
) -> anyhow::Result<RepoSnapshot> {
    let Some(repo_id) = parse_github_repo_url(repo_url) else {
        return Ok(RepoSnapshot::default());
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(25))
        .user_agent("epicenter-engine/0.1")
        .build()?;

    let resolved_ref = resolve_target_ref(&client, &repo_id, commit_sha, git_ref, github_token).await;

    let tree_url = format!(
        "https://api.github.com/repos/{}/{}/git/trees/{}?recursive=1",
        repo_id.owner, repo_id.repo, resolved_ref
    );
    let mut req = client
        .get(&tree_url)
        .header("Accept", "application/vnd.github+json");
    if let Some(token) = github_token.filter(|t| !t.trim().is_empty()) {
        req = req.header("Authorization", format!("Bearer {}", token.trim()));
    }

    let resp = req.send().await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!(
            "GitHub tree API returned HTTP {} for {}/{}@{}: {}",
            status,
            repo_id.owner,
            repo_id.repo,
            resolved_ref,
            truncate_str(&body, 240)
        );
    }

    let tree_resp: GitHubTreeResponse = resp.json().await?;
    let mut file_tree = Vec::new();
    let mut fetch_candidates: Vec<(i32, String)> = Vec::new();

    for entry in tree_resp.tree {
        if entry.entry_type != "blob" {
            continue;
        }
        let norm = normalize_rel_path(&entry.path);
        if is_ignored_path(&norm) {
            continue;
        }
        file_tree.push(norm.clone());

        if entry.size.unwrap_or(0) > MAX_FILE_BYTES {
            continue;
        }
        if is_source_file(&norm) {
            let score = file_priority_score(&norm, priority_hints);
            fetch_candidates.push((score, norm));
        }
    }

    fetch_candidates.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    fetch_candidates.truncate(MAX_FETCHED_FILES);

    let sources = fetch_raw_files(
        &client,
        &repo_id,
        &resolved_ref,
        github_token,
        &fetch_candidates
            .into_iter()
            .map(|(_, path)| path)
            .collect::<Vec<_>>(),
    )
    .await;

    Ok(RepoSnapshot {
        owner: repo_id.owner,
        repo: repo_id.repo,
        resolved_ref,
        file_tree,
        sources,
    })
}

/// Fetches additional suspect files discovered during Stage 1 Intake and merges them into `snapshot.sources`.
pub async fn enrich_snapshot_with_suspects(
    snapshot: &mut RepoSnapshot,
    suspect_files: &[String],
    github_token: Option<&str>,
) {
    if snapshot.owner.is_empty() || snapshot.repo.is_empty() || suspect_files.is_empty() {
        return;
    }
    let missing: Vec<String> = suspect_files
        .iter()
        .map(|s| normalize_rel_path(s))
        .filter(|p| !p.is_empty() && !snapshot.sources.contains_key(p))
        .take(15)
        .collect();
    if missing.is_empty() {
        return;
    }

    let Ok(client) = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .user_agent("epicenter-engine/0.1")
        .build()
    else {
        return;
    };

    let repo_id = GitHubRepoId {
        owner: snapshot.owner.clone(),
        repo: snapshot.repo.clone(),
    };
    let fetched = fetch_raw_files(
        &client,
        &repo_id,
        &snapshot.resolved_ref,
        github_token,
        &missing,
    )
    .await;
    for (k, v) in fetched {
        snapshot.sources.insert(k, v);
    }
}

async fn resolve_target_ref(
    client: &reqwest::Client,
    repo_id: &GitHubRepoId,
    commit_sha: Option<&str>,
    git_ref: Option<&str>,
    github_token: Option<&str>,
) -> String {
    if let Some(sha) = commit_sha.map(str::trim).filter(|s| !s.is_empty()) {
        return sha.to_string();
    }
    if let Some(r) = git_ref.map(str::trim).filter(|s| !s.is_empty()) {
        return r.to_string();
    }

    let meta_url = format!(
        "https://api.github.com/repos/{}/{}",
        repo_id.owner, repo_id.repo
    );
    let mut req = client
        .get(&meta_url)
        .header("Accept", "application/vnd.github+json");
    if let Some(token) = github_token.filter(|t| !t.trim().is_empty()) {
        req = req.header("Authorization", format!("Bearer {}", token.trim()));
    }
    if let Ok(resp) = req.send().await {
        if resp.status().is_success() {
            if let Ok(meta) = resp.json::<GitHubRepoMeta>().await {
                if let Some(branch) = meta.default_branch.filter(|b| !b.is_empty()) {
                    return branch;
                }
            }
        }
    }
    "HEAD".to_string()
}

async fn fetch_raw_files(
    client: &reqwest::Client,
    repo_id: &GitHubRepoId,
    git_ref: &str,
    github_token: Option<&str>,
    paths: &[String],
) -> BTreeMap<String, String> {
    let futs = paths.iter().map(|path| {
        let client = client.clone();
        let owner = repo_id.owner.clone();
        let repo = repo_id.repo.clone();
        let git_ref = git_ref.to_string();
        let token = github_token.map(String::from);
        let path = path.clone();

        async move {
            let url = format!(
                "https://raw.githubusercontent.com/{owner}/{repo}/{git_ref}/{path}"
            );
            let mut req = client.get(&url);
            if let Some(tok) = token.as_deref().filter(|t| !t.trim().is_empty()) {
                req = req.header("Authorization", format!("Bearer {}", tok.trim()));
            }
            match req.send().await {
                Ok(resp) if resp.status().is_success() => match resp.text().await {
                    Ok(body) if body.len() <= MAX_FILE_BYTES => Some((path, body)),
                    _ => None,
                },
                Ok(resp) => {
                    warn!(path = %path, status = resp.status().as_u16(), "failed to fetch raw source file");
                    None
                }
                Err(err) => {
                    warn!(path = %path, error = %err, "transport error fetching raw source file");
                    None
                }
            }
        }
    });

    let results = futures::future::join_all(futs).await;
    results.into_iter().flatten().collect()
}

pub(crate) fn is_ignored_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    IGNORED_PATH_SEGMENTS
        .iter()
        .any(|seg| lower.starts_with(seg) || lower.contains(&format!("/{seg}")))
}

pub(crate) fn is_source_file(path: &str) -> bool {
    let Some(ext) = path.rsplit('.').next() else {
        return false;
    };
    SOURCE_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str())
}

pub(crate) fn file_priority_score(path: &str, hints: &[String]) -> i32 {
    let lower = path.to_ascii_lowercase();
    let mut score = 10;
    for hint in hints {
        let norm_hint = normalize_rel_path(hint).to_ascii_lowercase();
        if !norm_hint.is_empty() && (lower == norm_hint || lower.contains(&norm_hint)) {
            score += 100;
        }
    }
    if lower.starts_with("src/") || lower.starts_with("app/") || lower.starts_with("lib/") || lower.starts_with("pkg/") {
        score += 25;
    }
    if lower.contains("test") || lower.contains("spec") {
        score -= 5;
    }
    score
}

/// Emits a `LiveEvent` onto the broadcast channel if a listener is attached.
pub fn emit_event(tx: &Option<broadcast::Sender<LiveEvent>>, event: LiveEvent) {
    if let Some(sender) = tx {
        let _ = sender.send(event);
    }
}

/// Emits a structured log `LiveEvent`.
pub fn emit_log(
    tx: &Option<broadcast::Sender<LiveEvent>>,
    session_id: Uuid,
    level: LogLevel,
    source: impl Into<String>,
    message: impl Into<String>,
) {
    emit_event(
        tx,
        LiveEvent::Log {
            session_id,
            level,
            source: source.into(),
            message: message.into(),
            timestamp: chrono::Utc::now(),
        },
    );
}

/// Safely truncates a UTF-8 string to at most `max_bytes` along a valid char boundary.
pub fn truncate_str(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// Extracts a JSON payload of type `T` from raw model or agent text, tolerating Markdown code fences
/// and surrounding prose.
pub fn extract_json_from_text<T: serde::de::DeserializeOwned>(raw: &str) -> anyhow::Result<T> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        anyhow::bail!("empty text response; expected JSON payload");
    }

    // 1. Direct parse
    if let Ok(parsed) = serde_json::from_str::<T>(trimmed) {
        return Ok(parsed);
    }

    // 2. Fenced ```json ... ``` or ``` ... ``` blocks (try last block first as agents put final JSON at the end)
    let mut fenced_blocks = Vec::new();
    let mut search = trimmed;
    while let Some(start_idx) = search.find("```") {
        let after_ticks = &search[start_idx + 3..];
        let after_lang = after_ticks
            .find('\n')
            .map(|nl| &after_ticks[nl + 1..])
            .unwrap_or(after_ticks);
        if let Some(end_idx) = after_lang.find("```") {
            fenced_blocks.push(after_lang[..end_idx].trim());
            search = &after_lang[end_idx + 3..];
        } else {
            break;
        }
    }
    for block in fenced_blocks.iter().rev() {
        if let Ok(parsed) = serde_json::from_str::<T>(block) {
            return Ok(parsed);
        }
    }

    // 3. Scan balanced JSON object `{ ... }` or array `[ ... ]` candidates (preferring the last valid one)
    for &(open, close) in &[('{', '}'), ('[', ']')] {
        let candidates = extract_balanced_spans(trimmed, open, close);
        for span in candidates.into_iter().rev() {
            if let Ok(parsed) = serde_json::from_str::<T>(span) {
                return Ok(parsed);
            }
        }
    }

    anyhow::bail!(
        "failed to extract valid JSON from response: {}",
        truncate_str(trimmed, 240)
    )
}

fn extract_balanced_spans(input: &str, open: char, close: char) -> Vec<&str> {
    let mut spans = Vec::new();
    let mut depth = 0usize;
    let mut start: Option<usize> = None;
    let mut in_str = false;
    let mut escaped = false;

    for (idx, ch) in input.char_indices() {
        if in_str {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_str = false;
            }
            continue;
        }
        if ch == '"' {
            in_str = true;
            continue;
        }
        if ch == open {
            if depth == 0 {
                start = Some(idx);
            }
            depth += 1;
        } else if ch == close && depth > 0 {
            depth -= 1;
            if depth == 0 {
                if let Some(s) = start.take() {
                    spans.push(&input[s..=idx]);
                }
            }
        }
    }
    spans
}

/// Extracts a unified `git diff` from raw LLM or agent output (stripping markdown ```diff fences).
pub fn extract_unified_diff_from_text(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    // Prefer fenced diff/patch block if present
    let mut search = trimmed;
    while let Some(start_idx) = search.find("```") {
        let after = &search[start_idx + 3..];
        let (lang, rest) = match after.find('\n') {
            Some(nl) => (after[..nl].trim().to_ascii_lowercase(), &after[nl + 1..]),
            None => (String::new(), after),
        };
        if let Some(end_idx) = rest.find("```") {
            let block = rest[..end_idx].trim();
            if (lang == "diff" || lang == "patch" || block.contains("diff --git") || block.contains("+++ "))
                && !block.is_empty()
            {
                return block.to_string();
            }
            search = &rest[end_idx + 3..];
        } else {
            break;
        }
    }

    if let Some(idx) = trimmed.find("diff --git ") {
        return trimmed[idx..].trim().to_string();
    }
    if let Some(idx) = trimmed.find("--- a/") {
        return trimmed[idx..].trim().to_string();
    }
    String::new()
}

/// Strips any file section from a unified `git diff` that touches `locked_test_path`.
/// Returns `(sanitized_diff, stripped_count)`.
pub fn strip_locked_test_from_diff(diff: &str, locked_test_path: &str) -> (String, usize) {
    let target = normalize_rel_path(locked_test_path);
    if target.is_empty() || diff.trim().is_empty() {
        return (diff.to_string(), 0);
    }

    let mut sections: Vec<String> = Vec::new();
    let mut current = String::new();

    for line in diff.lines() {
        if line.starts_with("diff --git ") && !current.is_empty() {
            sections.push(std::mem::take(&mut current));
        }
        current.push_str(line);
        current.push('\n');
    }
    if !current.is_empty() {
        sections.push(current);
    }

    // Handle bare unified diffs (multiple `--- ` headers without `diff --git`)
    if sections.len() == 1 && !sections[0].starts_with("diff --git ") {
        let mut bare_sections: Vec<String> = Vec::new();
        let mut cur = String::new();
        for line in diff.lines() {
            if line.starts_with("--- ") && !cur.is_empty() {
                bare_sections.push(std::mem::take(&mut cur));
            }
            cur.push_str(line);
            cur.push('\n');
        }
        if !cur.is_empty() {
            bare_sections.push(cur);
        }
        sections = bare_sections;
    }

    let mut kept = Vec::new();
    let mut stripped = 0usize;

    for section in sections {
        let parsed = parse_unified_diff(&section);
        let touches_locked = parsed.iter().any(|f| {
            let paths = [
                Some(f.path.as_str()),
                f.old_path.as_deref(),
                f.new_path.as_deref(),
            ];
            paths.into_iter().flatten().any(|p| {
                let norm = normalize_rel_path(p);
                norm == target
                    || norm.ends_with(&format!("/{target}"))
                    || target.ends_with(&format!("/{norm}"))
            })
        });
        if touches_locked {
            stripped += 1;
        } else {
            kept.push(section);
        }
    }

    (kept.join("").trim_end().to_string(), stripped)
}

/// Returns true if a unified diff modifies or adds any test file outside `locked_test_path`.
pub fn diff_adds_caller_tests(diff: &str, locked_test_path: Option<&str>) -> bool {
    let locked = locked_test_path.map(normalize_rel_path);
    parse_unified_diff(diff).iter().any(|f| {
        let norm = normalize_rel_path(&f.path);
        if let Some(ref lp) = locked {
            if &norm == lp {
                return false;
            }
        }
        let lower = norm.to_ascii_lowercase();
        lower.contains("test") || lower.contains("spec")
    })
}

/// Standard network domain allowlist for Managed Agent remote sandboxes.
pub fn default_sandbox_allowlist() -> Vec<String> {
    vec![
        "github.com".into(),
        "api.github.com".into(),
        "raw.githubusercontent.com".into(),
        "pypi.org".into(),
        "files.pythonhosted.org".into(),
        "registry.npmjs.org".into(),
        "crates.io".into(),
        "static.crates.io".into(),
        "index.crates.io".into(),
        "proxy.golang.org".into(),
        "sum.golang.org".into(),
    ]
}
