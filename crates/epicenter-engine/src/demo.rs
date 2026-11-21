use crate::briefing::build_deterministic_war_room_script;
use crate::repo::{emit_event, emit_log};
use crate::{EngineRunOptions, EngineRunOutput, UploadedAttachmentBytes};
use epicenter_common::{
    compute_pareto_frontier, crown_candidate_badges, percentile_ms, ArenaSummary,
    BenchmarkHarnessSpec, BugRedGate, CandidateArenaMetrics, CandidateResult, CandidateStatus,
    FeatureRedGate, HazardSeverity, LiveEvent, LogLevel, MediaKind, MultimodalFinding,
    RedGateResolution, RedGateSpec, SemanticHazard, StrategyDepthTier, StrategyPlan, StrategySpec,
    TaskKind, TournamentSession, TournamentStage,
};
use epicenter_graph::{BlastAnalysisConfig, CoverageMap, RepoCallGraph};
use std::collections::BTreeMap;
use tokio::sync::broadcast;

/// Executes a realistic, deterministic 7-stage offline tournament simulation when `GEMINI_API_KEY`
/// is absent. Uses real `RepoCallGraph` + `CoverageMap` analysis to produce authentic Ring 0/1/2
/// radial nodes, untested dark zones, interleaved Arena trial samples, Dual-Score badge crowning,
/// and a 2-speaker war-room briefing script while streaming all `LiveEvent`s.
pub async fn run_offline_demo_tournament(
    mut session: TournamentSession,
    attachments: Vec<UploadedAttachmentBytes>,
    _opts: EngineRunOptions,
    event_tx: Option<broadcast::Sender<LiveEvent>>,
) -> anyhow::Result<EngineRunOutput> {
    let session_id = session.id;

    // -------------------------------------------------------------------------
    // Stage 1: Intake (Hydrogen simulation)
    // -------------------------------------------------------------------------
    session.stage = TournamentStage::Intake;
    session.updated_at = chrono::Utc::now();
    emit_event(
        &event_tx,
        LiveEvent::StageChanged {
            session_id,
            stage: TournamentStage::Intake,
            message: "Stage 1 — Hydrogen multimodal intake (offline deterministic simulation)"
                .to_string(),
        },
    );

    if session.intake.title == "Analyzing issue..." || session.intake.title.trim().is_empty() {
        session.intake.title = session
            .intake
            .problem_statement
            .lines()
            .next()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.chars().take(80).collect())
            .unwrap_or_else(|| "Multi-tenant cache key collision & N+1 pricing lookup".to_string());
    }

    if session.intake.repro_steps.is_empty() {
        session.intake.repro_steps = vec![
            "Invoke pricing checkout with concurrent tenant requests sharing identical SKU keys."
                .to_string(),
            "Observe cross-tenant subtotal leak when currency/tax invariant is skipped on cache hit."
                .to_string(),
            "Measure p95 latency spike under burst batch settlement in `billing.py`.".to_string(),
        ];
    }

    if session.intake.suspect_files.is_empty() {
        session.intake.suspect_files = vec![
            "app/pricing.py".to_string(),
            "app/billing.py".to_string(),
            "app/routes.py".to_string(),
        ];
    }

    if !attachments.is_empty() {
        for att in &attachments {
            session.intake.multimodal_findings.push(MultimodalFinding {
                source_name: att.meta.filename.clone(),
                media_kind: att.meta.media_kind,
                timestamp_hint: if att.meta.media_kind == MediaKind::ScreenRecording {
                    Some("00:14".to_string())
                } else {
                    None
                },
                summary: format!(
                    "Extracted trace from '{}' showing stale pricing cache hit in `compute_total`.",
                    att.meta.filename
                ),
                extracted_symbols: vec![
                    "compute_total".to_string(),
                    "charge_invoice".to_string(),
                    "post_checkout".to_string(),
                ],
            });
        }
    } else if session.intake.multimodal_findings.is_empty() {
        session.intake.multimodal_findings.push(MultimodalFinding {
            source_name: "trace_capture.log".to_string(),
            media_kind: MediaKind::Log,
            timestamp_hint: Some("00:08".to_string()),
            summary:
                "Detected `compute_total` returning unrounded tax-exempt cached totals across tenant boundaries."
                    .to_string(),
            extracted_symbols: vec!["compute_total".to_string(), "charge_invoice".to_string()],
        });
    }

    session.updated_at = chrono::Utc::now();
    emit_event(
        &event_tx,
        LiveEvent::IntakeReady {
            session_id,
            brief: Box::new(session.intake.clone()),
        },
    );

    // -------------------------------------------------------------------------
    // Stage 2: Locked Red-Gate
    // -------------------------------------------------------------------------
    session.stage = TournamentStage::RedGate;
    session.updated_at = chrono::Utc::now();
    emit_event(
        &event_tx,
        LiveEvent::StageChanged {
            session_id,
            stage: TournamentStage::RedGate,
            message: "Stage 2 — Locked Red-Gate reproduction verified on HEAD".to_string(),
        },
    );

    let red_gate = match session.intake.task_kind {
        TaskKind::Bug => RedGateSpec::Bug(BugRedGate {
            repro_test_path: "tests/test_epicenter_repro.py".to_string(),
            repro_test_patch: concat!(
                "diff --git a/tests/test_epicenter_repro.py b/tests/test_epicenter_repro.py\n",
                "new file mode 100644\n",
                "--- /dev/null\n",
                "+++ b/tests/test_epicenter_repro.py\n",
                "@@ -0,0 +1,11 @@\n",
                "+from app.billing import charge_invoice\n",
                "+\n",
                "+def test_tenant_pricing_isolation_and_rounding():\n",
                "+    us = charge_invoice(\"tenant_a\", [10.0, 20.0], tax_rate=0.08875)\n",
                "+    eu = charge_invoice(\"tenant_b\", [10.0, 20.0], tax_rate=0.20)\n",
                "+    assert us != eu, \"cross-tenant cache collision leaked tax total\"\n",
                "+    assert round(us, 2) == 32.66\n",
                "+    assert round(eu, 2) == 36.00\n",
            )
            .to_string(),
            test_command: "pytest -q tests/test_epicenter_repro.py".to_string(),
            verified_fails_on_head: true,
            failure_excerpt: "AssertionError: cross-tenant cache collision leaked tax total (32.66 == 32.66)"
                .to_string(),
            base_environment_id: Some("env-demo-offline-01".to_string()),
            resolution: RedGateResolution::VerifiedNeon,
        }),
        TaskKind::Feature => RedGateSpec::Feature(FeatureRedGate {
            acceptance_test_patch: concat!(
                "diff --git a/tests/test_epicenter_acceptance.py b/tests/test_epicenter_acceptance.py\n",
                "new file mode 100644\n",
                "--- /dev/null\n",
                "+++ b/tests/test_epicenter_acceptance.py\n",
                "@@ -0,0 +1,8 @@\n",
                "+from app.billing import charge_invoice\n",
                "+\n",
                "+def test_acceptance_contract():\n",
                "+    assert charge_invoice(\"t1\", [10.0, 20.0], tax_rate=0.20) == 36.0\n",
            )
            .to_string(),
            benchmark_entrypoint_contract:
                "charge_invoice(tenant_id, items, tax_rate) -> float rounded to 2 decimals"
                    .to_string(),
            test_command: "pytest -q tests/test_epicenter_acceptance.py".to_string(),
            base_environment_id: Some("env-demo-offline-01".to_string()),
        }),
    };

    session.red_gate = Some(red_gate.clone());
    session.updated_at = chrono::Utc::now();
    emit_event(
        &event_tx,
        LiveEvent::RedGateVerified {
            session_id,
            red_gate: Box::new(red_gate),
        },
    );

    // -------------------------------------------------------------------------
    // Stage 3: Carbon Dynamic-K Strategy Plan (K = 3 orthogonal depth tiers)
    // -------------------------------------------------------------------------
    session.stage = TournamentStage::Planning;
    session.updated_at = chrono::Utc::now();
    emit_event(
        &event_tx,
        LiveEvent::StageChanged {
            session_id,
            stage: TournamentStage::Planning,
            message: "Stage 3 — Carbon selected K=3 orthogonal strategies across depth spectrum"
                .to_string(),
        },
    );

    let plan = StrategyPlan {
        degree_of_freedom_rationale:
            "3 orthogonal degrees of freedom: (1) localized hotfix guard disabling shared cache on mismatch, (2) algorithmic composite-key + precomputed tax lookup for peak throughput, and (3) architectural tenant-scoped invariant cure with Ring 1 caller regression tests."
                .to_string(),
        strategies: vec![
            StrategySpec {
                id: "s1-surgical-hotfix".to_string(),
                title: "Surgical Cache Bypass Guard".to_string(),
                thesis:
                    "Bypass cache lookup in `compute_total` whenever `tax_rate` differs from default, minimizing touched symbols for emergency hotfix deployment."
                        .to_string(),
                tradeoff_axis: "Smallest blast radius vs. symptom-level bandaid debt".to_string(),
                target_files_hint: vec!["app/pricing.py".to_string()],
                risk_hypothesis:
                    "Minimal diff footprint keeps structural risk lowest, but leaves upstream cache key format un-namespaced."
                        .to_string(),
                depth_tier: StrategyDepthTier::SurgicalHotfix,
                anti_overlap_rules: vec![
                    "Do not change cache key data structures or caller signatures.".to_string(),
                ],
                allow_caller_tests: false,
            },
            StrategySpec {
                id: "s2-algorithmic-cache".to_string(),
                title: "Algorithmic Tuple-Key LRU Fast Path".to_string(),
                thesis:
                    "Vectorize subtotal accumulation and key the pricing cache by `(tenant_id, items_tuple, tax_rate)` for O(1) repeat hits."
                        .to_string(),
                tradeoff_axis: "Peak Arena throughput vs. elevated cache-eviction hazard surface"
                    .to_string(),
                target_files_hint: vec![
                    "app/pricing.py".to_string(),
                    "app/billing.py".to_string(),
                ],
                risk_hypothesis:
                    "Cuts p50 latency nearly in half, though unbounded dictionary growth and untested refund callers pose semantic & dark-zone risk."
                        .to_string(),
                depth_tier: StrategyDepthTier::AlgorithmicOptimization,
                anti_overlap_rules: vec![
                    "Optimize hot-path latency; do not fall back to uncached recomputation."
                        .to_string(),
                ],
                allow_caller_tests: false,
            },
            StrategySpec {
                id: "s3-root-cause-cure".to_string(),
                title: "Tenant-Scoped Invariant Cure + Caller Tests".to_string(),
                thesis:
                    "Enforce typed `TenantPricingContext` key isolation at the authoritative cache boundary, centralize deterministic banker's rounding, and add caller regression tests for `refund_invoice` and `post_refund` to eliminate dark zones."
                        .to_string(),
                tradeoff_axis: "Maximum root-cause durability & zero dark-zone risk".to_string(),
                target_files_hint: vec![
                    "app/pricing.py".to_string(),
                    "app/billing.py".to_string(),
                    "tests/test_billing_callers.py".to_string(),
                ],
                risk_hypothesis:
                    "Adding caller regression tests lights up Ring 1/2 refund dark zones while fixing the root invariant."
                        .to_string(),
                depth_tier: StrategyDepthTier::ArchitecturalRootCause,
                anti_overlap_rules: vec![
                    "Cure the upstream cache-key invariant cleanly and add caller tests without editing the locked repro test."
                        .to_string(),
                ],
                allow_caller_tests: true,
            },
        ],
        shared_benchmark_spec: BenchmarkHarnessSpec {
            name: "pricing-checkout-batch-10k".to_string(),
            command: "python3 bench/pricing_arena.py --batch 10000".to_string(),
            setup_command: None,
            trials: 6,
            warmup_trials: 1,
            metric_unit: "ms".to_string(),
            entrypoint_contract: Some("charge_invoice(tenant_id, items, tax_rate)".to_string()),
            script_content: None,
        },
    };

    session.strategy_plan = Some(plan.clone());
    session.updated_at = chrono::Utc::now();
    emit_event(
        &event_tx,
        LiveEvent::StrategyPlanReady {
            session_id,
            plan: Box::new(plan.clone()),
        },
    );

    // -------------------------------------------------------------------------
    // Stage 4: Parallel Neon Workers (simulated diffs + green verification)
    // -------------------------------------------------------------------------
    session.stage = TournamentStage::Executing;
    session.updated_at = chrono::Utc::now();
    emit_event(
        &event_tx,
        LiveEvent::StageChanged {
            session_id,
            stage: TournamentStage::Executing,
            message: "Stage 4 — Executing 3 parallel Neon sandboxes".to_string(),
        },
    );

    let diff_s1 = concat!(
        "diff --git a/app/pricing.py b/app/pricing.py\n",
        "--- a/app/pricing.py\n",
        "+++ b/app/pricing.py\n",
        "@@ -4,7 +4,9 @@\n",
        " def compute_total(tenant_id, items, tax_rate=0.08875):\n",
        "-    cached = PRICE_CACHE.get(tuple(items))\n",
        "-    if cached is not None:\n",
        "-        return cached\n",
        "+    if tax_rate == 0.08875:\n",
        "+        cached = PRICE_CACHE.get(tuple(items))\n",
        "+        if cached is not None:\n",
        "+            return cached\n",
        "     subtotal = sum(items)\n",
        "     total = round(subtotal * (1.0 + tax_rate), 2)\n",
        "-    PRICE_CACHE[tuple(items)] = total\n",
        "+    if tax_rate == 0.08875:\n",
        "+        PRICE_CACHE[tuple(items)] = total\n",
        "     return total\n",
    );

    let diff_s2 = concat!(
        "diff --git a/app/pricing.py b/app/pricing.py\n",
        "--- a/app/pricing.py\n",
        "+++ b/app/pricing.py\n",
        "@@ -4,9 +4,10 @@\n",
        " def compute_total(tenant_id, items, tax_rate=0.08875):\n",
        "-    cached = PRICE_CACHE.get(tuple(items))\n",
        "+    key = (tenant_id, tuple(items), tax_rate)\n",
        "+    cached = PRICE_CACHE.get(key)\n",
        "     if cached is not None:\n",
        "         return cached\n",
        "     subtotal = sum(items)\n",
        "     total = round(subtotal * (1.0 + tax_rate), 2)\n",
        "-    PRICE_CACHE[tuple(items)] = total\n",
        "+    PRICE_CACHE[key] = total\n",
        "     return total\n",
        "diff --git a/app/billing.py b/app/billing.py\n",
        "--- a/app/billing.py\n",
        "+++ b/app/billing.py\n",
        "@@ -3,5 +3,6 @@\n",
        " def charge_invoice(tenant_id, items, tax_rate=0.08875):\n",
        "+    if not items:\n",
        "+        return 0.0\n",
        "     return compute_total(tenant_id, items, tax_rate)\n",
    );

    let diff_s3 = concat!(
        "diff --git a/app/pricing.py b/app/pricing.py\n",
        "--- a/app/pricing.py\n",
        "+++ b/app/pricing.py\n",
        "@@ -1,12 +1,18 @@\n",
        "+from collections import OrderedDict\n",
        "+MAX_CACHE_ENTRIES = 4096\n",
        "-PRICE_CACHE = {}\n",
        "+PRICE_CACHE = OrderedDict()\n",
        " \n",
        "+def make_cache_key(tenant_id, items, tax_rate):\n",
        "+    return (str(tenant_id), tuple(items), round(float(tax_rate), 6))\n",
        "+\n",
        " def compute_total(tenant_id, items, tax_rate=0.08875):\n",
        "-    cached = PRICE_CACHE.get(tuple(items))\n",
        "+    key = make_cache_key(tenant_id, items, tax_rate)\n",
        "+    cached = PRICE_CACHE.get(key)\n",
        "     if cached is not None:\n",
        "+        PRICE_CACHE.move_to_end(key)\n",
        "         return cached\n",
        "     subtotal = sum(items)\n",
        "     total = round(subtotal * (1.0 + tax_rate), 2)\n",
        "-    PRICE_CACHE[tuple(items)] = total\n",
        "+    PRICE_CACHE[key] = total\n",
        "+    if len(PRICE_CACHE) > MAX_CACHE_ENTRIES:\n",
        "+        PRICE_CACHE.popitem(last=False)\n",
        "     return total\n",
        "diff --git a/tests/test_billing_callers.py b/tests/test_billing_callers.py\n",
        "new file mode 100644\n",
        "--- /dev/null\n",
        "+++ b/tests/test_billing_callers.py\n",
        "@@ -0,0 +1,8 @@\n",
        "+from app.billing import refund_invoice\n",
        "+from app.routes import post_refund\n",
        "+\n",
        "+def test_refund_callers_isolated():\n",
        "+    assert refund_invoice(\"t1\", [10.0], 0.10) == -11.0\n",
        "+    assert post_refund({\"tenant\": \"t2\", \"items\": [10.0], \"tax_rate\": 0.20}) == -12.0\n",
    );

    let mut c1 = CandidateResult::pending("s1-surgical-hotfix");
    c1.status = CandidateStatus::PassedTests;
    c1.git_diff = diff_s1.to_string();
    c1.repro_passed = true;
    c1.regression_suite_passed = true;
    c1.test_output_tail = "2 passed in 0.12s (repro green, regression suite green)".to_string();

    let mut c2 = CandidateResult::pending("s2-algorithmic-cache");
    c2.status = CandidateStatus::PassedTests;
    c2.git_diff = diff_s2.to_string();
    c2.repro_passed = true;
    c2.regression_suite_passed = true;
    c2.test_output_tail = "2 passed in 0.09s (repro green, regression suite green)".to_string();

    let mut c3 = CandidateResult::pending("s3-root-cause-cure");
    c3.status = CandidateStatus::PassedTests;
    c3.git_diff = diff_s3.to_string();
    c3.repro_passed = true;
    c3.regression_suite_passed = true;
    c3.added_caller_tests = true;
    c3.test_output_tail =
        "4 passed in 0.11s (repro green, regression suite green, +2 Ring 1/2 caller tests)"
            .to_string();

    session.candidates = vec![c1, c2, c3];
    for cand in &session.candidates {
        emit_event(
            &event_tx,
            LiveEvent::CandidateStatusChanged {
                session_id,
                strategy_id: cand.strategy_id.clone(),
                status: cand.status,
                detail: cand.test_output_tail.clone(),
            },
        );
        emit_event(
            &event_tx,
            LiveEvent::CandidateUpdated {
                session_id,
                candidate: Box::new(cand.clone()),
            },
        );
    }

    // -------------------------------------------------------------------------
    // Stage 5: Single-VM Interleaved Arena
    // -------------------------------------------------------------------------
    session.stage = TournamentStage::Benchmarking;
    session.updated_at = chrono::Utc::now();
    emit_event(
        &event_tx,
        LiveEvent::StageChanged {
            session_id,
            stage: TournamentStage::Benchmarking,
            message: "Stage 5 — Single-VM Interleaved Arena (3 candidates × 6 trials)".to_string(),
        },
    );

    let baseline_samples = vec![102.4, 100.8, 101.5, 99.7, 101.2, 100.3];
    let trial_sets: [(&str, Vec<f64>, u64, u64); 3] = [
        (
            "s1-surgical-hotfix",
            vec![98.1, 97.6, 98.4, 96.9, 97.8, 97.2],
            22_400,
            10_000,
        ),
        (
            "s2-algorithmic-cache",
            vec![52.3, 51.8, 52.9, 51.4, 52.0, 51.7],
            31_800,
            1_240,
        ),
        (
            "s3-root-cause-cure",
            vec![74.6, 73.9, 75.2, 73.5, 74.1, 74.0],
            24_100,
            1_480,
        ),
    ];

    for (strategy_id, cand_samples, peak_mem_kb, calls) in trial_sets {
        for (trial_idx, (&b_ms, &c_ms)) in baseline_samples
            .iter()
            .zip(cand_samples.iter())
            .enumerate()
        {
            emit_event(
                &event_tx,
                LiveEvent::ArenaTrialSample {
                    session_id,
                    strategy_id: strategy_id.to_string(),
                    trial_index: trial_idx as u32,
                    baseline_ms: b_ms,
                    candidate_ms: c_ms,
                },
            );
        }

        let metrics = CandidateArenaMetrics::from_interleaved_samples(
            baseline_samples.clone(),
            cand_samples,
            Some(peak_mem_kb),
            Some(calls),
        );
        if let Some(cand) = session
            .candidates
            .iter_mut()
            .find(|c| c.strategy_id == strategy_id)
        {
            cand.arena_metrics = Some(metrics.clone());
            cand.status = CandidateStatus::BenchmarkDone;
            emit_event(
                &event_tx,
                LiveEvent::ArenaCandidateDone {
                    session_id,
                    strategy_id: cand.strategy_id.clone(),
                    metrics,
                },
            );
        }
    }

    let mut arena_summary = ArenaSummary {
        benchmark_name: plan.shared_benchmark_spec.name.clone(),
        baseline_p50_ms: percentile_ms(&baseline_samples, 50.0),
        baseline_p95_ms: percentile_ms(&baseline_samples, 95.0),
        trials_per_candidate: 6,
        pareto_frontier_ids: Vec::new(),
        recommended_strategy_id: None,
        summary_notes:
            "Single-VM interleaved trials completed with <1.2% baseline variance across all 3 candidates."
                .to_string(),
    };
    session.arena_summary = Some(arena_summary.clone());
    emit_event(
        &event_tx,
        LiveEvent::ArenaCompleted {
            session_id,
            summary: arena_summary.clone(),
        },
    );

    // -------------------------------------------------------------------------
    // Stage 6: 3-Layer Blast-Radius & Carbon Dual-Score Audit (real RepoCallGraph!)
    // -------------------------------------------------------------------------
    session.stage = TournamentStage::Auditing;
    session.updated_at = chrono::Utc::now();
    emit_event(
        &event_tx,
        LiveEvent::StageChanged {
            session_id,
            stage: TournamentStage::Auditing,
            message: "Stage 6 — 3-Layer Call-Graph Blast Radius & Carbon Dual-Score Audit"
                .to_string(),
        },
    );

    let sources = demo_repo_sources();
    let graph = RepoCallGraph::from_sources(&sources);
    let blast_cfg = BlastAnalysisConfig::default();

    // Baseline coverage leaves `refund_invoice` and `post_refund` as dark zones (0% coverage).
    let base_cov = CoverageMap::from_coverage_json(DEMO_BASE_COVERAGE_JSON)?;
    // Strategy 3 added caller regression tests covering `refund_invoice` and `post_refund`!
    let full_cov = CoverageMap::from_coverage_json(DEMO_FULL_COVERAGE_JSON)?;

    for cand in &mut session.candidates {
        let cov = if cand.added_caller_tests {
            &full_cov
        } else {
            &base_cov
        };
        let mut report = graph.analyze_diff(&cand.git_diff, Some(cov), &blast_cfg);

        match cand.strategy_id.as_str() {
            "s1-surgical-hotfix" => {
                // Minimal guard: lowest breakage risk, but Carbon flags bandaid tech debt.
                report.composite_risk_score = 12.0;
                report.root_cause_depth_score = 28.0;
                report.bandaid_debt_warning = Some(
                    "Bypasses cache only for non-default tax rates; leaves global un-namespaced cache key intact for multi-tenant debt."
                        .to_string(),
                );
                report.semantic_hazards = vec![SemanticHazard {
                    severity: HazardSeverity::Low,
                    category: "bandaid_invariant_debt".to_string(),
                    symbol: "compute_total".to_string(),
                    explanation:
                        "Two tenants sharing the default 0.08875 tax rate still share cache slots if tenant-specific discounts are added later."
                            .to_string(),
                    mitigation: "Scope cache keys by tenant_id at the cache layer.".to_string(),
                }];
                report.recalculate_composite_risk(8.0);
            }
            "s2-algorithmic-cache" => {
                // Fastest throughput, but unbounded dict + untouched dark-zone refund callers.
                let base_score = report.composite_risk_score.max(24.0);
                report.root_cause_depth_score = 72.0;
                report.bandaid_debt_warning = None;
                report.semantic_hazards = vec![SemanticHazard {
                    severity: HazardSeverity::High,
                    category: "unbounded_memory_growth".to_string(),
                    symbol: "compute_total".to_string(),
                    explanation:
                        "Global `PRICE_CACHE` grows without eviction bound when keyed by arbitrary `(tenant_id, items, tax_rate)` tuples."
                            .to_string(),
                    mitigation: "Use bounded LRU (`OrderedDict` or `functools.lru_cache`) with maxsize."
                        .to_string(),
                }];
                report.recalculate_composite_risk(base_score);
            }
            _ => {
                // Root-cause cure + caller tests: 0 dark zones, bounded LRU, highest root-cause depth!
                report.root_cause_depth_score = 95.0;
                report.bandaid_debt_warning = None;
                report.semantic_hazards = Vec::new();
                report.recalculate_composite_risk(16.0);
            }
        }

        let hazards = report.semantic_hazards.clone();
        let composite_risk = report.composite_risk_score;
        cand.blast_radius = Some(report.clone());
        cand.status = CandidateStatus::Audited;

        emit_event(
            &event_tx,
            LiveEvent::BlastRadiusReady {
                session_id,
                strategy_id: cand.strategy_id.clone(),
                report: Box::new(report),
            },
        );
        emit_event(
            &event_tx,
            LiveEvent::HazardAuditReady {
                session_id,
                strategy_id: cand.strategy_id.clone(),
                hazards,
                composite_risk_score: composite_risk,
            },
        );
    }

    let pareto_ids = compute_pareto_frontier(&mut session.candidates);
    let recommended = crown_candidate_badges(&mut session.candidates);

    arena_summary.pareto_frontier_ids = pareto_ids;
    arena_summary.recommended_strategy_id = recommended.clone();
    session.arena_summary = Some(arena_summary);

    for cand in &session.candidates {
        emit_event(
            &event_tx,
            LiveEvent::CandidateUpdated {
                session_id,
                candidate: Box::new(cand.clone()),
            },
        );
    }

    // -------------------------------------------------------------------------
    // Stage 7: 2-Speaker War-Room Briefing Script (offline deterministic)
    // -------------------------------------------------------------------------
    session.war_room_script = build_deterministic_war_room_script(&session);
    emit_log(
        &event_tx,
        session_id,
        LogLevel::Info,
        "briefing",
        format!(
            "Generated {}-turn Architect/Auditor war-room briefing script (offline mode)",
            session.war_room_script.len()
        ),
    );

    session.stage = TournamentStage::Completed;
    session.updated_at = chrono::Utc::now();
    emit_event(
        &event_tx,
        LiveEvent::TournamentCompleted {
            session_id,
            recommended_strategy_id: recommended,
        },
    );

    Ok(EngineRunOutput {
        session,
        briefing_wav: None,
    })
}

fn demo_repo_sources() -> BTreeMap<String, String> {
    let mut sources = BTreeMap::new();
    sources.insert(
        "app/pricing.py".to_string(),
        r#"PRICE_CACHE = {}

def compute_total(tenant_id, items, tax_rate=0.08875):
    cached = PRICE_CACHE.get(tuple(items))
    if cached is not None:
        return cached
    subtotal = sum(items)
    total = round(subtotal * (1.0 + tax_rate), 2)
    PRICE_CACHE[tuple(items)] = total
    return total
"#
        .to_string(),
    );
    sources.insert(
        "app/billing.py".to_string(),
        r#"from app.pricing import compute_total

def charge_invoice(tenant_id, items, tax_rate=0.08875):
    return compute_total(tenant_id, items, tax_rate)

def refund_invoice(tenant_id, items, tax_rate=0.08875):
    return -compute_total(tenant_id, items, tax_rate)
"#
        .to_string(),
    );
    sources.insert(
        "app/routes.py".to_string(),
        r#"from app.billing import charge_invoice, refund_invoice

def post_checkout(req):
    return charge_invoice(req["tenant"], req["items"], req.get("tax_rate", 0.08875))

def post_refund(req):
    return refund_invoice(req["tenant"], req["items"], req.get("tax_rate", 0.08875))
"#
        .to_string(),
    );
    sources
}

const DEMO_BASE_COVERAGE_JSON: &str = r#"{
  "files": {
    "app/pricing.py": {
      "executed_lines": [1, 3, 4, 5, 6, 7, 8, 9, 10],
      "missing_lines": []
    },
    "app/billing.py": {
      "executed_lines": [3, 4],
      "missing_lines": [6, 7]
    },
    "app/routes.py": {
      "executed_lines": [3, 4],
      "missing_lines": [6, 7]
    }
  }
}"#;

const DEMO_FULL_COVERAGE_JSON: &str = r#"{
  "files": {
    "app/pricing.py": {
      "executed_lines": [1, 3, 4, 5, 6, 7, 8, 9, 10],
      "missing_lines": []
    },
    "app/billing.py": {
      "executed_lines": [3, 4, 6, 7],
      "missing_lines": []
    },
    "app/routes.py": {
      "executed_lines": [3, 4, 6, 7],
      "missing_lines": []
    }
  }
}"#;
