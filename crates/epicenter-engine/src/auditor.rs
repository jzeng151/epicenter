use crate::intake::format_multimodal_findings;
use crate::repo::{emit_event, emit_log, extract_json_from_text, truncate_str, RepoSnapshot};
use epicenter_common::{
    compute_pareto_frontier, crown_candidate_badges, BlastRadiusReport, CandidateStatus,
    Gemini4Model, GraphNode, HazardSeverity, LiveEvent, LogLevel, SemanticHazard,
    StrategyDepthTier, TournamentSession, TournamentStage,
};
use epicenter_gemini::{
    ContentPart, CreateDecisionRequest, CreateInteractionRequest, DecisionQuestion,
    DecisionResponse, GeminiClient, GenerationConfig, ResponseFormat,
};
use epicenter_graph::{BlastAnalysisConfig, CoverageMap, RepoCallGraph};
use serde::Deserialize;
use serde_json::json;
use std::collections::BTreeMap;
use tokio::sync::broadcast;
use tracing::warn;

const MAX_DECISION_CALLER_QUESTIONS: usize = 12;
const ROOT_CAUSE_RUBRIC_LEVELS: usize = 5;
const BANDAID_WARNING_THRESHOLD: f64 = 0.65;
const BANDAID_PENALTY_SLOPE: f32 = 45.0;
const DARK_ZONE_CALLER_MEDIUM_THRESHOLD: f64 = 0.55;
const DARK_ZONE_CALLER_HIGH_THRESHOLD: f64 = 0.78;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CallerDecisionTarget {
    pub key: String,
    pub symbol: String,
    pub file_path: String,
    pub line: usize,
    pub is_untested_dark_zone: bool,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct DecisionCalibrationSummary {
    pub decision_root_cause_100: Option<f32>,
    pub bandaid_prob: Option<f64>,
    pub regression_prob: Option<f64>,
    pub mean_confidence: Option<f64>,
    pub added_caller_hazards: usize,
}

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
                "Stage 6 — 3-Layer Blast-Radius & concurrent Carbon + gemini-decision-1 calibrated Dual-Score audit ({} candidates, {} source files)",
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

    // Layer 3: Run Gemini 4 Carbon semantic hazard & root-cause depth audits (35s cap)
    // CONCURRENTLY alongside calibrated `gemini-decision-1` rubric & dark-zone caller audits (12s cap)
    // via `tokio::join!` per candidate and `futures::future::join_all` across candidates.
    let title_ref = issue_title.as_str();
    let problem_ref = problem_statement.as_str();
    let multimodal_ref = multimodal_block.as_str();
    let audit_futs = pending.iter().map(|item| async move {
        let carbon_fut = async {
            match tokio::time::timeout(
                std::time::Duration::from_secs(35),
                run_carbon_semantic_audit(
                    gemini,
                    title_ref,
                    problem_ref,
                    multimodal_ref,
                    &item.strategy_id,
                    item.tier,
                    item.added_caller_tests,
                    &item.git_diff,
                    &item.report,
                ),
            )
            .await
            {
                Ok(res) => res,
                Err(_) => Err(anyhow::anyhow!(
                    "Carbon semantic audit exceeded 35s timeout"
                )),
            }
        };

        let decision_fut = async {
            match tokio::time::timeout(
                std::time::Duration::from_secs(12),
                run_decision_rubric_audit(
                    gemini,
                    title_ref,
                    problem_ref,
                    multimodal_ref,
                    &item.strategy_id,
                    item.tier,
                    item.added_caller_tests,
                    &item.git_diff,
                    &item.report,
                ),
            )
            .await
            {
                Ok(res) => res,
                Err(_) => Err(anyhow::anyhow!(
                    "gemini-decision-1 rubric audit exceeded 12s timeout"
                )),
            }
        };

        tokio::join!(carbon_fut, decision_fut)
    });
    let audit_results = futures::future::join_all(audit_futs).await;

    for (mut item, (carbon_out, decision_out)) in pending.into_iter().zip(audit_results.into_iter()) {
        let carbon_succeeded = match carbon_out {
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
                true
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
                false
            }
        };

        match decision_out {
            Ok((decision_resp, caller_targets)) => {
                let cal = apply_decision_calibration(
                    &mut item.report,
                    &decision_resp,
                    &caller_targets,
                    carbon_succeeded,
                    item.base_structural_score,
                );
                emit_log(
                    event_tx,
                    session_id,
                    LogLevel::Info,
                    format!("auditor:{}", item.strategy_id),
                    format!(
                        "gemini-decision-1 calibrated audit | root_cause={:.1}/100 (rubric={:.1}/100) | bandaid_prob={:.1}% | confidence={:.1}% | regression_prob={:.1}% | caller_hazards_added={}",
                        item.report.root_cause_depth_score,
                        cal.decision_root_cause_100
                            .unwrap_or(item.report.root_cause_depth_score),
                        cal.bandaid_prob.unwrap_or(0.0) * 100.0,
                        cal.mean_confidence.unwrap_or(0.0) * 100.0,
                        cal.regression_prob.unwrap_or(0.0) * 100.0,
                        cal.added_caller_hazards,
                    ),
                );
            }
            Err(err) => {
                warn!(
                    strategy = %item.strategy_id,
                    error = %err,
                    "gemini-decision-1 rubric audit unavailable; preserving Carbon/structural audit"
                );
                emit_log(
                    event_tx,
                    session_id,
                    LogLevel::Info,
                    format!("auditor:{}", item.strategy_id),
                    format!(
                        "gemini-decision-1 rubric unavailable ({err}); root_cause={:.1}/100 | bandaid_prob={} | confidence={}",
                        item.report.root_cause_depth_score,
                        item.report
                            .bandaid_probability
                            .map(|p| format!("{:.1}%", p * 100.0))
                            .unwrap_or_else(|| "n/a".to_string()),
                        item.report
                            .decision_confidence
                            .map(|c| format!("{:.1}%", c * 100.0))
                            .unwrap_or_else(|| "n/a".to_string()),
                    ),
                );
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
         Problem Statement:\n{problem}\n\n\
         MULTIMODAL FINDINGS (verify whether this candidate resolves every visual/audio symptom):\n{multimodal_block}\n\n\
         STRUCTURAL BLAST RADIUS:\n\
         - Ring 0 (Mutated): {ring0_summary}\n\
         - Ring 1 (Direct Callers): {ring1_summary}\n\
         - Ring 2 (Transitive Entrypoints): {ring2_summary}\n\
         - Untested Dark Zones: {dark_zones}\n\
         - Base Structural Risk Score: {base_risk:.1}\n\n\
         CANDIDATE DIFF:\n```diff\n{diff}\n```\n\n\
         Keep each hazard description concise (1 sentence, max 3 hazards). Return STRICT JSON matching the schema.",
        problem = truncate_str(problem_statement, 2_000),
        dark_zones = structural_report.dark_zone_count(),
        base_risk = structural_report.composite_risk_score,
        diff = truncate_str(git_diff, 8_000),
    );

    let mut req = CreateInteractionRequest::for_model(Gemini4Model::Carbon, prompt);
    req.system_instruction = Some(
        "You are Gemini 4 Carbon, a Principal SRE & Security Auditor. Be razor-sharp, objective, verify multimodal findings, and return strict JSON."
            .to_string(),
    );
    req.response_format = Some(ResponseFormat::json_schema(carbon_audit_schema()));
    req.generation_config = Some(auditor_generation_config());

    let resp = gemini.create_interaction(&req).await?;
    extract_json_from_text::<CarbonAuditOutput>(&resp.extract_text())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_decision_rubric_request(
    issue_title: &str,
    problem_statement: &str,
    multimodal_block: &str,
    strategy_id: &str,
    tier: StrategyDepthTier,
    added_caller_tests: bool,
    git_diff: &str,
    structural_report: &BlastRadiusReport,
) -> (CreateDecisionRequest, Vec<CallerDecisionTarget>) {
    let ring0_summary = summarize_nodes(&structural_report.ring0_mutated);
    let ring1_summary = summarize_nodes(&structural_report.ring1_direct_callers);
    let dark_zone_nodes: Vec<GraphNode> = structural_report
        .ring1_direct_callers
        .iter()
        .chain(structural_report.ring2_transitive_entrypoints.iter())
        .filter(|n| n.is_untested_dark_zone)
        .cloned()
        .collect();
    let dark_zone_summary = summarize_nodes(&dark_zone_nodes);

    let context = format!(
        "Stage 6 Calibrated Rubric & Dark-Zone Caller Hazard Audit\n\
         Candidate Strategy: `{strategy_id}` (depth tier: `{tier:?}`, added_caller_tests: {added_caller_tests})\n\n\
         Issue Title: {issue_title}\n\
         Problem Statement:\n{problem}\n\n\
         Multimodal Findings:\n{multimodal_block}\n\n\
         Blast-Radius Caller Summary:\n\
         - Ring 0 (Mutated): {ring0_summary}\n\
         - Ring 1 (Direct Callers): {ring1_summary}\n\
         - Untested Dark-Zone Callers ({dark_count}): {dark_zone_summary}\n\n\
         Candidate Git Diff:\n```diff\n{diff}\n```",
        problem = truncate_str(problem_statement, 2_000),
        dark_count = structural_report.dark_zone_count(),
        diff = truncate_str(git_diff, 8_000),
    );

    let mut req = CreateDecisionRequest::new(vec![ContentPart::text(context)])
        .with_question(
            "root_cause_quality",
            DecisionQuestion::score(
                "Rate how deeply this candidate diff cures the underlying root cause versus merely patching symptoms.",
                [
                    "Broken, unrelated, or pure no-op workaround",
                    "Symptom-masking band-aid (swallows error, hardcodes case, or skips check without fixing cause)",
                    "Local guard that fixes immediate crash but leaves underlying invariant fragile",
                    "Clean targeted fix addressing the root bug in the affected function",
                    "Comprehensive root-cause cure preserving invariants and caller contracts",
                ],
            ),
        )
        .with_question(
            "is_symptom_bandaid",
            DecisionQuestion::noul(
                "Does this diff merely mask a symptom or hardcode a narrow workaround instead of curing the underlying root cause?",
            ),
        )
        .with_question(
            "regression_hazard",
            DecisionQuestion::noul(
                "Could this diff silently break existing callers, edge cases, or concurrency/performance invariants?",
            ),
        );

    // Ask up to 12 Ring 1 callers, prioritizing untested dark zones first.
    let prioritized_callers = structural_report
        .ring1_direct_callers
        .iter()
        .filter(|n| n.is_untested_dark_zone)
        .chain(
            structural_report
                .ring1_direct_callers
                .iter()
                .filter(|n| !n.is_untested_dark_zone),
        )
        .take(MAX_DECISION_CALLER_QUESTIONS);

    let mut caller_targets = Vec::new();
    for (idx, node) in prioritized_callers.enumerate() {
        let key = format!("caller_{idx:02}");
        req.insert_question(
            key.clone(),
            DecisionQuestion::noul(format!(
                "Could this diff break or alter expected behavior in caller `{}` ({})?",
                node.symbol, node.file_path
            )),
        );
        caller_targets.push(CallerDecisionTarget {
            key,
            symbol: node.symbol.clone(),
            file_path: node.file_path.clone(),
            line: node.line,
            is_untested_dark_zone: node.is_untested_dark_zone,
        });
    }

    (req, caller_targets)
}

#[allow(clippy::too_many_arguments)]
async fn run_decision_rubric_audit(
    gemini: &GeminiClient,
    issue_title: &str,
    problem_statement: &str,
    multimodal_block: &str,
    strategy_id: &str,
    tier: StrategyDepthTier,
    added_caller_tests: bool,
    git_diff: &str,
    structural_report: &BlastRadiusReport,
) -> anyhow::Result<(DecisionResponse, Vec<CallerDecisionTarget>)> {
    let (req, caller_targets) = build_decision_rubric_request(
        issue_title,
        problem_statement,
        multimodal_block,
        strategy_id,
        tier,
        added_caller_tests,
        git_diff,
        structural_report,
    );
    let resp = gemini.evaluate_decisions(&req).await?;
    Ok((resp, caller_targets))
}

pub(crate) fn apply_decision_calibration(
    report: &mut BlastRadiusReport,
    resp: &DecisionResponse,
    caller_targets: &[CallerDecisionTarget],
    carbon_succeeded: bool,
    base_structural_score: f32,
) -> DecisionCalibrationSummary {
    let decision_root_cause_100 =
        resp.score_normalized_100("root_cause_quality", ROOT_CAUSE_RUBRIC_LEVELS);
    let bandaid_prob = resp
        .noul_prob("is_symptom_bandaid")
        .map(|p| p.clamp(0.0, 1.0));
    let regression_prob = resp
        .noul_prob("regression_hazard")
        .map(|p| p.clamp(0.0, 1.0));

    let mean_confidence = if resp.answers.is_empty() {
        None
    } else {
        let sum: f64 = resp
            .answers
            .values()
            .map(|a| a.confidence().clamp(0.0, 1.0))
            .sum();
        Some((sum / resp.answers.len() as f64).clamp(0.0, 1.0))
    };

    if let Some(prob) = bandaid_prob {
        report.bandaid_probability = Some(prob as f32);
    }
    if let Some(conf) = mean_confidence {
        report.decision_confidence = Some(conf as f32);
    }

    if let Some(decision_score) = decision_root_cause_100 {
        let blended = if carbon_succeeded {
            0.65 * decision_score + 0.35 * report.root_cause_depth_score
        } else {
            decision_score
        };
        report.root_cause_depth_score = blended.clamp(0.0, 100.0);
    }

    if let Some(prob) = bandaid_prob {
        if !carbon_succeeded {
            // Defer to calibrated gemini-decision-1 when Carbon fell back to tier heuristics.
            report.bandaid_debt_warning = None;
        }
        if prob > BANDAID_WARNING_THRESHOLD {
            let penalty =
                (prob as f32 - BANDAID_WARNING_THRESHOLD as f32) * BANDAID_PENALTY_SLOPE;
            report.root_cause_depth_score =
                (report.root_cause_depth_score - penalty).clamp(0.0, 100.0);
            if report.bandaid_debt_warning.is_none() {
                report.bandaid_debt_warning = Some(format!(
                    "Calibrated gemini-decision-1 audit detected a {:.0}% probability that this patch masks symptoms rather than curing the underlying root cause.",
                    prob * 100.0
                ));
            }
        }
    }

    let mut added_caller_hazards = 0usize;
    for target in caller_targets {
        if !target.is_untested_dark_zone {
            continue;
        }
        let Some(prob) = resp.noul_prob(&target.key) else {
            continue;
        };
        let prob = prob.clamp(0.0, 1.0);
        if prob < DARK_ZONE_CALLER_MEDIUM_THRESHOLD {
            continue;
        }

        let already_listed = report
            .semantic_hazards
            .iter()
            .any(|h| h.symbol.trim().eq_ignore_ascii_case(target.symbol.trim()));
        if already_listed {
            continue;
        }

        let severity = if prob >= DARK_ZONE_CALLER_HIGH_THRESHOLD {
            HazardSeverity::High
        } else {
            HazardSeverity::Medium
        };
        report.semantic_hazards.push(SemanticHazard {
            severity,
            category: "calibrated_caller_risk".to_string(),
            symbol: target.symbol.clone(),
            explanation: format!(
                "Untested dark-zone caller `{}` ({}) carries a {:.0}% calibrated breakage probability (gemini-decision-1).",
                target.symbol,
                target.file_path,
                prob * 100.0
            ),
            mitigation: format!(
                "Add a targeted caller regression test covering `{}` ({}:{}) against the mutated behavior.",
                target.symbol, target.file_path, target.line
            ),
        });
        added_caller_hazards += 1;
    }

    report.recalculate_composite_risk(base_structural_score);

    DecisionCalibrationSummary {
        decision_root_cause_100,
        bandaid_prob,
        regression_prob,
        mean_confidence,
        added_caller_hazards,
    }
}

pub(crate) fn auditor_generation_config() -> GenerationConfig {
    GenerationConfig {
        thinking_level: Some("low".to_string()),
        ..Default::default()
    }
}

pub(crate) fn summarize_nodes(nodes: &[GraphNode]) -> String {
    const MAX_SUMMARY_NODES: usize = 12;
    if nodes.is_empty() {
        return "(none)".to_string();
    }
    let mut summary = nodes
        .iter()
        .take(MAX_SUMMARY_NODES)
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
        .join(", ");
    if nodes.len() > MAX_SUMMARY_NODES {
        let extra = nodes.len() - MAX_SUMMARY_NODES;
        summary.push_str(&format!(", ... (+{extra} more)"));
    }
    summary
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
            report.bandaid_probability = Some(0.82);
            report.decision_confidence = Some(0.76);
        }
        StrategyDepthTier::TargetedRefactor => {
            report.root_cause_depth_score = 68.0;
            report.bandaid_probability = Some(0.28);
            report.decision_confidence = Some(0.74);
        }
        StrategyDepthTier::AlgorithmicOptimization => {
            report.root_cause_depth_score = 74.0;
            report.bandaid_probability = Some(0.22);
            report.decision_confidence = Some(0.78);
        }
        StrategyDepthTier::ArchitecturalRootCause => {
            report.root_cause_depth_score = if added_caller_tests { 92.0 } else { 86.0 };
            report.bandaid_probability = Some(if added_caller_tests { 0.08 } else { 0.14 });
            report.decision_confidence = Some(if added_caller_tests { 0.89 } else { 0.83 });
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

#[cfg(test)]
mod tests {
    use super::*;
    use epicenter_common::{ImpactRing, GEMINI_DECISION_MODEL};
    use epicenter_gemini::DecisionAnswer;

    fn make_node(symbol: &str, file: &str, line: usize, ring: ImpactRing, dark_zone: bool) -> GraphNode {
        GraphNode {
            id: format!("{file}:{symbol}"),
            symbol: symbol.to_string(),
            file_path: file.to_string(),
            line,
            ring,
            test_coverage_pct: Some(if dark_zone { 0.0 } else { 95.0 }),
            is_untested_dark_zone: dark_zone,
            radial_x: 0.0,
            radial_y: 0.0,
        }
    }

    fn make_sample_report() -> BlastRadiusReport {
        let mut ring1 = Vec::new();
        // 8 covered callers first, then 6 untested dark-zone callers (14 total > 12 cap)
        for i in 0..8 {
            ring1.push(make_node(
                &format!("covered_caller_{i}"),
                "app/billing.py",
                10 + i,
                ImpactRing::Ring1,
                false,
            ));
        }
        for i in 0..6 {
            ring1.push(make_node(
                &format!("dark_caller_{i}"),
                "app/refunds.py",
                100 + i,
                ImpactRing::Ring1,
                true,
            ));
        }
        BlastRadiusReport {
            ring0_mutated: vec![make_node(
                "compute_total",
                "app/pricing.py",
                42,
                ImpactRing::Ring0,
                false,
            )],
            ring1_direct_callers: ring1,
            ring2_transitive_entrypoints: vec![make_node(
                "post_checkout",
                "app/routes.py",
                12,
                ImpactRing::Ring2,
                true,
            )],
            edges: Vec::new(),
            semantic_hazards: Vec::new(),
            composite_risk_score: 18.0,
            root_cause_depth_score: 60.0,
            bandaid_debt_warning: None,
            bandaid_probability: None,
            decision_confidence: None,
        }
    }

    #[test]
    fn decision_rubric_request_prioritizes_dark_zone_callers_and_caps_at_12() {
        let report = make_sample_report();
        let (req, targets) = build_decision_rubric_request(
            "Tenant pricing cache leak",
            "Concurrent tenants share stale tax totals.",
            "(none)",
            "s2-algorithmic-cache",
            StrategyDepthTier::AlgorithmicOptimization,
            false,
            "diff --git a/app/pricing.py b/app/pricing.py\n+cache_key = (tenant_id, sku)",
            &report,
        );

        assert_eq!(req.model, GEMINI_DECISION_MODEL);
        assert_eq!(targets.len(), 12);

        // First 6 caller slots must be all 6 untested dark-zone callers
        for (idx, target) in targets.iter().take(6).enumerate() {
            assert_eq!(target.key, format!("caller_{idx:02}"));
            assert!(target.is_untested_dark_zone);
            assert!(target.symbol.starts_with("dark_caller_"));
        }
        // Remaining 6 slots filled by covered Ring 1 callers
        for (offset, target) in targets.iter().skip(6).enumerate() {
            assert_eq!(target.key, format!("caller_{:02}", offset + 6));
            assert!(!target.is_untested_dark_zone);
            assert!(target.symbol.starts_with("covered_caller_"));
        }

        // Core rubric questions + 12 caller questions = 15 total
        assert_eq!(req.questions.len(), 15);
        match req.questions.get("root_cause_quality") {
            Some(DecisionQuestion::Score { criteria, .. }) => {
                assert_eq!(criteria.len(), 5);
                assert_eq!(criteria[0], "Broken, unrelated, or pure no-op workaround");
                assert_eq!(
                    criteria[4],
                    "Comprehensive root-cause cure preserving invariants and caller contracts"
                );
            }
            other => panic!("expected 5-level score question, got {other:?}"),
        }
        assert!(matches!(
            req.questions.get("is_symptom_bandaid"),
            Some(DecisionQuestion::Noul { .. })
        ));
        assert!(matches!(
            req.questions.get("regression_hazard"),
            Some(DecisionQuestion::Noul { .. })
        ));
        match req.questions.get("caller_00") {
            Some(DecisionQuestion::Noul { instructions }) => {
                assert!(instructions.contains("dark_caller_0"));
                assert!(instructions.contains("app/refunds.py"));
            }
            other => panic!("expected noul caller question, got {other:?}"),
        }
        assert!(req.questions.contains_key("caller_11"));
        assert!(!req.questions.contains_key("caller_12"));
    }

    #[test]
    fn blends_decision_and_carbon_scores_and_synthesizes_bandaid_warning_when_prob_high() {
        let mut report = make_sample_report();
        report.root_cause_depth_score = 70.0; // Carbon score
        report.bandaid_debt_warning = None;

        let mut answers = BTreeMap::new();
        // Raw score 1.6 / 4.0 = 40.0 / 100
        answers.insert(
            "root_cause_quality".to_string(),
            DecisionAnswer::Score {
                score: 1.6,
                legend: BTreeMap::new(),
                probabilities: BTreeMap::new(),
                confidence: 0.80,
            },
        );
        // Band-aid probability 0.85 (> 0.65 threshold -> penalty = (0.85 - 0.65) * 45 = 9.0)
        answers.insert(
            "is_symptom_bandaid".to_string(),
            DecisionAnswer::Noul {
                noul: 0.85,
                confidence: 0.90,
            },
        );
        answers.insert(
            "regression_hazard".to_string(),
            DecisionAnswer::Noul {
                noul: 0.30,
                confidence: 0.70,
            },
        );
        let resp = DecisionResponse {
            answers,
            usage: None,
        };

        let summary = apply_decision_calibration(&mut report, &resp, &[], true, 18.0);

        // Blended = 0.65 * 40.0 + 0.35 * 70.0 = 26.0 + 24.5 = 50.5; minus 9.0 penalty = 41.5
        assert!((report.root_cause_depth_score - 41.5).abs() < 0.05);
        assert!((report.bandaid_probability.unwrap() - 0.85).abs() < 1e-4);
        assert!((report.decision_confidence.unwrap() - 0.80).abs() < 1e-4);
        assert_eq!(summary.added_caller_hazards, 0);

        let warning = report
            .bandaid_debt_warning
            .as_deref()
            .expect("synthesized bandaid warning");
        assert!(warning.contains("85%"));
        assert!(warning.contains("gemini-decision-1"));
    }

    #[test]
    fn uses_100_percent_decision_score_when_carbon_timed_out() {
        let mut report = make_sample_report();
        apply_structural_dual_score_fallback(&mut report, StrategyDepthTier::SurgicalHotfix, false);
        assert!(report.bandaid_debt_warning.is_some());
        assert!((report.bandaid_probability.unwrap() - 0.82).abs() < 1e-4);

        let mut answers = BTreeMap::new();
        // Raw score 3.6 / 4.0 = 90.0 / 100
        answers.insert(
            "root_cause_quality".to_string(),
            DecisionAnswer::Score {
                score: 3.6,
                legend: BTreeMap::new(),
                probabilities: BTreeMap::new(),
                confidence: 0.92,
            },
        );
        answers.insert(
            "is_symptom_bandaid".to_string(),
            DecisionAnswer::Noul {
                noul: 0.12,
                confidence: 0.88,
            },
        );
        let resp = DecisionResponse {
            answers,
            usage: None,
        };

        apply_decision_calibration(&mut report, &resp, &[], false, 15.0);

        assert!((report.root_cause_depth_score - 90.0).abs() < 0.05);
        assert!((report.bandaid_probability.unwrap() - 0.12).abs() < 1e-4);
        assert!((report.decision_confidence.unwrap() - 0.90).abs() < 1e-4);
        assert!(report.bandaid_debt_warning.is_none());
    }

    #[test]
    fn synthesizes_calibrated_dark_zone_caller_hazards_without_duplicating_carbon_symbols() {
        let mut report = make_sample_report();
        report.semantic_hazards.push(SemanticHazard {
            severity: HazardSeverity::Medium,
            category: "existing_carbon".to_string(),
            symbol: "dark_caller_0".to_string(),
            explanation: "Carbon already flagged dark_caller_0.".to_string(),
            mitigation: "Test it.".to_string(),
        });

        let (_, targets) = build_decision_rubric_request(
            "Issue",
            "Problem",
            "(none)",
            "s2",
            StrategyDepthTier::TargetedRefactor,
            false,
            "+patch",
            &report,
        );

        let mut answers = BTreeMap::new();
        answers.insert(
            "root_cause_quality".to_string(),
            DecisionAnswer::Score {
                score: 3.0,
                legend: BTreeMap::new(),
                probabilities: BTreeMap::new(),
                confidence: 0.85,
            },
        );
        answers.insert(
            "is_symptom_bandaid".to_string(),
            DecisionAnswer::Noul {
                noul: 0.20,
                confidence: 0.85,
            },
        );
        // caller_00 = dark_caller_0 (already listed by Carbon -> must NOT duplicate)
        answers.insert(
            "caller_00".to_string(),
            DecisionAnswer::Noul {
                noul: 0.88,
                confidence: 0.90,
            },
        );
        // caller_01 = dark_caller_1 (prob 0.82 >= 0.78 -> High severity)
        answers.insert(
            "caller_01".to_string(),
            DecisionAnswer::Noul {
                noul: 0.82,
                confidence: 0.86,
            },
        );
        // caller_02 = dark_caller_2 (prob 0.62 in 0.55..0.78 -> Medium severity)
        answers.insert(
            "caller_02".to_string(),
            DecisionAnswer::Noul {
                noul: 0.62,
                confidence: 0.79,
            },
        );
        // caller_03 = dark_caller_3 (prob 0.40 < 0.55 -> no hazard)
        answers.insert(
            "caller_03".to_string(),
            DecisionAnswer::Noul {
                noul: 0.40,
                confidence: 0.75,
            },
        );
        // caller_06 = covered_caller_0 (NOT dark zone, even with high prob -> skipped)
        answers.insert(
            "caller_06".to_string(),
            DecisionAnswer::Noul {
                noul: 0.91,
                confidence: 0.90,
            },
        );

        let resp = DecisionResponse {
            answers,
            usage: None,
        };

        let summary = apply_decision_calibration(&mut report, &resp, &targets, true, 18.0);
        assert_eq!(summary.added_caller_hazards, 2);
        // 1 existing Carbon (Medium=10) + dark_caller_1 (High=20) + dark_caller_2 (Medium=10)
        assert_eq!(report.semantic_hazards.len(), 3);
        assert!((report.composite_risk_score - (18.0 + 10.0 + 20.0 + 10.0)).abs() < 1e-4);

        let h_high = report
            .semantic_hazards
            .iter()
            .find(|h| h.symbol == "dark_caller_1")
            .expect("high hazard for dark_caller_1");
        assert_eq!(h_high.severity, HazardSeverity::High);
        assert_eq!(h_high.category, "calibrated_caller_risk");
        assert!(h_high.explanation.contains("82%"));

        let h_med = report
            .semantic_hazards
            .iter()
            .find(|h| h.symbol == "dark_caller_2")
            .expect("medium hazard for dark_caller_2");
        assert_eq!(h_med.severity, HazardSeverity::Medium);
        assert_eq!(h_med.category, "calibrated_caller_risk");
        assert!(h_med.explanation.contains("62%"));

        // dark_caller_0 was not duplicated; dark_caller_3 and covered_caller_0 were not added
        assert_eq!(
            report
                .semantic_hazards
                .iter()
                .filter(|h| h.symbol == "dark_caller_0")
                .count(),
            1
        );
        assert!(!report
            .semantic_hazards
            .iter()
            .any(|h| h.symbol == "dark_caller_3" || h.symbol == "covered_caller_0"));
    }

    #[test]
    fn structural_fallback_populates_calibrated_probability_and_confidence() {
        let mut hotfix = make_sample_report();
        apply_structural_dual_score_fallback(&mut hotfix, StrategyDepthTier::SurgicalHotfix, false);
        assert!(hotfix.bandaid_probability.unwrap() > 0.75);
        assert!(hotfix.decision_confidence.unwrap() > 0.70);

        let mut root = make_sample_report();
        apply_structural_dual_score_fallback(
            &mut root,
            StrategyDepthTier::ArchitecturalRootCause,
            true,
        );
        assert!(root.bandaid_probability.unwrap() < 0.15);
        assert!(root.decision_confidence.unwrap() > 0.85);
    }

    #[test]
    fn export_bundle_renders_bandaid_probability_and_decision_confidence_when_present() {
        let mut session = crate::TournamentEngine::initialize_session(
            &epicenter_common::CreateSessionRequest {
                repo_url: "https://github.com/acme/repo".to_string(),
                commit_sha: Some("abc1234".to_string()),
                git_ref: Some("main".to_string()),
                task_kind: Some(epicenter_common::TaskKind::Bug),
                title: Some("Cache leak".to_string()),
                problem_statement: "Fix cache leak".to_string(),
                attachment_ids: Vec::new(),
                attachments: Vec::new(),
            },
        );
        let mut br = make_sample_report();
        br.bandaid_probability = Some(0.84);
        br.decision_confidence = Some(0.91);

        let mut cand = epicenter_common::CandidateResult::pending("s1");
        cand.status = CandidateStatus::Audited;
        cand.repro_passed = true;
        cand.regression_suite_passed = true;
        cand.git_diff = "diff --git a/a.py b/a.py\n+x = 1".to_string();
        cand.blast_radius = Some(br);
        session.candidates.push(cand);

        let bundle = crate::baselines::build_rich_export_bundle(&session, "s1").unwrap();
        assert!(bundle.audit_markdown.contains("| Band-Aid Probability | `84.0%`"));
        assert!(bundle.audit_markdown.contains("| Decision Confidence | `91.0%`"));

        // When absent, rows should not appear
        session.candidates[0].blast_radius.as_mut().unwrap().bandaid_probability = None;
        session.candidates[0].blast_radius.as_mut().unwrap().decision_confidence = None;
        let bundle_none = crate::baselines::build_rich_export_bundle(&session, "s1").unwrap();
        assert!(!bundle_none.audit_markdown.contains("Band-Aid Probability"));
        assert!(!bundle_none.audit_markdown.contains("Decision Confidence"));
    }
}
