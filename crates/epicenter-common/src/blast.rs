use serde::{Deserialize, Serialize};

/// Concentric impact ring distance from the mutated code (Ring 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImpactRing {
    Ring0,
    Ring1,
    Ring2,
}

impl ImpactRing {
    pub const fn depth(&self) -> u8 {
        match self {
            Self::Ring0 => 0,
            Self::Ring1 => 1,
            Self::Ring2 => 2,
        }
    }
}

/// Relationship represented by an edge in the blast-radius call/reference graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeRelation {
    Calls,
    Imports,
    References,
    Contains,
}

/// A symbol or module node in the concentric blast-radius graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphNode {
    pub id: String,
    pub symbol: String,
    pub file_path: String,
    pub line: usize,
    pub ring: ImpactRing,
    pub test_coverage_pct: Option<f32>,
    pub is_untested_dark_zone: bool,
    pub radial_x: f32,
    pub radial_y: f32,
}

/// Directed edge from caller/importer (`from_id`) to callee/target (`to_id`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GraphEdge {
    pub from_id: String,
    pub to_id: String,
    pub relation: EdgeRelation,
}

/// Severity level for semantic hazards emitted by Gemini 4 Carbon audits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HazardSeverity {
    Low,
    Medium,
    High,
    Critical,
}

impl HazardSeverity {
    pub const fn risk_weight(&self) -> f32 {
        match self {
            Self::Low => 4.0,
            Self::Medium => 10.0,
            Self::High => 20.0,
            Self::Critical => 35.0,
        }
    }
}

/// Semantic hazard identified by deep audit of a candidate diff against callers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SemanticHazard {
    pub severity: HazardSeverity,
    pub category: String,
    pub symbol: String,
    pub explanation: String,
    pub mitigation: String,
}

fn default_root_cause_score() -> f32 {
    50.0
}

/// Combined structural (AST/call-graph/coverage) + semantic blast-radius & root-cause report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlastRadiusReport {
    pub ring0_mutated: Vec<GraphNode>,
    pub ring1_direct_callers: Vec<GraphNode>,
    pub ring2_transitive_entrypoints: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    pub semantic_hazards: Vec<SemanticHazard>,
    /// Breakage/collateral risk score (`0.0..=100.0`, lower is safer).
    pub composite_risk_score: f32,
    /// Architectural root-cause cure depth (`0.0..=100.0`, higher = cures root invariant rather than stacking bandaid debt).
    #[serde(default = "default_root_cause_score")]
    pub root_cause_depth_score: f32,
    /// Optional warning emitted by Carbon when a candidate is a symptom-only bandaid that stacks tech debt.
    #[serde(default)]
    pub bandaid_debt_warning: Option<String>,
    /// Calibrated `noul` probability (`0.0..=1.0`) from `gemini-decision-1` that the patch masks
    /// symptoms rather than curing the underlying root-cause invariant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bandaid_probability: Option<f32>,
    /// Mean calibrated confidence (`0.0..=1.0`) reported by `gemini-decision-1` on this audit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_confidence: Option<f32>,
}

impl Default for BlastRadiusReport {
    fn default() -> Self {
        Self {
            ring0_mutated: Vec::new(),
            ring1_direct_callers: Vec::new(),
            ring2_transitive_entrypoints: Vec::new(),
            edges: Vec::new(),
            semantic_hazards: Vec::new(),
            composite_risk_score: 0.0,
            root_cause_depth_score: 50.0,
            bandaid_debt_warning: None,
            bandaid_probability: None,
            decision_confidence: None,
        }
    }
}

impl BlastRadiusReport {
    pub fn total_nodes(&self) -> usize {
        self.ring0_mutated.len()
            + self.ring1_direct_callers.len()
            + self.ring2_transitive_entrypoints.len()
    }

    pub fn dark_zone_count(&self) -> usize {
        self.ring1_direct_callers
            .iter()
            .chain(self.ring2_transitive_entrypoints.iter())
            .filter(|n| n.is_untested_dark_zone)
            .count()
    }

    pub fn all_nodes(&self) -> impl Iterator<Item = &GraphNode> {
        self.ring0_mutated
            .iter()
            .chain(self.ring1_direct_callers.iter())
            .chain(self.ring2_transitive_entrypoints.iter())
    }

    /// Recomputes `composite_risk_score` combining base structural risk and semantic hazards,
    /// clamped cleanly to `0.0..=100.0`.
    pub fn recalculate_composite_risk(&mut self, base_structural_score: f32) {
        let hazard_penalty: f32 = self
            .semantic_hazards
            .iter()
            .map(|h| h.severity.risk_weight())
            .sum();
        self.composite_risk_score = (base_structural_score + hazard_penalty).clamp(0.0, 100.0);
    }
}
