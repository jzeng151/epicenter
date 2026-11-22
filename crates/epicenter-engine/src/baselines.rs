use crate::repo::{
    default_sandbox_allowlist, extract_unified_diff_from_text, normalize_clone_url, truncate_str,
    RepoSnapshot,
};
use epicenter_common::{
    ExportBundleResponse, Gemini4Model, RedGateSpec, TournamentSession,
};
use epicenter_gemini::{
    CreateInteractionRequest, GeminiClient, GenerationConfig, InteractionEnvironment,
};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::time::Duration;

const BASELINE_POLL_INTERVAL: Duration = Duration::from_secs(2);
const BASELINE_TIMEOUT: Duration = Duration::from_secs(150);

/// Arm 1 Baseline: Single `gemini-4-neon` model call (no Managed Agent loop, no tournament)
/// returning a unified `git diff`.
pub async fn execute_single_model_baseline(
    gemini: Option<&GeminiClient>,
    repo_url: &str,
    parent_commit: Option<&str>,
    problem_statement: &str,
    repo_files: &BTreeMap<String, String>,
) -> anyhow::Result<String> {
    let Some(client) = gemini else {
        // Offline deterministic fallback so unit tests & dry-runs stay self-contained
        return Ok(String::new());
    };

    let snapshot = RepoSnapshot {
        sources: repo_files.clone(),
        ..Default::default()
    };
    let source_context = snapshot.prompt_focused_source_context(&[], problem_statement, 36_000);

    let prompt = format!(
        "You are solving a software engineering issue in a single shot.\n\
         Repository: {repo_url}\n\
         Parent Commit: {}\n\n\
         Problem Statement:\n{problem_statement}\n\n\
         Relevant Repository Files:\n{source_context}\n\n\
         Return ONLY a valid unified `git diff` inside a ```diff fenced block that fixes the issue cleanly.",
        parent_commit.unwrap_or("HEAD"),
    );

    let mut req = CreateInteractionRequest::for_model(Gemini4Model::Neon, prompt);
    req.system_instruction = Some(
        "Return a clean, minimal unified `git diff` solving the issue. Do not include commentary outside the diff block."
            .to_string(),
    );
    req.generation_config = Some(GenerationConfig {
        thinking_level: Some("low".to_string()),
        ..Default::default()
    });

    let resp = client.create_interaction(&req).await?;
    Ok(extract_unified_diff_from_text(&resp.extract_text()))
}

/// Arm 2 Baseline: Single `gemini-4-neon` Managed Agent sandbox (single linear attempt,
/// no Red-Gate, no parallel K tournament, no Arena/Blast-Radius audit) returning unified `git diff`.
pub async fn execute_single_managed_agent_baseline(
    gemini: Option<&GeminiClient>,
    repo_url: &str,
    parent_commit: Option<&str>,
    problem_statement: &str,
) -> anyhow::Result<String> {
    let Some(client) = gemini else {
        return Ok(String::new());
    };

    let worker_agent_id = client.ensure_neon_worker_agent().await;
    let clone_url = normalize_clone_url(repo_url);
    let checkout_step = match parent_commit.map(str::trim).filter(|s| !s.is_empty()) {
        Some(sha) => format!("First run `git checkout {sha}` inside /workspace/repo."),
        None => "Work from the checked-out HEAD inside /workspace/repo.".to_string(),
    };

    let prompt = format!(
        "You are solving a software engineering issue inside /workspace/repo.\n\
         {checkout_step}\n\
         Investigate the codebase, implement a clean fix, run relevant tests, and output the final\n\
         `git diff` inside a ```diff fenced block.\n\n\
         Problem Statement:\n{problem_statement}\n\n\
         Output ONLY the final unified `git diff` inside ```diff ... ```.",
    );

    let mut req = CreateInteractionRequest::for_agent(worker_agent_id, prompt);
    req.environment = Some(InteractionEnvironment::remote_repo(
        clone_url,
        None,
        parent_commit.map(String::from),
        default_sandbox_allowlist(),
    ));
    req.background = Some(true);

    let resp = client
        .run_background_interaction(req, BASELINE_POLL_INTERVAL, BASELINE_TIMEOUT)
        .await?;
    Ok(extract_unified_diff_from_text(&resp.extract_text()))
}

/// Builds a zero-auth `.patch` + Repro Test + rich GitHub-Flavored Markdown Blast-Radius &
/// Dual-Score Audit report for any candidate in a `TournamentSession`.
pub fn build_rich_export_bundle(
    session: &TournamentSession,
    strategy_id: &str,
) -> Option<ExportBundleResponse> {
    let cand = session
        .candidates
        .iter()
        .find(|c| c.strategy_id == strategy_id)?;

    let strategy_spec = session
        .strategy_plan
        .as_ref()
        .and_then(|p| p.strategies.iter().find(|s| s.id == strategy_id));

    let repro_patch = session
        .red_gate
        .as_ref()
        .map(|rg| rg.patch().to_string())
        .unwrap_or_default();

    let patch_filename = format!("epicenter-{}-{}.patch", session.id, cand.strategy_id);
    let git_apply_command = format!("git apply {patch_filename}");

    let mut md = String::with_capacity(4096);
    let _ = writeln!(md, "# Epicenter Blast-Radius & Dual-Score Audit Report");
    let _ = writeln!(md);
    let _ = writeln!(md, "## Session Overview");
    let _ = writeln!(md, "- **Session ID:** `{}`", session.id);
    let _ = writeln!(md, "- **Issue:** {}", session.intake.title);
    let _ = writeln!(md, "- **Repository:** {}", session.intake.repo_url);
    if let Some(ref sha) = session.intake.commit_sha {
        let _ = writeln!(md, "- **Base Commit:** `{sha}`");
    } else if let Some(ref git_ref) = session.intake.git_ref {
        let _ = writeln!(md, "- **Git Ref:** `{git_ref}`");
    }
    let _ = writeln!(md, "- **Task Kind:** `{:?}`", session.intake.task_kind);
    let _ = writeln!(md, "- **Candidate Strategy:** `{}`", cand.strategy_id);
    if let Some(spec) = strategy_spec {
        let _ = writeln!(md, "- **Strategy Title:** {}", spec.title);
        let _ = writeln!(md, "- **Depth Tier:** `{:?}`", spec.depth_tier);
        let _ = writeln!(md, "- **Thesis:** {}", spec.thesis);
        let _ = writeln!(md, "- **Tradeoff Axis:** {}", spec.tradeoff_axis);
    }

    let badges_str = if cand.badges.is_empty() {
        "None".to_string()
    } else {
        cand.badges
            .iter()
            .map(|b| b.label())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let _ = writeln!(md, "- **Crowned Badges:** {badges_str}");
    let _ = writeln!(
        md,
        "- **Pareto Optimal:** {}",
        if cand.pareto_optimal { "Yes" } else { "No" }
    );
    let _ = writeln!(
        md,
        "- **Verification:** Repro `{}` | Regression Suite `{}` | Added Caller Tests `{}`{}",
        if cand.repro_passed { "PASS" } else { "FAIL" },
        if cand.regression_suite_passed { "PASS" } else { "FAIL" },
        if cand.added_caller_tests { "Yes" } else { "No" },
        if cand.is_rescue_candidate {
            " | **0/K Rescue Candidate**"
        } else {
            ""
        }
    );
    let _ = writeln!(md);

    // Locked Red-Gate section
    if let Some(ref rg) = session.red_gate {
        let _ = writeln!(md, "## Locked Red-Gate Contract");
        match rg {
            RedGateSpec::Bug(b) => {
                let _ = writeln!(md, "- **Repro Test Path:** `{}`", b.repro_test_path);
                let _ = writeln!(md, "- **Test Command:** `{}`", b.test_command);
                let _ = writeln!(md, "- **Resolution:** `{:?}`", b.resolution);
                let _ = writeln!(
                    md,
                    "- **Verified Fails on Unmodified HEAD:** {}",
                    if b.verified_fails_on_head { "Yes" } else { "No" }
                );
                if !b.failure_excerpt.trim().is_empty() {
                    let _ = writeln!(
                        md,
                        "- **Failure Excerpt:** `{}`",
                        truncate_str(b.failure_excerpt.trim(), 220)
                    );
                }
            }
            RedGateSpec::Feature(f) => {
                let _ = writeln!(md, "- **Test Command:** `{}`", f.test_command);
                let _ = writeln!(
                    md,
                    "- **Benchmark Contract:** {}",
                    f.benchmark_entrypoint_contract
                );
            }
        }
        let _ = writeln!(md);
    }

    // Dual-Score & Arena Performance section
    let _ = writeln!(md, "## Dual-Score & Single-VM Arena Summary");
    let _ = writeln!(md, "| Metric | Value |");
    let _ = writeln!(md, "| :--- | :--- |");
    let _ = writeln!(md, "| Dual Utility Score | `{:.1}` |", cand.dual_utility_score());
    if let Some(ref br) = cand.blast_radius {
        let _ = writeln!(
            md,
            "| Root-Cause Depth Score | `{:.1} / 100` (higher is better) |",
            br.root_cause_depth_score
        );
        let _ = writeln!(
            md,
            "| Composite Blast-Radius Risk | `{:.1} / 100` (lower is safer) |",
            br.composite_risk_score
        );
        let _ = writeln!(md, "| Untested Dark-Zone Callers | `{}` |", br.dark_zone_count());
        if let Some(bandaid_prob) = br.bandaid_probability {
            let _ = writeln!(
                md,
                "| Band-Aid Probability | `{:.1}%` (calibrated `gemini-decision-1`) |",
                bandaid_prob * 100.0
            );
        }
        if let Some(confidence) = br.decision_confidence {
            let _ = writeln!(
                md,
                "| Decision Confidence | `{:.1}%` (calibrated `gemini-decision-1`) |",
                confidence * 100.0
            );
        }
    }
    if let Some(ref arena) = cand.arena_metrics {
        let _ = writeln!(md, "| Arena p50 Latency | `{:.2} ms` |", arena.p50_ms);
        let _ = writeln!(md, "| Arena p95 Latency | `{:.2} ms` |", arena.p95_ms);
        let _ = writeln!(md, "| Speedup vs. Baseline | `{:+.1}%` |", arena.speedup_pct);
        if let Some(mem) = arena.peak_memory_kb {
            let _ = writeln!(md, "| Peak Memory | `{mem} KB` |");
        }
        if let Some(calls) = arena.call_or_query_count {
            let _ = writeln!(md, "| Call / Query Count | `{calls}` |");
        }
    }
    let _ = writeln!(md);

    if let Some(ref br) = cand.blast_radius {
        if let Some(ref warning) = br.bandaid_debt_warning {
            let _ = writeln!(md, "> **Bandaid Debt Warning:** {warning}");
            let _ = writeln!(md);
        }

        let _ = writeln!(md, "## Concentric Blast-Radius Map");
        let _ = writeln!(
            md,
            "- **Ring 0 (Mutated Symbols — {}):** {}",
            br.ring0_mutated.len(),
            format_node_list(&br.ring0_mutated)
        );
        let _ = writeln!(
            md,
            "- **Ring 1 (Direct Callers — {}):** {}",
            br.ring1_direct_callers.len(),
            format_node_list(&br.ring1_direct_callers)
        );
        let _ = writeln!(
            md,
            "- **Ring 2 (Transitive Entrypoints — {}):** {}",
            br.ring2_transitive_entrypoints.len(),
            format_node_list(&br.ring2_transitive_entrypoints)
        );
        let _ = writeln!(md);

        let _ = writeln!(md, "## Semantic Hazard Audit");
        if br.semantic_hazards.is_empty() {
            let _ = writeln!(md, "_No semantic hazards detected by Gemini 4 Carbon._");
        } else {
            let _ = writeln!(md, "| Severity | Category | Symbol | Explanation | Mitigation |");
            let _ = writeln!(md, "| :--- | :--- | :--- | :--- | :--- |");
            for h in &br.semantic_hazards {
                let _ = writeln!(
                    md,
                    "| `{:?}` | `{}` | `{}` | {} | {} |",
                    h.severity,
                    escape_md_cell(&h.category),
                    escape_md_cell(&h.symbol),
                    escape_md_cell(&h.explanation),
                    escape_md_cell(&h.mitigation),
                );
            }
        }
        let _ = writeln!(md);
    }

    if !session.war_room_script.is_empty() {
        let _ = writeln!(md, "## War-Room Executive Briefing Transcript");
        for turn in &session.war_room_script {
            let _ = writeln!(md, "- **{}:** {}", turn.speaker.label(), turn.text);
        }
        let _ = writeln!(md);
    }

    let _ = writeln!(md, "## Apply Locally (Zero-Auth)");
    let _ = writeln!(md, "```bash");
    let _ = writeln!(md, "{git_apply_command}");
    let _ = writeln!(md, "```");

    Some(ExportBundleResponse {
        session_id: session.id,
        strategy_id: cand.strategy_id.clone(),
        patch_filename,
        git_diff: cand.git_diff.clone(),
        repro_test_patch: repro_patch,
        audit_markdown: md,
        git_apply_command,
    })
}

fn format_node_list(nodes: &[epicenter_common::GraphNode]) -> String {
    if nodes.is_empty() {
        return "_none_".to_string();
    }
    nodes
        .iter()
        .map(|n| {
            let dz = if n.is_untested_dark_zone {
                " **[DARK ZONE]**"
            } else {
                ""
            };
            let cov = n
                .test_coverage_pct
                .map(|p| format!(" ({p:.0}% cov)"))
                .unwrap_or_default();
            format!("`{}` (`{}:{}`){}{}", n.symbol, n.file_path, n.line, cov, dz)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn escape_md_cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}
