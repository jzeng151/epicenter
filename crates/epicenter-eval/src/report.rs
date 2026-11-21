use anyhow::{Context, Result};
use epicenter_common::{
    ArmAggregateMetrics, ArmOutcome, EvalArm, EvalIssueCase, EvalScorecard, EvalSplit,
    FiveSigmaEval,
};
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::Path;

/// 3-Way evaluation scorecards partitioned across All, Locked Unseen, Calibration, and Multimodal subsets.
#[derive(Debug, Clone)]
pub struct SubsetScorecards {
    pub all: EvalScorecard,
    pub locked_unseen: EvalScorecard,
    pub calibration: EvalScorecard,
    pub multimodal_only: EvalScorecard,
}

/// Computes `EvalScorecard` slices for All, Locked Unseen, Calibration, and Multimodal-Only subsets.
pub fn compute_subset_scorecards(eval: &FiveSigmaEval) -> SubsetScorecards {
    let all = EvalScorecard::from_outcomes(&eval.cases, &eval.outcomes);
    let locked_unseen = filter_scorecard(&eval.cases, &eval.outcomes, |c| {
        c.split == EvalSplit::LockedUnseen
    });
    let calibration = filter_scorecard(&eval.cases, &eval.outcomes, |c| {
        c.split == EvalSplit::Calibration
    });
    let multimodal_only =
        filter_scorecard(&eval.cases, &eval.outcomes, |c| c.has_multimodal_media);

    SubsetScorecards {
        all,
        locked_unseen,
        calibration,
        multimodal_only,
    }
}

fn filter_scorecard<F>(
    cases: &[EvalIssueCase],
    outcomes: &[ArmOutcome],
    predicate: F,
) -> EvalScorecard
where
    F: Fn(&EvalIssueCase) -> bool,
{
    let subset_cases: Vec<EvalIssueCase> =
        cases.iter().filter(|c| predicate(c)).cloned().collect();
    let ids: HashSet<&str> = subset_cases.iter().map(|c| c.id.as_str()).collect();
    let subset_outcomes: Vec<ArmOutcome> = outcomes
        .iter()
        .filter(|o| ids.contains(o.case_id.as_str()))
        .cloned()
        .collect();
    EvalScorecard::from_outcomes(&subset_cases, &subset_outcomes)
}

/// Renders the full executive Markdown report (`FIVE_SIGMA_REPORT.md`) for a live `FiveSigmaEval` run.
pub fn render_markdown_report(eval: &FiveSigmaEval) -> String {
    let subsets = compute_subset_scorecards(eval);
    let mut md = String::with_capacity(8192);

    let _ = writeln!(md, "# Epicenter 5-Sigma 3-Way Evaluation Report\n");
    let _ = writeln!(md, "## Run Metadata\n");
    let _ = writeln!(md, "| Field | Value |");
    let _ = writeln!(md, "| --- | --- |");
    let _ = writeln!(md, "| Run ID | `{}` |", eval.run_id);
    let _ = writeln!(md, "| Run Label | `{}` |", eval.label);
    let _ = writeln!(
        md,
        "| Execution Mode | Live Gemini 4 API + Independent Host Sandbox Verification (`git apply` + real OS test exit codes) |"
    );
    let _ = writeln!(
        md,
        "| Created At (UTC) | `{}` |",
        eval.created_at.to_rfc3339()
    );
    let _ = writeln!(
        md,
        "| Dataset Breakdown | {} total ({} Calibration, {} Locked Unseen, {} Multimodal) |\n",
        subsets.all.total_cases,
        subsets.all.calibration_cases,
        subsets.all.unseen_cases,
        subsets.all.multimodal_cases
    );

    let _ = writeln!(md, "## 3-Way Scorecards\n");
    render_scorecard_section(
        &mut md,
        &format!("All Cases ({})", subsets.all.total_cases),
        &subsets.all,
    );
    render_scorecard_section(
        &mut md,
        &format!("Locked Unseen Split ({})", subsets.locked_unseen.total_cases),
        &subsets.locked_unseen,
    );
    render_scorecard_section(
        &mut md,
        &format!("Calibration Split ({})", subsets.calibration.total_cases),
        &subsets.calibration,
    );
    render_scorecard_section(
        &mut md,
        &format!(
            "Multimodal-Only Subset ({})",
            subsets.multimodal_only.total_cases
        ),
        &subsets.multimodal_only,
    );

    render_delta_highlights(&mut md, &subsets);

    let _ = writeln!(md, "## Calibration Findings (5-Issue Iteration Log)\n");
    if eval.calibration_findings.is_empty() {
        let _ = writeln!(md, "_No calibration findings recorded for this run._\n");
    } else {
        let _ = writeln!(
            md,
            "| Failure Mode | Observed Case | System Change Made | Impact Summary |"
        );
        let _ = writeln!(md, "| --- | --- | --- | --- |");
        for finding in &eval.calibration_findings {
            let _ = writeln!(
                md,
                "| {} | `{}` | {} | {} |",
                escape_pipe(&finding.failure_mode),
                escape_pipe(&finding.observed_on_case),
                escape_pipe(&finding.system_change_made),
                escape_pipe(&finding.impact_summary),
            );
        }
        let _ = writeln!(md);
    }

    let _ = writeln!(md, "## Honest Remaining Failure Modes\n");
    if eval.remaining_failure_modes.is_empty() {
        let _ = writeln!(md, "_No remaining failures recorded in this run._\n");
    } else {
        for mode in &eval.remaining_failure_modes {
            let _ = writeln!(md, "- {}", mode);
        }
        let _ = writeln!(md);
    }

    render_per_case_matrix(&mut md, eval);

    md
}

fn render_scorecard_section(md: &mut String, heading: &str, scorecard: &EvalScorecard) {
    let _ = writeln!(md, "### {heading}\n");
    if scorecard.arm_metrics.is_empty() {
        let _ = writeln!(md, "_No outcomes recorded in this subset._\n");
        return;
    }
    let _ = writeln!(
        md,
        "| Evaluation Arm | Resolved | Regression-Free | Median Speedup | Mean Blast Risk (lower=safer) | Mean Root-Cause Depth | Median Duration |"
    );
    let _ = writeln!(md, "| --- | ---: | ---: | ---: | ---: | ---: | ---: |");
    for m in &scorecard.arm_metrics {
        let speedup = m
            .median_speedup_pct
            .map(|v| format!("{v:+.1}%"))
            .unwrap_or_else(|| "n/a".to_string());
        let blast = m
            .mean_blast_risk_score
            .map(|v| format!("{v:.1}"))
            .unwrap_or_else(|| "n/a".to_string());
        let rcd = m
            .mean_root_cause_depth
            .map(|v| format!("{v:.1}"))
            .unwrap_or_else(|| "n/a".to_string());
        let _ = writeln!(
            md,
            "| {} | {}/{} ({:.1}%) | {:.1}% | {} | {} | {} | {:.0} ms |",
            m.arm.label(),
            m.resolved_count,
            m.total_cases,
            m.resolved_rate_pct,
            m.regression_free_rate_pct,
            speedup,
            blast,
            rcd,
            m.median_duration_ms
        );
    }
    let _ = writeln!(md);
}

fn render_delta_highlights(md: &mut String, subsets: &SubsetScorecards) {
    fn find_arm(sc: &EvalScorecard, arm: EvalArm) -> Option<&ArmAggregateMetrics> {
        sc.arm_metrics.iter().find(|m| m.arm == arm)
    }

    if let (Some(arm1), Some(arm2), Some(arm3)) = (
        find_arm(&subsets.locked_unseen, EvalArm::SingleModelCall),
        find_arm(&subsets.locked_unseen, EvalArm::SingleManagedAgent),
        find_arm(&subsets.locked_unseen, EvalArm::EpicenterTournament),
    ) {
        let _ = writeln!(md, "## Key Delta Highlights (Locked Unseen Split)\n");
        let _ = writeln!(
            md,
            "- **Resolve Rate Gain**: Epicenter Tournament achieves **{:.1}%** vs **{:.1}%** Single Managed Agent (**{:+.1} pp**) and **{:.1}%** Single Model Call (**{:+.1} pp**).",
            arm3.resolved_rate_pct,
            arm2.resolved_rate_pct,
            arm3.resolved_rate_pct - arm2.resolved_rate_pct,
            arm1.resolved_rate_pct,
            arm3.resolved_rate_pct - arm1.resolved_rate_pct
        );
        if let (Some(r2), Some(r3)) = (arm2.mean_blast_risk_score, arm3.mean_blast_risk_score) {
            let _ = writeln!(
                md,
                "- **Blast-Radius Reduction**: Mean composite blast risk drops from **{:.1}** (Arm 2) to **{:.1}** (Arm 3, **{:+.1}** pts).",
                r2,
                r3,
                r3 - r2
            );
        }
        if let (Some(d2), Some(d3)) = (arm2.mean_root_cause_depth, arm3.mean_root_cause_depth) {
            let _ = writeln!(
                md,
                "- **Root-Cause Cure Depth**: Mean root-cause depth improves from **{:.1}** (Arm 2) to **{:.1}** (Arm 3, **{:+.1}** pts).",
                d2,
                d3,
                d3 - d2
            );
        }
        if let (Some(mm2), Some(mm3)) = (
            find_arm(&subsets.multimodal_only, EvalArm::SingleManagedAgent),
            find_arm(&subsets.multimodal_only, EvalArm::EpicenterTournament),
        ) {
            let _ = writeln!(
                md,
                "- **Multimodal Advantage**: On screenshot/trace-bearing issues, Epicenter resolves **{:.1}%** vs **{:.1}%** Single Managed Agent (**{:+.1} pp**).",
                mm3.resolved_rate_pct,
                mm2.resolved_rate_pct,
                mm3.resolved_rate_pct - mm2.resolved_rate_pct
            );
        }
        let _ = writeln!(md);
    }
}

fn render_per_case_matrix(md: &mut String, eval: &FiveSigmaEval) {
    let mut lookup: HashMap<(&str, EvalArm), &ArmOutcome> = HashMap::new();
    for out in &eval.outcomes {
        lookup.insert((out.case_id.as_str(), out.arm), out);
    }

    let _ = writeln!(md, "## Per-Case Outcome Matrix\n");
    let _ = writeln!(
        md,
        "| Case ID | Split | Media | Arm 1: Single Model | Arm 2: Single Agent | Arm 3: Epicenter Tournament |"
    );
    let _ = writeln!(md, "| --- | --- | :---: | --- | --- | --- |");

    for case in &eval.cases {
        let split_label = match case.split {
            EvalSplit::Calibration => "calibration",
            EvalSplit::LockedUnseen => "unseen",
            EvalSplit::LiveJudge => "live_judge",
        };
        let media = if case.has_multimodal_media { "yes" } else { "no" };
        let c1 = format_cell(lookup.get(&(case.id.as_str(), EvalArm::SingleModelCall)).copied());
        let c2 = format_cell(
            lookup
                .get(&(case.id.as_str(), EvalArm::SingleManagedAgent))
                .copied(),
        );
        let c3 = format_cell(
            lookup
                .get(&(case.id.as_str(), EvalArm::EpicenterTournament))
                .copied(),
        );
        let _ = writeln!(
            md,
            "| `{}` | {} | {} | {} | {} | {} |",
            case.id, split_label, media, c1, c2, c3
        );
    }
    let _ = writeln!(md);
}

fn format_cell(outcome: Option<&ArmOutcome>) -> String {
    let Some(o) = outcome else {
        return "—".to_string();
    };
    let risk = o
        .blast_risk_score
        .map(|r| format!("risk {:.0}", r))
        .unwrap_or_else(|| "risk n/a".to_string());
    if o.resolved_ground_truth && o.regression_free {
        format!("PASS ({risk})")
    } else {
        let reason = o
            .failure_category
            .as_deref()
            .unwrap_or("unresolved");
        format!("FAIL: `{reason}` ({risk})")
    }
}

fn escape_pipe(s: &str) -> String {
    s.replace('|', "\\|")
}

/// Persists `latest_five_sigma.json` (read by `epicenter-server` on `GET /api/eval/latest`)
/// and `FIVE_SIGMA_REPORT.md` to disk, creating parent directories if needed.
pub fn write_eval_artifacts(
    eval: &FiveSigmaEval,
    json_path: &Path,
    md_path: &Path,
) -> Result<()> {
    if let Some(parent) = json_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create directory {}", parent.display()))?;
        }
    }
    if let Some(parent) = md_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create directory {}", parent.display()))?;
        }
    }

    let json_body =
        serde_json::to_string_pretty(eval).context("failed to serialize FiveSigmaEval to JSON")?;
    std::fs::write(json_path, json_body)
        .with_context(|| format!("failed to write {}", json_path.display()))?;

    let md_body = render_markdown_report(eval);
    std::fs::write(md_path, md_body)
        .with_context(|| format!("failed to write {}", md_path.display()))?;

    Ok(())
}
