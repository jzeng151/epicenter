use crate::coverage::CoverageMap;
use crate::diff::{normalize_rel_path, parse_unified_diff, FileDiff};
use crate::extract::{extract_file_symbols, SymbolDef};
use epicenter_common::{BlastRadiusReport, EdgeRelation, GraphEdge, GraphNode, ImpactRing};
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::Direction;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::f32::consts::{FRAC_PI_2, TAU};
use std::path::Path;
use thiserror::Error;
use walkdir::WalkDir;

const IGNORED_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    ".venv",
    "venv",
    "__pycache__",
    "dist",
    "build",
    ".next",
    ".mypy_cache",
    ".pytest_cache",
];

const SUPPORTED_EXTENSIONS: &[&str] = &[
    "py", "ts", "tsx", "js", "jsx", "mjs", "cjs", "mts", "cts", "rs", "go", "java", "kt", "rb",
    "cs", "cpp", "c", "h", "hpp",
];

#[derive(Debug, Error)]
pub enum GraphError {
    #[error("failed to walk repository directory '{path}': {source}")]
    Walk {
        path: String,
        #[source]
        source: walkdir::Error,
    },
    #[error("failed to read source file '{path}': {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

/// Configuration knobs for blast-radius analysis.
#[derive(Debug, Clone)]
pub struct BlastAnalysisConfig {
    /// Maximum BFS reverse-hop depth grouped into Ring 2 (default: 3).
    pub max_ring2_depth: usize,
    /// Radius for Ring 0 when multiple nodes are mutated (`0.0..=60.0`).
    pub ring0_max_radius: f32,
    /// Concentric radius for Ring 1 direct callers.
    pub ring1_radius: f32,
    /// Concentric radius for Ring 2 transitive entrypoints.
    pub ring2_radius: f32,
}

impl Default for BlastAnalysisConfig {
    fn default() -> Self {
        Self {
            max_ring2_depth: 3,
            ring0_max_radius: 56.0,
            ring1_radius: 160.0,
            ring2_radius: 280.0,
        }
    }
}

/// Directed repository call & import graph powered by `petgraph`.
#[derive(Debug, Clone)]
pub struct RepoCallGraph {
    graph: DiGraph<SymbolDef, EdgeRelation>,
    id_to_idx: HashMap<String, NodeIndex>,
}

impl RepoCallGraph {
    /// Scans a repository directory on disk and builds its symbol call/import graph.
    pub fn from_directory(repo_root: &Path) -> Result<Self, GraphError> {
        let mut files: BTreeMap<String, String> = BTreeMap::new();
        for entry in WalkDir::new(repo_root)
            .into_iter()
            .filter_entry(|e| {
                let name = e.file_name().to_string_lossy();
                !(e.file_type().is_dir() && IGNORED_DIRS.contains(&name.as_ref()))
            })
        {
            let entry = entry.map_err(|source| GraphError::Walk {
                path: repo_root.display().to_string(),
                source,
            })?;
            if !entry.file_type().is_file() {
                continue;
            }
            let Some(ext) = entry.path().extension().and_then(|s| s.to_str()) else {
                continue;
            };
            if !SUPPORTED_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()) {
                continue;
            }
            let rel = entry
                .path()
                .strip_prefix(repo_root)
                .unwrap_or(entry.path())
                .to_string_lossy();
            let norm = normalize_rel_path(&rel);
            let content = std::fs::read_to_string(entry.path()).map_err(|source| GraphError::Io {
                path: entry.path().display().to_string(),
                source,
            })?;
            files.insert(norm, content);
        }
        Ok(Self::from_sources(&files))
    }

    /// Builds a call/import graph from an in-memory map of `relative_path -> source_code`.
    pub fn from_sources(sources: &BTreeMap<String, String>) -> Self {
        let mut graph = DiGraph::<SymbolDef, EdgeRelation>::new();
        let mut id_to_idx: HashMap<String, NodeIndex> = HashMap::new();
        let mut by_short_name: HashMap<String, Vec<NodeIndex>> = HashMap::new();
        let mut by_qualified_name: HashMap<String, Vec<NodeIndex>> = HashMap::new();
        let mut by_file_stem: HashMap<String, Vec<NodeIndex>> = HashMap::new();

        for (raw_path, source) in sources {
            let norm = normalize_rel_path(raw_path);
            let extracted = extract_file_symbols(&norm, source);
            let stem = file_stem(&norm);

            for sym in extracted.symbols {
                if id_to_idx.contains_key(&sym.id) {
                    continue;
                }
                let id = sym.id.clone();
                let short = sym.name.clone();
                let qname = sym.qualified_name.clone();
                let is_mod = sym.is_module;
                let idx = graph.add_node(sym);
                id_to_idx.insert(id, idx);
                if !is_mod {
                    by_short_name.entry(short).or_default().push(idx);
                    by_qualified_name.entry(qname).or_default().push(idx);
                }
                if !stem.is_empty() {
                    by_file_stem.entry(stem.clone()).or_default().push(idx);
                }
            }
        }

        // Resolve call & import edges deterministically.
        let node_indices: Vec<NodeIndex> = graph.node_indices().collect();
        let mut added_edges: HashSet<(NodeIndex, NodeIndex, EdgeRelation)> = HashSet::new();

        for caller_idx in node_indices {
            let caller = graph[caller_idx].clone();
            let import_set: HashSet<&str> = caller.imports.iter().map(String::as_str).collect();

            for callee_raw in &caller.calls {
                let candidates = resolve_callee_candidates(
                    callee_raw,
                    &caller.file_path,
                    &import_set,
                    &graph,
                    &by_short_name,
                    &by_qualified_name,
                );
                for callee_idx in candidates {
                    if callee_idx == caller_idx {
                        continue;
                    }
                    // Avoid linking a class container node to its own method or vice versa
                    if graph[callee_idx].file_path == caller.file_path
                        && (graph[callee_idx]
                            .qualified_name
                            .starts_with(&format!("{}.", caller.qualified_name))
                            || caller
                                .qualified_name
                                .starts_with(&format!("{}.", graph[callee_idx].qualified_name)))
                    {
                        continue;
                    }
                    if added_edges.insert((caller_idx, callee_idx, EdgeRelation::Calls)) {
                        graph.add_edge(caller_idx, callee_idx, EdgeRelation::Calls);
                    }
                }
            }

            // Link explicit symbol imports when no direct call edge already connects them.
            for imp in &caller.imports {
                if let Some(targets) = by_short_name.get(imp) {
                    for &target_idx in targets {
                        if target_idx == caller_idx
                            || graph[target_idx].file_path == caller.file_path
                        {
                            continue;
                        }
                        let already_called =
                            added_edges.contains(&(caller_idx, target_idx, EdgeRelation::Calls));
                        if !already_called
                            && caller.is_module
                            && added_edges.insert((caller_idx, target_idx, EdgeRelation::Imports))
                        {
                            graph.add_edge(caller_idx, target_idx, EdgeRelation::Imports);
                        }
                    }
                }
            }
        }

        Self { graph, id_to_idx }
    }

    pub fn node_count(&self) -> usize {
        self.graph.node_count()
    }

    pub fn edge_count(&self) -> usize {
        self.graph.edge_count()
    }

    /// Computes a deterministic `BlastRadiusReport` from a unified `git diff` and optional `CoverageMap`.
    pub fn analyze_diff(
        &self,
        unified_diff: &str,
        coverage: Option<&CoverageMap>,
        config: &BlastAnalysisConfig,
    ) -> BlastRadiusReport {
        let diffs = parse_unified_diff(unified_diff);
        self.analyze_parsed_diffs(&diffs, coverage, config)
    }

    /// Computes a deterministic `BlastRadiusReport` from pre-parsed `FileDiff` items.
    pub fn analyze_parsed_diffs(
        &self,
        diffs: &[FileDiff],
        coverage: Option<&CoverageMap>,
        config: &BlastAnalysisConfig,
    ) -> BlastRadiusReport {
        let mut ring0_set: HashSet<NodeIndex> = HashSet::new();
        let mut touched_files: HashSet<String> = HashSet::new();

        for file_diff in diffs {
            let norm_path = normalize_rel_path(&file_diff.path);
            touched_files.insert(norm_path.clone());

            let mut file_non_module_hits = Vec::new();
            let mut file_module_node = None;

            for idx in self.graph.node_indices() {
                let sym = &self.graph[idx];
                if sym.file_path != norm_path {
                    continue;
                }
                if sym.is_module {
                    file_module_node = Some(idx);
                } else if file_diff.touches_line_range(sym.start_line, sym.end_line) {
                    file_non_module_hits.push(idx);
                }
            }

            if !file_non_module_hits.is_empty() {
                // If both a class and a specific method inside it matched, keep the narrower symbol(s)
                // when specific methods were touched.
                let specific: Vec<NodeIndex> = file_non_module_hits
                    .iter()
                    .copied()
                    .filter(|&idx| {
                        let s = &self.graph[idx];
                        !file_non_module_hits.iter().any(|&other| {
                            other != idx
                                && self.graph[other].start_line >= s.start_line
                                && self.graph[other].end_line <= s.end_line
                                && (self.graph[other].end_line - self.graph[other].start_line)
                                    < (s.end_line - s.start_line)
                        })
                    })
                    .collect();
                for idx in specific {
                    ring0_set.insert(idx);
                }
            } else if let Some(mod_idx) = file_module_node {
                ring0_set.insert(mod_idx);
            }
        }

        // Reverse BFS along incoming edges (caller -> callee) to discover Ring 1 and Ring 2.
        let mut depth_map: HashMap<NodeIndex, usize> = HashMap::new();
        let mut queue: VecDeque<NodeIndex> = VecDeque::new();

        let mut sorted_ring0: Vec<NodeIndex> = ring0_set.iter().copied().collect();
        sorted_ring0.sort_by(|&a, &b| sort_key(&self.graph[a]).cmp(&sort_key(&self.graph[b])));

        for idx in sorted_ring0 {
            depth_map.insert(idx, 0);
            queue.push_back(idx);
        }

        while let Some(curr) = queue.pop_front() {
            let curr_depth = depth_map[&curr];
            if curr_depth >= config.max_ring2_depth {
                continue;
            }
            let mut callers: Vec<NodeIndex> = self
                .graph
                .neighbors_directed(curr, Direction::Incoming)
                .collect();
            callers.sort_by(|&a, &b| sort_key(&self.graph[a]).cmp(&sort_key(&self.graph[b])));

            for caller in callers {
                if let std::collections::hash_map::Entry::Vacant(entry) = depth_map.entry(caller) {
                    entry.insert(curr_depth + 1);
                    queue.push_back(caller);
                }
            }
        }

        let mut ring0_indices = Vec::new();
        let mut ring1_indices = Vec::new();
        let mut ring2_indices = Vec::new();

        for (&idx, &depth) in &depth_map {
            match depth {
                0 => ring0_indices.push(idx),
                1 => ring1_indices.push(idx),
                _ => ring2_indices.push(idx),
            }
        }

        ring0_indices.sort_by(|&a, &b| sort_key(&self.graph[a]).cmp(&sort_key(&self.graph[b])));
        ring1_indices.sort_by(|&a, &b| sort_key(&self.graph[a]).cmp(&sort_key(&self.graph[b])));
        ring2_indices.sort_by(|&a, &b| sort_key(&self.graph[a]).cmp(&sort_key(&self.graph[b])));

        let r0 = if ring0_indices.len() <= 1 {
            0.0
        } else {
            (22.0 + (ring0_indices.len() as f32) * 7.0).min(config.ring0_max_radius)
        };

        let ring0_mutated =
            build_ring_nodes(&self.graph, &ring0_indices, ImpactRing::Ring0, r0, coverage);
        let ring1_direct_callers = build_ring_nodes(
            &self.graph,
            &ring1_indices,
            ImpactRing::Ring1,
            config.ring1_radius,
            coverage,
        );
        let ring2_transitive_entrypoints = build_ring_nodes(
            &self.graph,
            &ring2_indices,
            ImpactRing::Ring2,
            config.ring2_radius,
            coverage,
        );

        let included: HashSet<NodeIndex> = depth_map.keys().copied().collect();
        let mut edges = Vec::new();
        for edge_idx in self.graph.edge_indices() {
            if let Some((src, dst)) = self.graph.edge_endpoints(edge_idx) {
                if included.contains(&src) && included.contains(&dst) {
                    edges.push(GraphEdge {
                        from_id: self.graph[src].id.clone(),
                        to_id: self.graph[dst].id.clone(),
                        relation: self.graph[edge_idx],
                    });
                }
            }
        }
        edges.sort_by(|a, b| (&a.from_id, &a.to_id).cmp(&(&b.from_id, &b.to_id)));

        let composite_risk_score = compute_deterministic_blast_score(
            &ring0_mutated,
            &ring1_direct_callers,
            &ring2_transitive_entrypoints,
            touched_files.len(),
        );

        BlastRadiusReport {
            ring0_mutated,
            ring1_direct_callers,
            ring2_transitive_entrypoints,
            edges,
            semantic_hazards: Vec::new(),
            composite_risk_score,
            root_cause_depth_score: 50.0,
            bandaid_debt_warning: None,
            bandaid_probability: None,
            decision_confidence: None,
        }
    }

    pub fn get_symbol(&self, id: &str) -> Option<&SymbolDef> {
        self.id_to_idx.get(id).map(|&idx| &self.graph[idx])
    }
}

fn sort_key(sym: &SymbolDef) -> (&str, usize, &str) {
    (&sym.file_path, sym.start_line, &sym.qualified_name)
}

fn file_stem(path: &str) -> String {
    Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string()
}

fn resolve_callee_candidates(
    callee_raw: &str,
    caller_file: &str,
    import_set: &HashSet<&str>,
    graph: &DiGraph<SymbolDef, EdgeRelation>,
    by_short_name: &HashMap<String, Vec<NodeIndex>>,
    by_qualified_name: &HashMap<String, Vec<NodeIndex>>,
) -> Vec<NodeIndex> {
    if let Some(exact) = by_qualified_name.get(callee_raw) {
        return exact.clone();
    }

    let short = callee_raw.rsplit('.').next().unwrap_or(callee_raw);
    let Some(matches) = by_short_name.get(short) else {
        return Vec::new();
    };

    if matches.len() == 1 {
        return matches.clone();
    }

    // Prefer same-file definitions or definitions from explicitly imported modules/symbols.
    let preferred: Vec<NodeIndex> = matches
        .iter()
        .copied()
        .filter(|&idx| {
            let sym = &graph[idx];
            if sym.file_path == caller_file {
                return true;
            }
            let stem = file_stem(&sym.file_path);
            import_set.contains(sym.name.as_str())
                || import_set.contains(stem.as_str())
                || callee_raw
                    .split('.')
                    .next()
                    .is_some_and(|prefix| prefix == stem || import_set.contains(prefix))
        })
        .collect();

    if !preferred.is_empty() {
        preferred
    } else if matches.len() <= 3 {
        matches.clone()
    } else {
        // Avoid noisy fan-out on extremely generic names like `get`, `run`, `new`
        Vec::new()
    }
}

fn build_ring_nodes(
    graph: &DiGraph<SymbolDef, EdgeRelation>,
    indices: &[NodeIndex],
    ring: ImpactRing,
    radius: f32,
    coverage: Option<&CoverageMap>,
) -> Vec<GraphNode> {
    let count = indices.len();
    let mut nodes = Vec::with_capacity(count);

    for (i, &idx) in indices.iter().enumerate() {
        let sym = &graph[idx];
        let (radial_x, radial_y) = if radius <= 0.0 || count == 0 {
            (0.0, 0.0)
        } else {
            let angle = -FRAC_PI_2 + (TAU * (i as f32) / (count as f32));
            let x = (radius * angle.cos() * 100.0).round() / 100.0;
            let y = (radius * angle.sin() * 100.0).round() / 100.0;
            (x, y)
        };

        let test_coverage_pct =
            coverage.and_then(|c| c.symbol_coverage_pct(&sym.file_path, sym.start_line, sym.end_line));
        let is_untested_dark_zone = matches!(ring, ImpactRing::Ring1 | ImpactRing::Ring2)
            && test_coverage_pct.is_some_and(|pct| pct <= 0.0);

        nodes.push(GraphNode {
            id: sym.id.clone(),
            symbol: sym.qualified_name.clone(),
            file_path: sym.file_path.clone(),
            line: sym.start_line,
            ring,
            test_coverage_pct,
            is_untested_dark_zone,
            radial_x,
            radial_y,
        });
    }

    nodes
}

/// Computes a deterministic structural blast-radius risk score in `0.0..=100.0`.
pub fn compute_deterministic_blast_score(
    ring0: &[GraphNode],
    ring1: &[GraphNode],
    ring2: &[GraphNode],
    touched_file_count: usize,
) -> f32 {
    let mut score = 0.0_f32;
    score += (ring0.len() as f32 * 5.0).min(20.0);
    score += (ring1.len() as f32 * 6.0).min(30.0);
    score += (ring2.len() as f32 * 3.5).min(20.0);
    score += (touched_file_count.saturating_sub(1) as f32 * 3.0).min(12.0);

    let ring1_dark = ring1.iter().filter(|n| n.is_untested_dark_zone).count() as f32;
    let ring2_dark = ring2.iter().filter(|n| n.is_untested_dark_zone).count() as f32;
    score += ring1_dark * 12.0 + ring2_dark * 7.0;

    ((score.clamp(0.0, 100.0)) * 10.0).round() / 10.0
}
