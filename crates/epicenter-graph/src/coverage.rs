use crate::diff::normalize_rel_path;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoverageError {
    #[error("failed to read coverage file '{path}': {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse coverage.json: {0}")]
    Json(#[from] serde_json::Error),
}

/// Per-file line coverage data parsed from Python `coverage.json` or LCOV `lcov.info`.
#[derive(Debug, Clone, Default)]
pub struct FileCoverage {
    /// Map of 1-based line number -> execution hit count (`0` means uncovered).
    pub line_hits: HashMap<usize, u64>,
    /// Optional summary coverage percentage (`0.0..=100.0`) from the report.
    pub summary_pct: Option<f32>,
}

impl FileCoverage {
    /// Computes line coverage percentage over lines within `start_line..=end_line`.
    pub fn range_coverage_pct(&self, start_line: usize, end_line: usize) -> Option<f32> {
        let mut total = 0usize;
        let mut hit = 0usize;
        for (&line, &count) in &self.line_hits {
            if line >= start_line && line <= end_line {
                total += 1;
                if count > 0 {
                    hit += 1;
                }
            }
        }
        if total > 0 {
            return Some((hit as f32 / total as f32) * 100.0);
        }
        self.summary_pct
    }
}

/// Aggregated repository coverage overlay supporting Python `coverage.json` and LCOV.
#[derive(Debug, Clone, Default)]
pub struct CoverageMap {
    files: HashMap<String, FileCoverage>,
}

#[derive(Debug, Deserialize)]
struct PyCoverageReport {
    #[serde(default)]
    files: HashMap<String, PyFileEntry>,
}

#[derive(Debug, Deserialize)]
struct PyFileEntry {
    #[serde(default)]
    executed_lines: Vec<usize>,
    #[serde(default)]
    missing_lines: Vec<usize>,
    #[serde(default)]
    summary: Option<PyFileSummary>,
}

#[derive(Debug, Deserialize)]
struct PyFileSummary {
    #[serde(default)]
    percent_covered: Option<f32>,
}

impl CoverageMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// Parses Python `coverage.py` JSON format (`coverage json`).
    pub fn from_coverage_json(json_str: &str) -> Result<Self, CoverageError> {
        let parsed: PyCoverageReport = serde_json::from_str(json_str)?;
        let mut map = Self::new();
        for (raw_path, entry) in parsed.files {
            let norm = normalize_rel_path(&raw_path);
            let mut fc = FileCoverage {
                line_hits: HashMap::new(),
                summary_pct: entry.summary.and_then(|s| s.percent_covered),
            };
            for line in entry.executed_lines {
                fc.line_hits.insert(line, 1);
            }
            for line in entry.missing_lines {
                fc.line_hits.entry(line).or_insert(0);
            }
            map.merge_file(norm, fc);
        }
        Ok(map)
    }

    /// Parses an LCOV (`lcov.info`) report produced by c8, jest, vitest, pytest-cov, or cargo-llvm-cov.
    pub fn from_lcov(lcov_str: &str) -> Self {
        let mut map = Self::new();
        let mut current_path: Option<String> = None;
        let mut current_fc = FileCoverage::default();
        let mut lf: Option<usize> = None;
        let mut lh: Option<usize> = None;

        for raw_line in lcov_str.lines() {
            let line = raw_line.trim();
            if let Some(sf) = line.strip_prefix("SF:") {
                current_path = Some(normalize_rel_path(sf));
                current_fc = FileCoverage::default();
                lf = None;
                lh = None;
            } else if let Some(da) = line.strip_prefix("DA:") {
                let mut parts = da.split(',');
                if let (Some(ln_s), Some(hits_s)) = (parts.next(), parts.next()) {
                    if let (Ok(ln), Ok(hits)) = (ln_s.parse::<usize>(), hits_s.parse::<u64>()) {
                        let entry = current_fc.line_hits.entry(ln).or_insert(0);
                        *entry = (*entry).max(hits);
                    }
                }
            } else if let Some(v) = line.strip_prefix("LF:") {
                lf = v.parse().ok();
            } else if let Some(v) = line.strip_prefix("LH:") {
                lh = v.parse().ok();
            } else if line == "end_of_record" {
                if let Some(path) = current_path.take() {
                    if let (Some(found), Some(hit)) = (lf, lh) {
                        if found > 0 {
                            current_fc.summary_pct = Some((hit as f32 / found as f32) * 100.0);
                        }
                    }
                    map.merge_file(path, std::mem::take(&mut current_fc));
                }
            }
        }

        if let Some(path) = current_path.take() {
            map.merge_file(path, current_fc);
        }

        map
    }

    /// Loads coverage from disk, automatically detecting `coverage.json` vs LCOV.
    pub fn from_file(path: &Path) -> Result<Self, CoverageError> {
        let content = std::fs::read_to_string(path).map_err(|source| CoverageError::Io {
            path: path.display().to_string(),
            source,
        })?;
        let trimmed = content.trim_start();
        if trimmed.starts_with('{') {
            Self::from_coverage_json(&content)
        } else {
            Ok(Self::from_lcov(&content))
        }
    }

    /// Merges another `CoverageMap` into `self` (taking max hit counts per line).
    pub fn merge(&mut self, other: CoverageMap) {
        for (path, fc) in other.files {
            self.merge_file(path, fc);
        }
    }

    fn merge_file(&mut self, path: String, fc: FileCoverage) {
        let entry = self.files.entry(path).or_default();
        for (line, hits) in fc.line_hits {
            let slot = entry.line_hits.entry(line).or_insert(0);
            *slot = (*slot).max(hits);
        }
        if fc.summary_pct.is_some() {
            entry.summary_pct = fc.summary_pct;
        }
    }

    /// Queries coverage percentage for a symbol spanning `start_line..=end_line` in `file_path`.
    /// Handles exact relative path matches as well as suffix matches when coverage files contain
    /// absolute container paths (e.g. `/workspace/src/app.py` matching `src/app.py`).
    pub fn symbol_coverage_pct(
        &self,
        file_path: &str,
        start_line: usize,
        end_line: usize,
    ) -> Option<f32> {
        let norm = normalize_rel_path(file_path);
        if let Some(fc) = self.files.get(&norm) {
            return fc.range_coverage_pct(start_line, end_line);
        }
        let suffix = format!("/{norm}");
        for (cov_path, fc) in &self.files {
            if cov_path.ends_with(&suffix) || norm.ends_with(&format!("/{cov_path}")) {
                return fc.range_coverage_pct(start_line, end_line);
            }
        }
        None
    }
}
