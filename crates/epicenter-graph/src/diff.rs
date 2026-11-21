use regex::Regex;
use std::sync::OnceLock;

/// A single unified diff hunk within a modified file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffHunk {
    pub old_start: usize,
    pub old_count: usize,
    pub new_start: usize,
    pub new_count: usize,
    pub context_header: Option<String>,
    pub added_lines: Vec<usize>,
    pub deleted_lines: Vec<usize>,
}

impl DiffHunk {
    /// Returns the inclusive `(start, end)` line span in the new file touched by this hunk.
    pub fn touched_new_span(&self) -> (usize, usize) {
        if let (Some(&first), Some(&last)) = (self.added_lines.first(), self.added_lines.last()) {
            return (first, last);
        }
        let start = self.new_start.max(1);
        let end = if self.new_count == 0 {
            start
        } else {
            start + self.new_count.saturating_sub(1)
        };
        (start, end)
    }
}

/// Parsed unified diff for a single file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub old_path: Option<String>,
    pub new_path: Option<String>,
    pub path: String,
    pub is_new_file: bool,
    pub is_deleted_file: bool,
    pub hunks: Vec<DiffHunk>,
}

impl FileDiff {
    /// Returns all touched `(start_line, end_line)` spans in the post-patch file.
    pub fn touched_line_spans(&self) -> Vec<(usize, usize)> {
        self.hunks.iter().map(DiffHunk::touched_new_span).collect()
    }

    /// Returns true if any hunk or added/deleted anchor overlaps `start_line..=end_line`.
    pub fn touches_line_range(&self, start_line: usize, end_line: usize) -> bool {
        for hunk in &self.hunks {
            if !hunk.added_lines.is_empty()
                && hunk
                    .added_lines
                    .iter()
                    .any(|&line| line >= start_line && line <= end_line)
            {
                return true;
            }
            let (hs, he) = hunk.touched_new_span();
            if hs <= end_line && he >= start_line {
                return true;
            }
        }
        false
    }
}

fn hunk_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@(?:\s*(.*))?$")
            .expect("valid hunk header regex")
    })
}

/// Normalizes a git diff path (strips leading `a/`, `b/`, `./`, and converts `\` to `/`).
pub fn normalize_rel_path(raw: &str) -> String {
    let trimmed = raw.trim().trim_matches('"');
    let without_prefix = trimmed
        .strip_prefix("a/")
        .or_else(|| trimmed.strip_prefix("b/"))
        .unwrap_or(trimmed);
    let without_dot = without_prefix
        .strip_prefix("./")
        .unwrap_or(without_prefix);
    without_dot.replace('\\', "/")
}

/// Parses a standard unified `git diff` string into structured `FileDiff` entries.
pub fn parse_unified_diff(diff: &str) -> Vec<FileDiff> {
    let re_hunk = hunk_regex();
    let mut files: Vec<FileDiff> = Vec::new();
    let mut current: Option<FileDiff> = None;
    let mut current_hunk: Option<DiffHunk> = None;
    let mut old_line: usize = 0;
    let mut new_line: usize = 0;

    let flush_hunk = |file: &mut Option<FileDiff>, hunk: &mut Option<DiffHunk>| {
        if let (Some(f), Some(h)) = (file.as_mut(), hunk.take()) {
            f.hunks.push(h);
        }
    };

    let flush_file = |files: &mut Vec<FileDiff>,
                      file: &mut Option<FileDiff>,
                      hunk: &mut Option<DiffHunk>| {
        flush_hunk(file, hunk);
        if let Some(mut f) = file.take() {
            if f.path.is_empty() {
                if let Some(ref np) = f.new_path {
                    f.path = np.clone();
                } else if let Some(ref op) = f.old_path {
                    f.path = op.clone();
                }
            }
            if !f.path.is_empty() {
                files.push(f);
            }
        }
    };

    for raw_line in diff.lines() {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);

        if let Some(rest) = line.strip_prefix("diff --git ") {
            flush_file(&mut files, &mut current, &mut current_hunk);
            let mut parts = rest.split_whitespace();
            let old_p = parts.next().map(normalize_rel_path);
            let new_p = parts.next().map(normalize_rel_path);
            let path = new_p
                .clone()
                .or_else(|| old_p.clone())
                .unwrap_or_default();
            current = Some(FileDiff {
                old_path: old_p,
                new_path: new_p,
                path,
                is_new_file: false,
                is_deleted_file: false,
                hunks: Vec::new(),
            });
            continue;
        }

        if line.starts_with("new file mode") {
            if let Some(f) = current.as_mut() {
                f.is_new_file = true;
            }
            continue;
        }

        if line.starts_with("deleted file mode") {
            if let Some(f) = current.as_mut() {
                f.is_deleted_file = true;
            }
            continue;
        }

        if let Some(rest) = line.strip_prefix("--- ") {
            // Handle bare unified diffs without a `diff --git` header
            if current.is_none() {
                current = Some(FileDiff {
                    old_path: None,
                    new_path: None,
                    path: String::new(),
                    is_new_file: false,
                    is_deleted_file: false,
                    hunks: Vec::new(),
                });
            }
            if let Some(f) = current.as_mut() {
                if rest.trim() == "/dev/null" {
                    f.is_new_file = true;
                    f.old_path = None;
                } else {
                    let path_part = rest.split('\t').next().unwrap_or(rest);
                    f.old_path = Some(normalize_rel_path(path_part));
                }
            }
            continue;
        }

        if let Some(rest) = line.strip_prefix("+++ ") {
            if let Some(f) = current.as_mut() {
                if rest.trim() == "/dev/null" {
                    f.is_deleted_file = true;
                    f.new_path = None;
                } else {
                    let path_part = rest.split('\t').next().unwrap_or(rest);
                    let norm = normalize_rel_path(path_part);
                    f.new_path = Some(norm.clone());
                    f.path = norm;
                }
            }
            continue;
        }

        if let Some(caps) = re_hunk.captures(line) {
            flush_hunk(&mut current, &mut current_hunk);
            let old_start: usize = caps[1].parse().unwrap_or(1);
            let old_count: usize = caps
                .get(2)
                .and_then(|m| m.as_str().parse().ok())
                .unwrap_or(1);
            let new_start: usize = caps[3].parse().unwrap_or(1);
            let new_count: usize = caps
                .get(4)
                .and_then(|m| m.as_str().parse().ok())
                .unwrap_or(1);
            let context_header = caps
                .get(5)
                .map(|m| m.as_str().trim().to_string())
                .filter(|s| !s.is_empty());

            old_line = old_start;
            new_line = new_start;
            current_hunk = Some(DiffHunk {
                old_start,
                old_count,
                new_start,
                new_count,
                context_header,
                added_lines: Vec::new(),
                deleted_lines: Vec::new(),
            });
            continue;
        }

        if let Some(hunk) = current_hunk.as_mut() {
            if line.starts_with('+') && !line.starts_with("+++") {
                hunk.added_lines.push(new_line);
                new_line += 1;
            } else if line.starts_with('-') && !line.starts_with("---") {
                hunk.deleted_lines.push(old_line);
                old_line += 1;
            } else if line.starts_with(' ') || line.is_empty() {
                old_line += 1;
                new_line += 1;
            }
        }
    }

    flush_file(&mut files, &mut current, &mut current_hunk);
    files
}
