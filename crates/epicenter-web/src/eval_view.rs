// Live PR Benchmark & Evaluation Results dashboard.
use std::collections::HashSet;

use epicenter_common::{
    ArmAggregateMetrics, ArmOutcome, CalibrationFinding, EvalArm, EvalIssueCase, EvalScorecard,
    EvalSplit, FiveSigmaEval, LiveJudgePrRequest, LiveJudgePrResponse,
};
use gloo_net::http::Request;
use leptos::prelude::*;
use leptos::task::spawn_local;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EvalFilter {
    All,
    LockedUnseen,
    Calibration,
    MultimodalOnly,
}

const EVAL_STYLES: &str = r#"
.epi-eval-wrap {
  display: flex;
  flex-direction: column;
  gap: 22px;
  color: #e2e8f0;
  font-family: ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
}
.epi-eval-card {
  background: #0f1624;
  border: 1px solid #1e293b;
  border-radius: 12px;
  padding: 20px 22px;
  display: flex;
  flex-direction: column;
  gap: 18px;
  box-shadow: 0 6px 24px rgba(0, 0, 0, 0.22);
}
.epi-eval-head {
  display: flex;
  flex-wrap: wrap;
  justify-content: space-between;
  align-items: flex-start;
  gap: 12px;
  border-bottom: 1px solid #1e293b;
  padding-bottom: 14px;
}
.epi-eval-title {
  margin: 0;
  font-size: 16px;
  font-weight: 700;
  letter-spacing: 0.01em;
  color: #f8fafc;
}
.epi-eval-sub {
  font-size: 12.5px;
  color: #94a3b8;
  margin-top: 3px;
  line-height: 1.45;
}
.epi-eval-btn {
  background: #131d31;
  color: #e2e8f0;
  border: 1px solid #334155;
  border-radius: 8px;
  padding: 8px 14px;
  font-size: 12.5px;
  font-weight: 600;
  cursor: pointer;
  transition: border-color 140ms ease, background 140ms ease, color 140ms ease;
  white-space: nowrap;
}
.epi-eval-btn:hover:not(:disabled) {
  border-color: #38bdf8;
  color: #38bdf8;
  background: #16243d;
}
.epi-eval-btn:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}
.epi-eval-btn-primary {
  background: linear-gradient(135deg, #0284c7, #10b981);
  border: 1px solid rgba(56, 189, 248, 0.55);
  color: #04101a;
  font-weight: 700;
}
.epi-eval-btn-primary:hover:not(:disabled) {
  filter: brightness(1.08);
  color: #04101a;
}

.epi-judge-box {
  background: linear-gradient(145deg, rgba(14, 165, 233, 0.08), rgba(16, 185, 129, 0.05));
  border: 1px solid rgba(56, 189, 248, 0.34);
  border-radius: 12px;
  padding: 20px 22px;
  display: flex;
  flex-direction: column;
  gap: 14px;
}
.epi-judge-form {
  display: flex;
  flex-wrap: wrap;
  gap: 10px;
}
.epi-judge-input {
  flex: 1 1 320px;
  background: #080d18;
  color: #f8fafc;
  border: 1px solid #334155;
  border-radius: 8px;
  padding: 10px 13px;
  font-size: 13px;
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
  outline: none;
}
.epi-judge-input:focus {
  border-color: #38bdf8;
  box-shadow: 0 0 0 3px rgba(56, 189, 248, 0.14);
}

.epi-filter-bar {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
}
.epi-filter-pill {
  background: #0b1220;
  color: #94a3b8;
  border: 1px solid #1e293b;
  border-radius: 999px;
  padding: 6px 13px;
  font-size: 12px;
  font-weight: 600;
  cursor: pointer;
  transition: all 140ms ease;
}
.epi-filter-pill:hover {
  border-color: #38bdf8;
  color: #e2e8f0;
}
.epi-filter-pill.active {
  background: rgba(56, 189, 248, 0.14);
  border-color: #38bdf8;
  color: #38bdf8;
}

.epi-legend-strip {
  display: flex;
  flex-wrap: wrap;
  gap: 18px;
  align-items: center;
  padding: 10px 14px;
  background: #0b1220;
  border: 1px solid #1e293b;
  border-radius: 10px;
  font-size: 12.5px;
  color: #cbd5e1;
}
.epi-legend-entry {
  display: inline-flex;
  align-items: center;
  gap: 7px;
}
.epi-legend-desc {
  color: #64748b;
  font-size: 11.5px;
}
.epi-arm-dot {
  width: 9px;
  height: 9px;
  border-radius: 999px;
  display: inline-block;
  flex-shrink: 0;
}

.epi-metric-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(240px, 1fr));
  gap: 14px;
}
.epi-metric-card {
  background: #0b1220;
  border: 1px solid #1e293b;
  border-radius: 10px;
  padding: 15px 16px;
  display: flex;
  flex-direction: column;
  gap: 12px;
}
.epi-metric-head {
  display: flex;
  flex-direction: column;
  gap: 2px;
}
.epi-metric-name {
  font-size: 13px;
  font-weight: 700;
  color: #f8fafc;
}
.epi-metric-hint {
  font-size: 11.5px;
  color: #64748b;
}
.epi-bar-list {
  display: flex;
  flex-direction: column;
  gap: 9px;
}
.epi-bar-item {
  display: flex;
  flex-direction: column;
  gap: 4px;
}
.epi-bar-top {
  display: flex;
  justify-content: space-between;
  align-items: baseline;
  gap: 8px;
  font-size: 12px;
}
.epi-bar-label {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  color: #cbd5e1;
}
.epi-bar-val {
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
  font-size: 12px;
  font-weight: 700;
}
.epi-bar-track {
  width: 100%;
  height: 7px;
  border-radius: 999px;
  background: #162033;
  overflow: hidden;
}
.epi-bar-fill {
  height: 100%;
  border-radius: 999px;
  transition: width 220ms ease;
}

.epi-rubric-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(320px, 1fr));
  gap: 16px;
}
.epi-finding-card {
  background: #0b1220;
  border: 1px solid #1e293b;
  border-left: 3px solid #38bdf8;
  border-radius: 10px;
  padding: 13px 14px;
  display: flex;
  flex-direction: column;
  gap: 7px;
  font-size: 12.5px;
}
.epi-finding-step {
  color: #cbd5e1;
  line-height: 1.5;
}
.epi-failure-pill {
  background: rgba(239, 68, 68, 0.08);
  border: 1px solid rgba(239, 68, 68, 0.3);
  border-radius: 9px;
  padding: 10px 12px;
  font-size: 12.5px;
  color: #fecaca;
  line-height: 1.45;
}

/* ── Human-Readable Per-PR Comparison Cards ────────────────────── */
.epi-case-list {
  display: flex;
  flex-direction: column;
  gap: 14px;
}
.epi-case-card {
  background: #0b1220;
  border: 1px solid #1e293b;
  border-radius: 11px;
  overflow: hidden;
}
.epi-case-head {
  display: flex;
  flex-wrap: wrap;
  justify-content: space-between;
  align-items: flex-start;
  gap: 10px;
  padding: 14px 16px;
  background: #111a2c;
  border-bottom: 1px solid #1e293b;
}
.epi-case-title {
  font-size: 14px;
  font-weight: 700;
  color: #f8fafc;
  line-height: 1.35;
}
.epi-case-meta {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  align-items: center;
  margin-top: 6px;
}
.epi-arm-compare-grid {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: 12px;
  padding: 14px 16px;
}
@media (max-width: 980px) {
  .epi-arm-compare-grid {
    grid-template-columns: 1fr;
  }
}
.epi-arm-box {
  background: #0f1624;
  border: 1px solid #1e293b;
  border-radius: 9px;
  padding: 12px 13px;
  display: flex;
  flex-direction: column;
  gap: 9px;
}
.epi-arm-box.winner {
  border-color: rgba(16, 185, 129, 0.38);
  background: linear-gradient(180deg, rgba(16, 185, 129, 0.06), #0f1624 55%);
}
.epi-arm-box-head {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 8px;
}
.epi-arm-box-name {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  font-size: 12.5px;
  font-weight: 700;
  color: #e2e8f0;
}
.epi-arm-subrow {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 6px;
  font-size: 11.5px;
  color: #94a3b8;
}
.epi-arm-stats-grid {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: 8px;
  padding-top: 8px;
  border-top: 1px solid #1e293b;
}
.epi-arm-stat {
  display: flex;
  flex-direction: column;
  gap: 2px;
}
.epi-arm-stat-label {
  font-size: 10px;
  text-transform: uppercase;
  letter-spacing: 0.04em;
  color: #64748b;
}
.epi-arm-stat-val {
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
  font-size: 12px;
  font-weight: 700;
  color: #e2e8f0;
}
.epi-cause-box {
  font-size: 12px;
  color: #fecaca;
  background: rgba(239, 68, 68, 0.1);
  border: 1px solid rgba(239, 68, 68, 0.28);
  border-radius: 7px;
  padding: 6px 9px;
  line-height: 1.4;
}
.epi-note-text {
  font-size: 11.5px;
  color: #94a3b8;
  line-height: 1.45;
}
.epi-note-details {
  font-size: 11.5px;
  color: #94a3b8;
}
.epi-note-details summary {
  cursor: pointer;
  color: #7dd3fc;
  font-weight: 600;
  user-select: none;
}
.epi-note-details p {
  margin: 6px 0 0;
  line-height: 1.45;
  color: #cbd5e1;
}

.epi-badge {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  padding: 3px 8px;
  border-radius: 999px;
  font-size: 11px;
  font-weight: 600;
  border: 1px solid #1e293b;
  background: #0b1220;
  color: #cbd5e1;
}
.epi-badge-pass { color: #6ee7b7; border-color: rgba(16, 185, 129, 0.42); background: rgba(16, 185, 129, 0.14); }
.epi-badge-fail { color: #fca5a5; border-color: rgba(239, 68, 68, 0.42); background: rgba(239, 68, 68, 0.14); }
.epi-badge-cyan { color: #7dd3fc; border-color: rgba(56, 189, 248, 0.38); background: rgba(56, 189, 248, 0.10); }
.epi-badge-amber { color: #fcd34d; border-color: rgba(245, 158, 11, 0.42); background: rgba(245, 158, 11, 0.12); }
.epi-badge-violet { color: #c4b5fd; border-color: rgba(167, 139, 250, 0.42); background: rgba(167, 139, 250, 0.12); }

.epi-err-banner {
  background: rgba(239, 68, 68, 0.12);
  border: 1px solid rgba(239, 68, 68, 0.42);
  border-radius: 9px;
  padding: 10px 12px;
  color: #fca5a5;
  font-size: 12.5px;
}
.epi-sim-warning {
  background: linear-gradient(135deg, rgba(239, 68, 68, 0.2), rgba(245, 158, 11, 0.18));
  border: 1px solid rgba(245, 158, 11, 0.55);
  border-left: 4px solid #ef4444;
  border-radius: 10px;
  padding: 12px 14px;
  color: #fef3c7;
  font-size: 12.5px;
  font-weight: 600;
  line-height: 1.45;
  display: flex;
  align-items: center;
  gap: 10px;
}
.epi-live-pill {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 4px 11px;
  border-radius: 999px;
  font-size: 11.5px;
  font-weight: 700;
  color: #6ee7b7;
  background: rgba(16, 185, 129, 0.14);
  border: 1px solid rgba(16, 185, 129, 0.45);
}
.epi-sha-badge {
  display: inline-flex;
  align-items: center;
  padding: 2px 7px;
  border-radius: 999px;
  font-size: 11px;
  font-weight: 600;
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
  color: #cbd5e1;
  background: #080d18;
  border: 1px solid #334155;
  text-decoration: none;
}
.epi-sha-badge:hover {
  border-color: #38bdf8;
  color: #38bdf8;
}
.epi-empty-state {
  padding: 32px 20px;
  text-align: center;
  color: #94a3b8;
  font-size: 13px;
  line-height: 1.5;
  background: #0b1220;
  border: 1px dashed #1e293b;
  border-radius: 10px;
}
"#;

fn is_simulated_label(label: &str) -> bool {
    let lower = label.to_ascii_lowercase();
    lower.contains("dry-run")
        || lower.contains("offline")
        || lower.contains("seed")
        || lower.contains("simulated")
}

fn arm_color(arm: EvalArm) -> &'static str {
    match arm {
        EvalArm::SingleModelCall => "#94a3b8",
        EvalArm::SingleManagedAgent => "#f59e0b",
        EvalArm::EpicenterTournament => "#10b981",
    }
}

fn arm_short_label(arm: EvalArm) -> &'static str {
    match arm {
        EvalArm::SingleModelCall => "Single Prompt",
        EvalArm::SingleManagedAgent => "Single Agent",
        EvalArm::EpicenterTournament => "Epicenter",
    }
}

fn split_badge(split: EvalSplit) -> (&'static str, &'static str) {
    match split {
        EvalSplit::LockedUnseen => ("Held-Out PR", "epi-badge epi-badge-cyan"),
        EvalSplit::Calibration => ("Calibration", "epi-badge epi-badge-amber"),
        EvalSplit::LiveJudge => ("Live PR", "epi-badge epi-badge-violet"),
    }
}

fn strip_markdown_backticks(s: &str) -> String {
    s.replace('`', "")
}

fn humanize_failure_category(raw: &str) -> String {
    let cleaned = raw.trim().trim_matches('`');
    match cleaned {
        "incomplete_edge_case_fix" => "Missed an edge case".to_string(),
        "regression_in_caller_dark_zone" => "Broke an untested caller".to_string(),
        "multimodal_context_missed" => "Missed screenshot or media clues".to_string(),
        "bandaid_symptom_masking" => "Surface-level workaround instead of root fix".to_string(),
        "empty_patch" => "No code changes generated".to_string(),
        "missed_target_files" => "Did not modify the relevant source files".to_string(),
        "candidate_failed_verification" => "Failed test verification".to_string(),
        "ground_truth_test_failed" => "Failed the PR's verification tests".to_string(),
        "regression_suite_failed" => "Broke existing repository tests".to_string(),
        "patch_apply_failed" => "Generated patch did not apply cleanly".to_string(),
        "no_candidates" => "No valid approach produced".to_string(),
        "test_tampering_violation" => "Attempted to alter locked test assertions".to_string(),
        "arm1_execution_error" | "arm2_execution_error" | "arm3_execution_error" => {
            "Execution error while running".to_string()
        }
        other => {
            let spaced = other.replace('_', " ");
            let mut chars = spaced.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
                None => "Unknown issue".to_string(),
            }
        }
    }
}

fn humanize_failure_summary_line(line: &str) -> String {
    let mut s = strip_markdown_backticks(line)
        .replace("Arm 1: Single Model Call", "Single Prompt")
        .replace("Arm 2: Single Managed Agent", "Single Agent")
        .replace("Arm 3: Epicenter Tournament", "Epicenter")
        .replace("Single Model Call", "Single Prompt")
        .replace("Single Managed Agent", "Single Agent")
        .replace("Epicenter Tournament", "Epicenter");

    for raw in [
        "incomplete_edge_case_fix",
        "regression_in_caller_dark_zone",
        "multimodal_context_missed",
        "bandaid_symptom_masking",
        "empty_patch",
        "missed_target_files",
        "candidate_failed_verification",
        "ground_truth_test_failed",
        "regression_suite_failed",
        "patch_apply_failed",
        "no_candidates",
        "test_tampering_violation",
        "arm1_execution_error",
        "arm2_execution_error",
        "arm3_execution_error",
    ] {
        if s.contains(raw) {
            s = s.replace(raw, &humanize_failure_category(raw));
        }
    }
    s
}

fn format_duration_ms(ms: u64) -> String {
    if ms >= 60_000 {
        let mins = ms / 60_000;
        let secs = (ms % 60_000) / 1000;
        format!("{mins}m {secs}s")
    } else if ms >= 1000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        format!("{ms}ms")
    }
}

fn repo_short_name(url: &str) -> String {
    let trimmed = url.trim_end_matches('/').trim_end_matches(".git");
    let parts: Vec<&str> = trimmed.split('/').collect();
    if parts.len() >= 2 {
        format!("{}/{}", parts[parts.len() - 2], parts[parts.len() - 1])
    } else {
        trimmed.to_string()
    }
}

fn filter_eval(
    eval: &FiveSigmaEval,
    filter: EvalFilter,
) -> (Vec<EvalIssueCase>, Vec<ArmOutcome>, EvalScorecard) {
    let cases: Vec<EvalIssueCase> = eval
        .cases
        .iter()
        .filter(|c| match filter {
            EvalFilter::All => true,
            EvalFilter::LockedUnseen => c.split == EvalSplit::LockedUnseen,
            EvalFilter::Calibration => c.split == EvalSplit::Calibration,
            EvalFilter::MultimodalOnly => c.has_multimodal_media,
        })
        .cloned()
        .collect();

    let case_ids: HashSet<&str> = cases.iter().map(|c| c.id.as_str()).collect();
    let outcomes: Vec<ArmOutcome> = eval
        .outcomes
        .iter()
        .filter(|o| case_ids.contains(o.case_id.as_str()))
        .cloned()
        .collect();

    let scorecard = if filter == EvalFilter::All && cases.len() == eval.cases.len() {
        eval.scorecard.clone()
    } else {
        EvalScorecard::from_outcomes(&cases, &outcomes)
    };

    (cases, outcomes, scorecard)
}

fn find_arm_metrics(scorecard: &EvalScorecard, arm: EvalArm) -> Option<&ArmAggregateMetrics> {
    scorecard.arm_metrics.iter().find(|m| m.arm == arm)
}

fn load_latest_eval(
    eval_data: RwSignal<Option<FiveSigmaEval>>,
    loading: RwSignal<bool>,
    error_msg: RwSignal<Option<String>>,
) {
    loading.set(true);
    error_msg.set(None);
    spawn_local(async move {
        let result = async {
            let resp = Request::get(&crate::api_client::api_url("/api/eval/latest"))
                .send()
                .await
                .map_err(|e| format!("Network error fetching latest benchmark: {e}"))?;
            let status = resp.status();
            let text = resp
                .text()
                .await
                .map_err(|e| format!("Failed reading benchmark response: {e}"))?;
            if status == 404 {
                return Ok::<Option<FiveSigmaEval>, String>(None);
            }
            if !(200..300).contains(&status) {
                return Err(format!("Benchmark request returned HTTP {status}: {text}"));
            }
            let trimmed = text.trim();
            if trimmed.is_empty() || trimmed == "null" {
                return Ok(None);
            }
            serde_json::from_str::<Option<FiveSigmaEval>>(trimmed)
                .map_err(|e| format!("Invalid evaluation payload: {e}"))
        }
        .await;

        match result {
            Ok(data) => eval_data.set(data),
            Err(err) => error_msg.set(Some(err)),
        }
        loading.set(false);
    });
}

#[component]
pub fn FiveSigmaEvalView() -> impl IntoView {
    let eval_data = RwSignal::new(Option::<FiveSigmaEval>::None);
    let loading = RwSignal::new(false);
    let error_msg = RwSignal::new(Option::<String>::None);
    let active_filter = RwSignal::new(EvalFilter::All);

    // Live Merged GitHub PR Evaluator signals
    let pr_url_input = RwSignal::new(String::new());
    let judging = RwSignal::new(false);
    let judge_error = RwSignal::new(Option::<String>::None);
    let live_judge_eval = RwSignal::new(Option::<FiveSigmaEval>::None);

    // Load latest live eval on mount
    load_latest_eval(eval_data, loading, error_msg);

    let on_refresh = move |_| {
        load_latest_eval(eval_data, loading, error_msg);
    };

    let on_run_live_judge = move |ev: web_sys::SubmitEvent| {
        ev.prevent_default();
        let url = pr_url_input.get_untracked().trim().to_string();
        if url.is_empty() || judging.get_untracked() {
            return;
        }
        judging.set(true);
        judge_error.set(None);
        spawn_local(async move {
            let result = async {
                let payload = LiveJudgePrRequest { pr_url: url };
                let body = serde_json::to_string(&payload)
                    .map_err(|e| format!("Failed serializing request: {e}"))?;
                let req = Request::post(&crate::api_client::api_url("/api/eval/judge-pr"))
                    .header("Content-Type", "application/json")
                    .body(body)
                    .map_err(|e| format!("Failed building request: {e}"))?;
                let resp = req
                    .send()
                    .await
                    .map_err(|e| format!("Network error starting PR evaluation: {e}"))?;
                let status = resp.status();
                let text = resp
                    .text()
                    .await
                    .map_err(|e| format!("Failed reading PR evaluation response: {e}"))?;
                if !(200..300).contains(&status) {
                    return Err(format!("PR evaluation returned HTTP {status}: {text}"));
                }
                let parsed: LiveJudgePrResponse = serde_json::from_str(&text)
                    .map_err(|e| format!("Invalid PR evaluation JSON: {e}"))?;
                Ok::<FiveSigmaEval, String>(parsed.eval)
            }
            .await;

            match result {
                Ok(live_eval) => live_judge_eval.set(Some(live_eval)),
                Err(err) => judge_error.set(Some(err)),
            }
            judging.set(false);
        });
    };

    view! {
        <style>{EVAL_STYLES}</style>
        <div class="epi-eval-wrap">
            // 1. Evaluate Any Merged PR
            <section class="epi-judge-box">
                <div style="display:flex; flex-wrap:wrap; justify-content:space-between; align-items:flex-start; gap:10px;">
                    <div>
                        <h2 class="epi-eval-title" style="color:#7dd3fc;">"Evaluate Any Merged PR"</h2>
                        <div class="epi-eval-sub">
                            "Paste any merged Python or TypeScript GitHub PR URL. Epicenter checks out the commit before the fix, withholds the PR's tests, and compares Single Prompt, Single Agent, and Epicenter head-to-head."
                        </div>
                    </div>
                    <span class="epi-live-pill">
                        <span class="model-dot dot-emerald"></span>
                        "Live Evaluation"
                    </span>
                </div>

                <form class="epi-judge-form" on:submit=on_run_live_judge>
                    <input
                        type="url"
                        class="epi-judge-input"
                        placeholder="https://github.com/owner/repo/pull/123"
                        prop:value=move || pr_url_input.get()
                        on:input=move |ev| pr_url_input.set(event_target_value(&ev))
                    />
                    <button
                        type="submit"
                        class="epi-eval-btn epi-eval-btn-primary"
                        disabled=move || judging.get() || pr_url_input.get().trim().is_empty()
                    >
                        {move || if judging.get() {
                            "Running Live Evaluation…"
                        } else {
                            "Run Live Evaluation"
                        }}
                    </button>
                </form>

                {move || judge_error.get().map(|err| view! {
                    <div class="epi-err-banner">{err}</div>
                })}

                {move || live_judge_eval.get().map(|live_eval| {
                    let live_clone_for_promote = live_eval.clone();
                    let pr_count = live_eval.cases.len();
                    view! {
                        <div class="epi-eval-card" style="background:#080d18; border-color:rgba(16,185,129,0.4);">
                            <div class="epi-eval-head">
                                <div>
                                    <h3 class="epi-eval-title" style="font-size:14.5px; color:#6ee7b7;">
                                        {format!("Live Evaluation Result · {}", live_eval.label)}
                                    </h3>
                                    <div class="epi-eval-sub">
                                        {if pr_count == 1 {
                                            "1 PR evaluated live".to_string()
                                        } else {
                                            format!("{pr_count} PRs evaluated live")
                                        }}
                                    </div>
                                </div>
                                <button
                                    type="button"
                                    class="epi-eval-btn"
                                    on:click=move |_| {
                                        eval_data.set(Some(live_clone_for_promote.clone()));
                                        active_filter.set(EvalFilter::All);
                                    }
                                >
                                    "Inspect in Results Below"
                                </button>
                            </div>
                            {render_metric_cards(&live_eval.scorecard)}
                            {render_case_cards(&live_eval.cases, &live_eval.outcomes)}
                        </div>
                    }
                })}
            </section>

            // 2. Benchmark Results Dashboard
            <section class="epi-eval-card">
                <div class="epi-eval-head">
                    <div>
                        <h2 class="epi-eval-title">"Benchmark Results"</h2>
                        <div class="epi-eval-sub">
                            {move || match eval_data.get() {
                                Some(ev) => {
                                    let n = ev.scorecard.total_cases;
                                    let unit = if n == 1 { "PR" } else { "PRs" };
                                    format!(
                                        "{} · {n} {unit} evaluated · {}",
                                        ev.label,
                                        ev.created_at.format("%Y-%m-%d %H:%M UTC")
                                    )
                                }
                                None => "Compare Single Prompt, Single Agent, and Epicenter on real merged GitHub PRs.".to_string(),
                            }}
                        </div>
                    </div>
                    <button
                        type="button"
                        class="epi-eval-btn"
                        disabled=move || loading.get()
                        on:click=on_refresh
                    >
                        {move || if loading.get() { "Refreshing…" } else { "Refresh" }}
                    </button>
                </div>

                {move || error_msg.get().map(|err| view! {
                    <div class="epi-err-banner">{err}</div>
                })}

                {move || {
                    if loading.get() && eval_data.get().is_none() {
                        return view! {
                            <div class="epi-empty-state">"Loading latest benchmark results…"</div>
                        }.into_any();
                    }

                    let Some(ev) = eval_data.get() else {
                        return view! {
                            <div class="epi-empty-state">
                                "No live benchmark results yet. Paste any merged GitHub PR URL above to run a live head-to-head evaluation."
                            </div>
                        }.into_any();
                    };

                    let simulated = is_simulated_label(&ev.label);

                    let total_cnt = ev.cases.len();
                    let unseen_cnt = ev.cases.iter().filter(|c| c.split == EvalSplit::LockedUnseen).count();
                    let calib_cnt = ev.cases.iter().filter(|c| c.split == EvalSplit::Calibration).count();
                    let media_cnt = ev.cases.iter().filter(|c| c.has_multimodal_media).count();

                    let filter = active_filter.get();
                    let (filtered_cases, filtered_outcomes, scorecard) = filter_eval(&ev, filter);

                    let all_cls = if filter == EvalFilter::All { "epi-filter-pill active" } else { "epi-filter-pill" };
                    let unseen_cls = if filter == EvalFilter::LockedUnseen { "epi-filter-pill active" } else { "epi-filter-pill" };
                    let calib_cls = if filter == EvalFilter::Calibration { "epi-filter-pill active" } else { "epi-filter-pill" };
                    let media_cls = if filter == EvalFilter::MultimodalOnly { "epi-filter-pill active" } else { "epi-filter-pill" };

                    let has_notes_or_failures =
                        !ev.calibration_findings.is_empty() || !ev.remaining_failure_modes.is_empty();

                    view! {
                        // Data Provenance Strip
                        {if simulated {
                            view! {
                                <div class="epi-sim-warning" role="alert">
                                    <span>"⚠"</span>
                                    <span>
                                        "Simulated / dry-run data — not live benchmark results. Paste a real merged PR above to run a live evaluation."
                                    </span>
                                </div>
                            }.into_any()
                        } else {
                            view! {
                                <div style="display:flex;align-items:center;justify-content:space-between;flex-wrap:wrap;gap:8px;">
                                    <span class="epi-live-pill">
                                        <span class="model-dot dot-emerald"></span>
                                        "Live Host-Verified Run"
                                    </span>
                                    <span style="font-size:12px;color:#94a3b8;">
                                        "Scored by running withheld PR tests against the repository's pre-fix commit."
                                    </span>
                                </div>
                            }.into_any()
                        }}

                        // Split Filter Pills (only shown when multiple cases exist)
                        {(total_cnt > 1).then(|| view! {
                            <div class="epi-filter-bar">
                                <button type="button" class=all_cls on:click=move |_| active_filter.set(EvalFilter::All)>
                                    {format!("All ({total_cnt})")}
                                </button>
                                {(unseen_cnt > 0).then(|| view! {
                                    <button type="button" class=unseen_cls on:click=move |_| active_filter.set(EvalFilter::LockedUnseen)>
                                        {format!("Held-Out PRs ({unseen_cnt})")}
                                    </button>
                                })}
                                {(calib_cnt > 0).then(|| view! {
                                    <button type="button" class=calib_cls on:click=move |_| active_filter.set(EvalFilter::Calibration)>
                                        {format!("Calibration ({calib_cnt})")}
                                    </button>
                                })}
                                {(media_cnt > 0).then(|| view! {
                                    <button type="button" class=media_cls on:click=move |_| active_filter.set(EvalFilter::MultimodalOnly)>
                                        {format!("With Media ({media_cnt})")}
                                    </button>
                                })}
                            </div>
                        })}

                        // Plain-English Approach Legend
                        <div class="epi-legend-strip">
                            <span class="epi-legend-entry">
                                <span class="epi-arm-dot" style="background:#94a3b8;"></span>
                                <strong>"Single Prompt"</strong>
                                <span class="epi-legend-desc">"One-shot model call"</span>
                            </span>
                            <span class="epi-legend-entry">
                                <span class="epi-arm-dot" style="background:#f59e0b;"></span>
                                <strong>"Single Agent"</strong>
                                <span class="epi-legend-desc">"Single coding agent"</span>
                            </span>
                            <span class="epi-legend-entry">
                                <span class="epi-arm-dot" style="background:#10b981;"></span>
                                <strong style="color:#6ee7b7;">"Epicenter"</strong>
                                <span class="epi-legend-desc">"Multi-approach comparison"</span>
                            </span>
                        </div>

                        // Human-Readable Metric Comparison Cards (with directional horizontal bars)
                        {render_metric_cards(&scorecard)}

                        // Optional Failure Summary & Calibration Notes (hidden when empty)
                        {has_notes_or_failures.then(|| view! {
                            <div class="epi-rubric-grid">
                                {(!ev.remaining_failure_modes.is_empty()).then(|| view! {
                                    <div style="display:flex; flex-direction:column; gap:10px;">
                                        <h3 class="epi-eval-title" style="font-size:13.5px; color:#fcd34d;">
                                            {format!("Failure Summary ({})", ev.remaining_failure_modes.len())}
                                        </h3>
                                        {ev.remaining_failure_modes
                                            .iter()
                                            .map(|mode| {
                                                let clean = humanize_failure_summary_line(mode);
                                                view! {
                                                    <div class="epi-failure-pill">{clean}</div>
                                                }
                                            })
                                            .collect_view()}
                                    </div>
                                })}

                                {(!ev.calibration_findings.is_empty()).then(|| view! {
                                    <div style="display:flex; flex-direction:column; gap:10px;">
                                        <h3 class="epi-eval-title" style="font-size:13.5px; color:#7dd3fc;">
                                            {format!("Calibration Notes ({})", ev.calibration_findings.len())}
                                        </h3>
                                        {ev.calibration_findings
                                            .iter()
                                            .cloned()
                                            .map(render_calibration_finding)
                                            .collect_view()}
                                    </div>
                                })}
                            </div>
                        })}

                        // Per-PR Results Cards
                        <div style="display:flex; flex-direction:column; gap:10px;">
                            <h3 class="epi-eval-title" style="font-size:14px;">
                                {let n = filtered_cases.len();
                                if n == 1 {
                                    "PR Result (1 case)".to_string()
                                } else {
                                    format!("PR Results ({n} cases)")
                                }}
                            </h3>
                            {render_case_cards(&filtered_cases, &filtered_outcomes)}
                        </div>
                    }.into_any()
                }}
            </section>
        </div>
    }
}

fn render_bar_metric_card(
    scorecard: &EvalScorecard,
    title: &'static str,
    hint: &'static str,
    extract: fn(&ArmAggregateMetrics) -> Option<(f64, String)>,
    color_by_risk: bool,
) -> impl IntoView {
    let arms = EvalArm::all();
    let rows = arms
        .iter()
        .map(|&arm| {
            let color = arm_color(arm);
            let val_pair = find_arm_metrics(scorecard, arm).and_then(extract);
            let (pct_width, display_val, bar_color) = match val_pair {
                Some((v, text)) => {
                    let w = v.clamp(0.0, 100.0);
                    let bc = if color_by_risk {
                        if v <= 25.0 {
                            "#10b981"
                        } else if v <= 55.0 {
                            "#f59e0b"
                        } else {
                            "#ef4444"
                        }
                    } else {
                        color
                    };
                    (w, text, bc)
                }
                None => (0.0, "—".to_string(), "#334155"),
            };

            view! {
                <div class="epi-bar-item">
                    <div class="epi-bar-top">
                        <span class="epi-bar-label">
                            <span class="epi-arm-dot" style=format!("background:{color};")></span>
                            {arm_short_label(arm)}
                        </span>
                        <span class="epi-bar-val" style=format!("color:{bar_color};")>{display_val}</span>
                    </div>
                    <div class="epi-bar-track">
                        <div
                            class="epi-bar-fill"
                            style=format!("width:{pct_width:.1}%; background:{bar_color};")
                        ></div>
                    </div>
                </div>
            }
        })
        .collect_view();

    view! {
        <div class="epi-metric-card">
            <div class="epi-metric-head">
                <div class="epi-metric-name">{title}</div>
                <div class="epi-metric-hint">{hint}</div>
            </div>
            <div class="epi-bar-list">{rows}</div>
        </div>
    }
}

fn render_speedup_card(scorecard: &EvalScorecard) -> impl IntoView {
    let arms = EvalArm::all();
    let rows = arms
        .iter()
        .map(|&arm| {
            let dot_color = arm_color(arm);
            let val = find_arm_metrics(scorecard, arm).and_then(|m| m.median_speedup_pct);
            let (text, text_color) = match val {
                Some(v) if v > 1.0 => (format!("{v:+.1}% faster"), "#6ee7b7"),
                Some(v) if v < -1.0 => (format!("{:.1}% slower", v.abs()), "#fca5a5"),
                Some(v) => (format!("{v:+.1}%"), "#cbd5e1"),
                None => ("Not measured".to_string(), "#64748b"),
            };
            view! {
                <div class="epi-bar-top" style="padding:4px 0; border-bottom:1px solid rgba(30,41,59,0.55);">
                    <span class="epi-bar-label">
                        <span class="epi-arm-dot" style=format!("background:{dot_color};")></span>
                        {arm_short_label(arm)}
                    </span>
                    <span class="epi-bar-val" style=format!("color:{text_color};")>{text}</span>
                </div>
            }
        })
        .collect_view();

    view! {
        <div class="epi-metric-card">
            <div class="epi-metric-head">
                <div class="epi-metric-name">"Runtime Speedup"</div>
                <div class="epi-metric-hint">"Faster is better (median vs. baseline)"</div>
            </div>
            <div class="epi-bar-list" style="gap:4px;">{rows}</div>
        </div>
    }
}

fn render_metric_cards(scorecard: &EvalScorecard) -> impl IntoView {
    view! {
        <div class="epi-metric-grid">
            {render_bar_metric_card(
                scorecard,
                "Tests Passed",
                "Higher is better (PR verification tests)",
                |m| Some((m.resolved_rate_pct, format!("{:.0}% ({}/{})", m.resolved_rate_pct, m.resolved_count, m.total_cases))),
                false,
            )}
            {render_bar_metric_card(
                scorecard,
                "No Regressions",
                "Higher is better (existing test suite)",
                |m| Some((m.regression_free_rate_pct, format!("{:.0}%", m.regression_free_rate_pct))),
                false,
            )}
            {render_bar_metric_card(
                scorecard,
                "Impact Risk",
                "Lower is safer (0–100 scale)",
                |m| m.mean_blast_risk_score.map(|v| (v as f64, format!("{v:.0} / 100"))),
                true,
            )}
            {render_bar_metric_card(
                scorecard,
                "Root-Cause Quality",
                "Higher is deeper (0–100 scale)",
                |m| m.mean_root_cause_depth.map(|v| (v as f64, format!("{v:.0} / 100"))),
                false,
            )}
            {render_speedup_card(scorecard)}
        </div>
    }
}

fn render_calibration_finding(finding: CalibrationFinding) -> impl IntoView {
    let failure_mode = strip_markdown_backticks(&finding.failure_mode);
    let system_change = strip_markdown_backticks(&finding.system_change_made);
    let impact = strip_markdown_backticks(&finding.impact_summary);

    view! {
        <div class="epi-finding-card">
            <div style="display:flex; flex-wrap:wrap; justify-content:space-between; gap:6px; align-items:center;">
                <span style="font-weight:700; color:#f8fafc;">{failure_mode}</span>
                <span class="epi-badge epi-badge-amber">{finding.observed_on_case}</span>
            </div>
            <div class="epi-finding-step">
                <strong style="color:#7dd3fc;">"Change: "</strong>
                {system_change}
            </div>
            <div class="epi-finding-step">
                <strong style="color:#6ee7b7;">"Result: "</strong>
                {impact}
            </div>
        </div>
    }
}

fn render_arm_outcome_box(arm: EvalArm, outcome: Option<&ArmOutcome>) -> impl IntoView {
    let dot_color = arm_color(arm);
    let arm_name = arm_short_label(arm);
    let Some(o) = outcome else {
        return view! {
            <div class="epi-arm-box">
                <div class="epi-arm-box-head">
                    <span class="epi-arm-box-name">
                        <span class="epi-arm-dot" style=format!("background:{dot_color};")></span>
                        {arm_name}
                    </span>
                    <span class="epi-badge">"Not Run"</span>
                </div>
            </div>
        }
        .into_any();
    };

    let pass_cls = if o.resolved_ground_truth {
        "epi-badge epi-badge-pass"
    } else {
        "epi-badge epi-badge-fail"
    };
    let pass_text = if o.resolved_ground_truth {
        "Passed"
    } else {
        "Failed"
    };
    let reg_cls = if o.regression_free {
        "epi-badge epi-badge-cyan"
    } else {
        "epi-badge epi-badge-fail"
    };
    let reg_text = if o.regression_free {
        "No Regressions"
    } else {
        "Regressed"
    };

    let box_cls = if arm == EvalArm::EpicenterTournament && o.resolved_ground_truth {
        "epi-arm-box winner"
    } else {
        "epi-arm-box"
    };

    let risk_str = o
        .blast_risk_score
        .map(|v| format!("{v:.0}/100"))
        .unwrap_or_else(|| "—".into());
    let depth_str = o
        .root_cause_depth_score
        .map(|v| format!("{v:.0}/100"))
        .unwrap_or_else(|| "—".into());
    let speed_str = o
        .speedup_pct
        .map(|v| format!("{v:+.1}%"))
        .unwrap_or_else(|| "—".into());

    let duration_opt = (o.duration_ms > 0).then(|| format_duration_ms(o.duration_ms));
    let fail_reason = o
        .failure_category
        .as_deref()
        .map(humanize_failure_category);
    let notes = o
        .notes
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(strip_markdown_backticks);

    view! {
        <div class=box_cls>
            <div class="epi-arm-box-head">
                <span class="epi-arm-box-name">
                    <span class="epi-arm-dot" style=format!("background:{dot_color};")></span>
                    {arm_name}
                </span>
                <span class=pass_cls>{pass_text}</span>
            </div>

            <div class="epi-arm-subrow">
                <span class=reg_cls>{reg_text}</span>
                {duration_opt.map(|d| view! {
                    <span class="mono" style="font-size:11px; color:#64748b;">{format!("Runtime: {d}")}</span>
                })}
            </div>

            <div class="epi-arm-stats-grid">
                <div class="epi-arm-stat">
                    <span class="epi-arm-stat-label">"Risk"</span>
                    <span class="epi-arm-stat-val">{risk_str}</span>
                </div>
                <div class="epi-arm-stat">
                    <span class="epi-arm-stat-label">"Root-Cause"</span>
                    <span class="epi-arm-stat-val">{depth_str}</span>
                </div>
                <div class="epi-arm-stat">
                    <span class="epi-arm-stat-label">"Speedup"</span>
                    <span class="epi-arm-stat-val">{speed_str}</span>
                </div>
            </div>

            {fail_reason.map(|reason| view! {
                <div class="epi-cause-box">
                    <strong>"Reason: "</strong>
                    {reason}
                </div>
            })}

            {notes.map(|n| {
                if n.len() > 110 {
                    view! {
                        <details class="epi-note-details">
                            <summary>"View evaluation notes"</summary>
                            <p>{n}</p>
                        </details>
                    }.into_any()
                } else {
                    view! {
                        <div class="epi-note-text">{n}</div>
                    }.into_any()
                }
            })}
        </div>
    }
    .into_any()
}

fn render_case_cards(cases: &[EvalIssueCase], outcomes: &[ArmOutcome]) -> impl IntoView {
    if cases.is_empty() {
        return view! {
            <div class="epi-empty-state" style="padding:22px;">
                "No PR cases match the active filter."
            </div>
        }
        .into_any();
    }

    let cards = cases
        .iter()
        .map(|case| {
            let (sp_lbl, sp_cls) = split_badge(case.split);
            let repo_short = repo_short_name(&case.repo_url);
            let repo_base = case.repo_url.trim_end_matches('/').trim_end_matches(".git");

            let pr_link = case.pr_number.map(|pr| {
                let href = format!("{repo_base}/pull/{pr}");
                let label = format!("{repo_short} PR #{pr}");
                (href, label)
            });
            let show_separate_issue = match case.pr_number {
                Some(pr) => case.issue_number != 0 && case.issue_number != pr,
                None => true,
            };
            let issue_link = show_separate_issue.then(|| {
                let href = format!("{repo_base}/issues/{}", case.issue_number);
                let label = if case.pr_number.is_some() {
                    format!("Issue #{}", case.issue_number)
                } else {
                    format!("{repo_short} Issue #{}", case.issue_number)
                };
                (href, label)
            });

            let commit_badge = (!case.parent_commit.trim().is_empty()).then(|| {
                let full_sha = case.parent_commit.trim().to_string();
                let short_len = 7.min(full_sha.len());
                let short_sha = full_sha[..short_len].to_string();
                let href = format!("{repo_base}/commit/{full_sha}");
                (href, short_sha, full_sha)
            });

            let o1 = outcomes
                .iter()
                .find(|o| o.case_id == case.id && o.arm == EvalArm::SingleModelCall);
            let o2 = outcomes
                .iter()
                .find(|o| o.case_id == case.id && o.arm == EvalArm::SingleManagedAgent);
            let o3 = outcomes
                .iter()
                .find(|o| o.case_id == case.id && o.arm == EvalArm::EpicenterTournament);

            let title = if case.title.is_empty() {
                case.id.clone()
            } else {
                case.title.clone()
            };

            view! {
                <article class="epi-case-card">
                    <div class="epi-case-head">
                        <div>
                            <div class="epi-case-title">{title}</div>
                            <div class="epi-case-meta">
                                {pr_link.map(|(href, label)| view! {
                                    <a
                                        href=href
                                        target="_blank"
                                        rel="noopener noreferrer"
                                        style="color:#38bdf8; font-family:ui-monospace, SFMono-Regular, Menlo, monospace; font-size:11.5px; text-decoration:none; font-weight:600;"
                                    >
                                        {label}
                                    </a>
                                })}
                                {issue_link.map(|(href, label)| view! {
                                    <a
                                        href=href
                                        target="_blank"
                                        rel="noopener noreferrer"
                                        style="color:#7dd3fc; font-family:ui-monospace, SFMono-Regular, Menlo, monospace; font-size:11.5px; text-decoration:none;"
                                    >
                                        {label}
                                    </a>
                                })}
                                {commit_badge.map(|(href, short_sha, full_sha)| view! {
                                    <a
                                        href=href
                                        target="_blank"
                                        rel="noopener noreferrer"
                                        class="epi-sha-badge"
                                        title=format!("Pre-fix commit {full_sha}")
                                    >
                                        {format!("@{short_sha}")}
                                    </a>
                                })}
                                <span class=sp_cls>{sp_lbl}</span>
                                {case.has_multimodal_media.then(|| view! {
                                    <span class="epi-badge epi-badge-violet">"Has Media"</span>
                                })}
                            </div>
                        </div>
                    </div>

                    <div class="epi-arm-compare-grid">
                        {render_arm_outcome_box(EvalArm::SingleModelCall, o1)}
                        {render_arm_outcome_box(EvalArm::SingleManagedAgent, o2)}
                        {render_arm_outcome_box(EvalArm::EpicenterTournament, o3)}
                    </div>
                </article>
            }
        })
        .collect_view();

    view! {
        <div class="epi-case-list">{cards}</div>
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::{humanize_failure_category, humanize_failure_summary_line, is_simulated_label};

    #[test]
    fn detects_simulated_labels_case_insensitively() {
        assert!(is_simulated_label("5-sigma-dry-run"));
        assert!(is_simulated_label("OFFLINE_EVAL_V1"));
        assert!(is_simulated_label("Seeded Benchmark"));
        assert!(is_simulated_label("Simulated Run"));
        assert!(!is_simulated_label("live-gemini4-host-verified-v1"));
        assert!(!is_simulated_label("pallets/click#3884 live judge"));
    }

    #[test]
    fn humanizes_failure_codes_and_summaries() {
        assert_eq!(
            humanize_failure_category("regression_in_caller_dark_zone"),
            "Broke an untested caller"
        );
        assert_eq!(
            humanize_failure_category("custom_timeout_error"),
            "Custom timeout error"
        );
        let summary = humanize_failure_summary_line(
            "Arm 3: Epicenter Tournament — `incomplete_edge_case_fix` (2 cases)",
        );
        assert_eq!(summary, "Epicenter — Missed an edge case (2 cases)");
    }
}
