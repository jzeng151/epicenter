use crate::intake::format_multimodal_findings;
use crate::repo::{emit_event, emit_log, extract_json_from_text, truncate_str, RepoSnapshot};
use epicenter_common::{
    compute_pareto_frontier, crown_candidate_badges, BlastRadiusReport, CandidateStatus,
    Gemini4Model, HazardSeverity, LiveEvent, LogLevel, SemanticHazard, StrategyDepthTier,
    TournamentSession, TournamentStage,
};
use epicenter_gemini::{CreateInteractionRequest, GeminiClient, ResponseFormat};
use epicenter_graph::{BlastAnalysisConfig, CoverageMap, RepoCallGraph};
use serde::Deserialize;
use serde_json::json;
use std::collections::BTreeMap;
use tokio::sync::broadcast;
use tracing::warn;

#[derive(Debug, Deserialize)]
struct CarbonAuditOutput {
    #[serde(default)]
    semantic_hazards: Vec<RawSemanticHazard>,
    #[serde(default = "default_depth")]
    root_cause_depth_score: f32,
    #[serde(default)]
    bandaid_debt_warning: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawSemanticHazard {
    #[serde(default = "default_severity")]
    severity: HazardSeverity,
    #[serde(default)]
    category: String,
    #[serde(default)]
    symbol: String,
    #[serde(default)]
    explanation: String,
    #[serde(default)]
    mitigation: String,
}

fn default_depth() -> f32 {
    50.0
}

fn default_severity() -> HazardSeverity {
    HazardSeverity::Medium
}

/// Stage 6: 3-Layer Blast-Radius Impact & Concurrent `Gemini4Model::Carbon` Dual-Score Auditor.
///
/// Layer 1: Deterministic AST + Reverse Call-Graph (`RepoCallGraph::from_sources` -> Ring 0/1/2).
/// Layer 2: Per-candidate Test Coverage Dark-Zone Overlay (`CoverageMap`).
/// Layer 3: Concurrent `Gemini4Model::Carbon` Semantic Hazard + Root-Cause Depth (`0..=100`) &
/// Bandaid Debt audit via `futures::future::join_all`, checking multimodal visual/audio findings.
///
/// Recomputes `composite_risk_score`, runs `compute_pareto_frontier` and `crown_candidate_badges`,
/// and returns the recommended `BestOverall` strategy ID.
pub async fn run_auditor_stage(
    gemini: &GeminiClient,
    session: &mut TournamentSession,
    repo: &RepoSnapshot,
    coverage_maps: &BTreeMap<String, CoverageMap>,
    event_tx: &Option<broadcast::Sender<LiveEvent>>,
) -> anyhow::Result<Option<String>> {
    session.stage = TournamentStage::Auditing;
    session.updated_at = chrono::Utc::now();
    emit_event(
        event_tx,
        LiveEvent::StageChanged {
            session_id: session.id,
            stage: TournamentStage::Auditing,
            message: format!(
                "Stage 6 — 3-Layer Blast-Radius & concurrent Carbon Dual-Score audit ({} candidates, {} source files)",
                session.candidates.len(),
                repo.sources.len()
            ),
        },
    );

    let graph = RepoCallGraph::from_sources(&repo.sources);
    let blast_cfg = BlastAnalysisConfig::default();

    let strategy_tiers: BTreeMap<String, StrategyDepthTier> = session
        .strategy_plan
        .as_ref()
        .map(|p| {
            p.strategies
                .iter()
                .map(|s| (s.id.clone(), s.depth_tier))
                .collect()
        })
        .unwrap_or_default();

    let issue_title = session.intake.title.clone();
    let problem_statement = session.intake.problem_statement.clone();
    let multimodal_block = format_multimodal_findings(&session.intake.multimodal_findings);
    let session_id = session.id;

    // Layers 1 & 2 (deterministic): build structural + coverage blast-radius reports for every candidate.
    struct PendingAudit {
        cand_idx: usize,
        strategy_id: String,
        tier: StrategyDepthTier,
        added_caller_tests: bool,
        git_diff: String,
        report: BlastRadiusReport,
        base_structural_score: f32,
    }

    let mut pending: Vec<PendingAudit> = Vec::new();
    for (idx, cand) in session.candidates.iter().enumerate() {
        if cand.git_diff.trim().is_empty() {
            continue;
        }
        let cov = coverage_maps.get(&cand.strategy_id);
        let report = graph.analyze_diff(&cand.git_diff, cov, &blast_cfg);
        let base_structural_score = report.composite_risk_score;
        let tier = strategy_tiers
            .get(&cand.strategy_id)
            .copied()
            .unwrap_or_default();
        pending.push(PendingAudit {
            cand_idx: idx,
            strategy_id: cand.strategy_id.clone(),
            tier,
            added_caller_tests: cand.added_caller_tests,
            git_diff: cand.git_diff.clone(),
            report,
            base_structural_score,
        });
    }

    // Layer 3: Run Gemini 4 Carbon semantic hazard & root-cause depth audits CONCURRENTLY!
    let audit_futs = pending.iter().map(|item| {
        run_carbon_semantic_audit(
            gemini,
            &issue_title,
            &problem_statement,
            &multimodal_block,
            &item.strategy_id,
            item.tier,
            item.added_caller_tests,
            &item.git_diff,
            &item.report,
        )
    });
    let audit_results = futures::future::join_all(audit_futs).await;

    for (mut item, audit_out) in pending.into_iter().zip(audit_results.into_iter()) {
        match audit_out {
            Ok(out) => {
                item.report.semantic_hazards = out
                    .semantic_hazards
                    .into_iter()
                    .filter(|h| !h.explanation.trim().is_empty())
                    .map(|h| SemanticHazard {
                        severity: h.severity,
                        category: if h.category.trim().is_empty() {
                            "semantic_contract".to_string()
                        } else {
                            h.category
                        },
                        symbol: if h.symbol.trim().is_empty() {
                            item.report
                                .ring0_mutated
                                .first()
                                .map(|n| n.symbol.clone())
                                .unwrap_or_else(|| "unknown".to_string())
                        } else {
                            h.symbol
                        },
                        explanation: h.explanation,
                        mitigation: h.mitigation,
                    })
                    .collect();
                item.report.root_cause_depth_score = out.root_cause_depth_score.clamp(0.0, 100.0);
                item.report.bandaid_debt_warning = out
                    .bandaid_debt_warning
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty());
                item.report
                    .recalculate_composite_risk(item.base_structural_score);
            }
            Err(err) => {
                warn!(strategy = %item.strategy_id, error = %err, "Carbon semantic audit fallback");
                emit_log(
                    event_tx,
                    session_id,
                    LogLevel::Warn,
                    format!("auditor:{}", item.strategy_id),
                    format!("Carbon audit fallback ({err}); using structural Dual-Score heuristic"),
                );
                apply_structural_dual_score_fallback(
                    &mut item.report,
                    item.tier,
                    item.added_caller_tests,
                );
                item.report
                    .recalculate_composite_risk(item.base_structural_score);
            }
        }

        let hazards = item.report.semantic_hazards.clone();
        let composite_risk = item.report.composite_risk_score;

        let cand = &mut session.candidates[item.cand_idx];
        cand.blast_radius = Some(item.report.clone());
        if cand.is_green() {
            cand.status = CandidateStatus::Audited;
        }

        emit_event(
            event_tx,
            LiveEvent::BlastRadiusReady {
                session_id,
                strategy_id: cand.strategy_id.clone(),
                report: Box::new(item.report),
            },
        );
        emit_event(
            event_tx,
            LiveEvent::HazardAuditReady {
                session_id,
                strategy_id: cand.strategy_id.clone(),
                hazards,
                composite_risk_score: composite_risk,
            },
        );
    }

    // Compute Pareto frontier & crown the 3 Dual-Score Studio badges
    let pareto_ids = compute_pareto_frontier(&mut session.candidates);
    let recommended = crown_candidate_badges(&mut session.candidates);

    if let Some(ref mut summary) = session.arena_summary {
        summary.pareto_frontier_ids = pareto_ids.clone();
        summary.recommended_strategy_id = recommended.clone();
    }

    // Emit final CandidateUpdated with pareto_optimal & badges populated
    for cand in &session.candidates {
        emit_event(
            event_tx,
            LiveEvent::CandidateUpdated {
                session_id,
                candidate: Box::new(cand.clone()),
            },
        );
    }

    emit_log(
        event_tx,
        session_id,
        LogLevel::Info,
        "auditor",
        format!(
            "Dual-Score Audit complete | pareto={:?} | BestOverall={:?}",
            pareto_ids, recommended
        ),
    );

    session.updated_at = chrono::Utc::now();
    Ok(recommended)
}

#[allow(clippy::too_many_arguments)]
async fn run_carbon_semantic_audit(
    gemini: &GeminiClient,
    issue_title: &str,
    problem_statement: &str,
    multimodal_block: &str,
    strategy_id: &str,
    tier: StrategyDepthTier,
    added_caller_tests: bool,
    git_diff: &str,
    structural_report: &BlastRadiusReport,
) -> anyhow::Result<CarbonAuditOutput> {
    let ring0_summary = summarize_nodes(&structural_report.ring0_mutated);
    let ring1_summary = summarize_nodes(&structural_report.ring1_direct_callers);
    let ring2_summary = summarize_nodes(&structural_report.ring2_transitive_entrypoints);

    let prompt = format!(
        "You are Stage 6 (Layer-3 Semantic Hazard & Dual-Score Auditor) powered by Gemini 4 Carbon.\n\
         Evaluate candidate `{strategy_id}` (depth tier: `{tier:?}`, added_caller_tests: {added_caller_tests})\n\
         against the problem statement, multimodal visual/audio findings, and concentric blast-radius graph below.\n\n\
         DUAL-SCORE RUBRIC:\n\
         1. `semantic_hazards`: hidden behavioral regressions, concurrency/race conditions, cache staleness,\n\
            unchecked null/error contract changes, unresolved multimodal visual/audio symptoms, or security\n\
            flaws affecting Ring 1 / Ring 2 callers.\n\
         2. `root_cause_depth_score` (0.0..=100.0):\n\
            - 80..=100: Cures the underlying upstream invariant cleanly with durable architecture and resolves all reported visual/audio symptoms.\n\
            - 50..=79: Solid targeted fix/optimization that addresses core logic.\n\
            - 0..=49: Symptom-only guard/bandaid that masks the defect downstream and stacks tech debt.\n\
         3. `bandaid_debt_warning`: null if root_cause_depth_score >= 60, otherwise a crisp 1-sentence\n\
            warning explaining what upstream invariant debt remains.\n\n\
         Issue Title: {issue_title}\n\
         Problem Statement:\n{problem_statement}\n\n\
         MULTIMODAL FINDINGS (verify whether this candidate resolves every visual/audio symptom):\n{multimodal_block}\n\n\
         STRUCTURAL BLAST RADIUS:\n\
         - Ring 0 (Mutated): {ring0_summary}\n\
         - Ring 1 (Direct Callers): {ring1_summary}\n\
         - Ring 2 (Transitive Entrypoints): {ring2_summary}\n\
         - Untested Dark Zones: {dark_zones}\n\
         - Base Structural Risk Score: {base_risk:.1}\n\n\
         CANDIDATE DIFF:\n```diff\n{diff}\n```\n\n\
         Return STRICT JSON matching the schema.",
        dark_zones = structural_report.dark_zone_count(),
        base_risk = structural_report.composite_risk_score,
        diff = truncate_str(git_diff, 18_000),
    );

    let mut req = CreateInteractionRequest::for_model(Gemini4Model::Carbon, prompt);
    req.system_instruction = Some(
        "You are Gemini 4 Carbon, a Principal SRE & Security Auditor. Be razor-sharp, objective, verify multimodal findings, and return strict JSON."
            .to_string(),
    );
    req.response_format = Some(ResponseFormat::json_schema(carbon_audit_schema()));

    let resp = gemini.create_interaction(&req).await?;
    extract_json_from_text::<CarbonAuditOutput>(&resp.extract_text())
}

fn summarize_nodes(nodes: &[epicenter_common::GraphNode]) -> String {
    if nodes.is_empty() {
        return "(none)".to_string();
    }
    nodes
        .iter()
        .take(15)
        .map(|n| {
            let cov = match n.test_coverage_pct {
                Some(pct) => format!("{pct:.0}%"),
                None => "unknown".to_string(),
            };
            let dz = if n.is_untested_dark_zone {
                " [DARK ZONE]"
            } else {
                ""
            };
            format!("{} ({}:{}, cov={}{})", n.symbol, n.file_path, n.line, cov, dz)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn apply_structural_dual_score_fallback(
    report: &mut BlastRadiusReport,
    tier: StrategyDepthTier,
    added_caller_tests: bool,
) {
    match tier {
        StrategyDepthTier::SurgicalHotfix => {
            report.root_cause_depth_score = 32.0;
            report.bandaid_debt_warning = Some(
                "Localized boundary guard mitigates immediate symptom without curing upstream invariant."
                    .to_string(),
            );
        }
        StrategyDepthTier::TargetedRefactor => {
            report.root_cause_depth_score = 68.0;
        }
        StrategyDepthTier::AlgorithmicOptimization => {
            report.root_cause_depth_score = 74.0;
        }
        StrategyDepthTier::ArchitecturalRootCause => {
            report.root_cause_depth_score = if added_caller_tests { 92.0 } else { 86.0 };
        }
    }
}

fn carbon_audit_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "semantic_hazards": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "severity": {
                            "type": "string",
                            "enum": ["low", "medium", "high", "critical"]
                        },
                        "category": { "type": "string" },
                        "symbol": { "type": "string" },
                        "explanation": { "type": "string" },
                        "mitigation": { "type": "string" }
                    },
                    "required": ["severity", "category", "symbol", "explanation", "mitigation"]
                }
            },
            "root_cause_depth_score": { "type": "number" },
            "bandaid_debt_warning": { "type": ["string", "null"] }
        },
        "required": ["semantic_hazards", "root_cause_depth_score"]
    })
}
