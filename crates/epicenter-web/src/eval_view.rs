// Owned by `neon-web-viz`: 5-Sigma 3-Way Evaluation Ladder dashboard + Live Merged GitHub PR Judge mode.
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
  gap: 16px;
  color: #e2e8f0;
  font-family: ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
}
.epi-eval-card {
  background: #0f1624;
  border: 1px solid #1e293b;
  border-radius: 14px;
  padding: 18px;
  display: flex;
  flex-direction: column;
  gap: 14px;
  box-shadow: 0 12px 32px rgba(2, 6, 23, 0.45);
}
.epi-eval-head {
  display: flex;
  flex-wrap: wrap;
  justify-content: space-between;
  align-items: center;
  gap: 10px;
  border-bottom: 1px solid #1e293b;
  padding-bottom: 12px;
}
.epi-eval-title {
  margin: 0;
  font-size: 16px;
  font-weight: 700;
  letter-spacing: 0.03em;
  text-transform: uppercase;
  color: #f8fafc;
}
.epi-eval-sub {
  font-size: 12px;
  color: #94a3b8;
  margin-top: 3px;
}
.epi-eval-btn {
  background: #131d31;
  color: #e2e8f0;
  border: 1px solid #334155;
  border-radius: 8px;
  padding: 7px 14px;
  font-size: 12px;
  font-weight: 600;
  cursor: pointer;
  transition: border-color 140ms ease, background 140ms ease, color 140ms ease;
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

.epi-metric-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(210px, 1fr));
  gap: 10px;
}
.epi-metric-card {
  background: #0b1220;
  border: 1px solid #1e293b;
  border-radius: 11px;
  padding: 12px;
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.epi-metric-name {
  font-size: 11px;
  font-weight: 700;
  text-transform: uppercase;
  letter-spacing: 0.04em;
  color: #94a3b8;
}
.epi-arm-row {
  display: flex;
  justify-content: space-between;
  align-items: center;
  font-size: 12px;
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
}
.epi-arm-dot {
  width: 8px;
  height: 8px;
  border-radius: 999px;
  display: inline-block;
  margin-right: 6px;
}

.epi-chart-frame {
  background: #080d18;
  border: 1px solid #1e293b;
  border-radius: 12px;
  padding: 12px;
}
.epi-chart-svg {
  width: 100%;
  height: auto;
  display: block;
}

.epi-judge-box {
  background: linear-gradient(145deg, rgba(14, 165, 233, 0.08), rgba(16, 185, 129, 0.07));
  border: 1px solid rgba(56, 189, 248, 0.35);
  border-radius: 14px;
  padding: 18px;
  display: flex;
  flex-direction: column;
  gap: 12px;
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
  border-radius: 9px;
  padding: 9px 12px;
  font-size: 13px;
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
  outline: none;
}
.epi-judge-input:focus {
  border-color: #38bdf8;
  box-shadow: 0 0 0 2px rgba(56, 189, 248, 0.18);
}

.epi-rubric-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(300px, 1fr));
  gap: 14px;
}
.epi-finding-card {
  background: #0b1220;
  border: 1px solid #1e293b;
  border-left: 3px solid #38bdf8;
  border-radius: 10px;
  padding: 12px;
  display: flex;
  flex-direction: column;
  gap: 6px;
  font-size: 12px;
}
.epi-finding-step {
  color: #cbd5e1;
  line-height: 1.45;
}
.epi-failure-pill {
  background: rgba(239, 68, 68, 0.10);
  border: 1px solid rgba(239, 68, 68, 0.35);
  border-radius: 9px;
  padding: 9px 11px;
  font-size: 12px;
  color: #fecaca;
  line-height: 1.4;
}

.epi-table-scroll {
  overflow-x: auto;
  border: 1px solid #1e293b;
  border-radius: 11px;
}
.epi-table {
  width: 100%;
  border-collapse: collapse;
  font-size: 12px;
  text-align: left;
}
.epi-table th {
  background: #0b1220;
  color: #94a3b8;
  font-size: 10.5px;
  text-transform: uppercase;
  letter-spacing: 0.04em;
  padding: 10px 12px;
  border-bottom: 1px solid #1e293b;
  white-space: nowrap;
}
.epi-table td {
  padding: 10px 12px;
  border-bottom: 1px solid #1e293b;
  vertical-align: top;
}
.epi-table tr:last-child td {
  border-bottom: none;
}
.epi-table tr:hover td {
  background: rgba(56, 189, 248, 0.04);
}
.epi-badge {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  padding: 2px 7px;
  border-radius: 999px;
  font-size: 10.5px;
  font-weight: 700;
  border: 1px solid #1e293b;
  background: #0b1220;
  color: #cbd5e1;
}
.epi-badge-pass { color: #10b981; border-color: rgba(16, 185, 129, 0.42); background: rgba(16, 185, 129, 0.12); }
.epi-badge-fail { color: #ef4444; border-color: rgba(239, 68, 68, 0.42); background: rgba(239, 68, 68, 0.12); }
.epi-badge-cyan { color: #38bdf8; border-color: rgba(56, 189, 248, 0.42); background: rgba(56, 189, 248, 0.10); }
.epi-badge-amber { color: #f59e0b; border-color: rgba(245, 158, 11, 0.42); background: rgba(245, 158, 11, 0.12); }
.epi-badge-violet { color: #a78bfa; border-color: rgba(167, 139, 250, 0.42); background: rgba(167, 139, 250, 0.12); }

.epi-err-banner {
  background: rgba(239, 68, 68, 0.12);
  border: 1px solid rgba(239, 68, 68, 0.42);
  border-radius: 9px;
  padding: 10px 12px;
  color: #fca5a5;
  font-size: 12px;
}
.epi-empty-state {
  padding: 32px 16px;
  text-align: center;
  color: #94a3b8;
  font-size: 13px;
  background: #0b1220;
  border: 1px dashed #1e293b;
  border-radius: 10px;
}
"#;

fn arm_color(arm: EvalArm) -> &'static str {
    match arm {
        EvalArm::SingleModelCall => "#94a3b8",
        EvalArm::SingleManagedAgent => "#f59e0b",
        EvalArm::EpicenterTournament => "#10b981",
    }
}

fn arm_short_label(arm: EvalArm) -> &'static str {
    match arm {
        EvalArm::SingleModelCall => "Arm 1 · Single Model",
        EvalArm::SingleManagedAgent => "Arm 2 · Managed Agent",
        EvalArm::EpicenterTournament => "Arm 3 · Epicenter",
    }
}

fn split_badge(split: EvalSplit) -> (&'static str, &'static str) {
    match split {
        EvalSplit::LockedUnseen => ("Locked Unseen", "epi-badge epi-badge-cyan"),
        EvalSplit::Calibration => ("Calibration", "epi-badge epi-badge-amber"),
        EvalSplit::LiveJudge => ("Live Judge", "epi-badge epi-badge-violet"),
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

fn filter_eval(eval: &FiveSigmaEval, filter: EvalFilter) -> (Vec<EvalIssueCase>, Vec<ArmOutcome>, EvalScorecard) {
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
            let resp = Request::get("/api/eval/latest")
                .send()
                .await
                .map_err(|e| format!("Network error fetching /api/eval/latest: {e}"))?;
            let status = resp.status();
            let text = resp
                .text()
                .await
                .map_err(|e| format!("Failed reading /api/eval/latest body: {e}"))?;
            if status == 404 {
                return Ok::<Option<FiveSigmaEval>, String>(None);
            }
            if !(200..300).contains(&status) {
                return Err(format!("GET /api/eval/latest returned HTTP {status}: {text}"));
            }
            let trimmed = text.trim();
            if trimmed.is_empty() || trimmed == "null" {
                return Ok(None);
            }
            serde_json::from_str::<Option<FiveSigmaEval>>(trimmed)
                .map_err(|e| format!("Invalid FiveSigmaEval JSON: {e}"))
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

    // Live Merged GitHub PR Judge signals
    let pr_url_input = RwSignal::new(String::new());
    let judging = RwSignal::new(false);
    let judge_error = RwSignal::new(Option::<String>::None);
    let live_judge_eval = RwSignal::new(Option::<FiveSigmaEval>::None);

    // Load latest eval on mount
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
                let req = Request::post("/api/eval/judge-pr")
                    .header("Content-Type", "application/json")
                    .body(body)
                    .map_err(|e| format!("Failed building request: {e}"))?;
                let resp = req
                    .send()
                    .await
                    .map_err(|e| format!("Network error calling /api/eval/judge-pr: {e}"))?;
                let status = resp.status();
                let text = resp
                    .text()
                    .await
                    .map_err(|e| format!("Failed reading /api/eval/judge-pr response: {e}"))?;
                if !(200..300).contains(&status) {
                    return Err(format!("Live PR Judge returned HTTP {status}: {text}"));
                }
                let parsed: LiveJudgePrResponse = serde_json::from_str(&text)
                    .map_err(|e| format!("Invalid LiveJudgePrResponse JSON: {e}"))?;
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
            // 1. Live Merged GitHub PR Judge Deck
            <section class="epi-judge-box">
                <div style="display:flex; flex-wrap:wrap; justify-content:space-between; align-items:center; gap:8px;">
                    <div>
                        <h2 class="epi-eval-title" style="color:#38bdf8;">"Judge Any Merged GitHub PR Live"</h2>
                        <div class="epi-eval-sub">
                            "Paste any merged Python/TypeScript GitHub PR URL. Epicenter checks out base.sha, withholds maintainer test hunks as hidden ground truth, and runs a live 3-way blind test."
                        </div>
                    </div>
                    <span class="epi-badge epi-badge-violet">"LIVE 3-WAY BLIND HARNESS"</span>
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
                            "Running 3-Way Blind Test…"
                        } else {
                            "Run Live 3-Way Blind Test"
                        }}
                    </button>
                </form>

                {move || judge_error.get().map(|err| view! {
                    <div class="epi-err-banner">{err}</div>
                })}

                {move || live_judge_eval.get().map(|live_eval| {
                    let live_clone_for_promote = live_eval.clone();
                    view! {
                        <div class="epi-eval-card" style="background:#080d18; border-color:rgba(16,185,129,0.45);">
                            <div class="epi-eval-head">
                                <div>
                                    <h3 class="epi-eval-title" style="font-size:14px; color:#10b981;">
                                        {format!("Live Blind Verdict · {}", live_eval.label)}
                                    </h3>
                                    <div class="epi-eval-sub">
                                        {format!("Run {} · {} evaluated case(s)", live_eval.run_id, live_eval.cases.len())}
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
                                    "Inspect in Full Dashboard"
                                </button>
                            </div>
                            {render_metric_cards(&live_eval.scorecard)}
                            {render_case_table(&live_eval.cases, &live_eval.outcomes)}
                        </div>
                    }
                })}
            </section>

            // 2. Main 5-Sigma 3-Way Ladder Dashboard
            <section class="epi-eval-card">
                <div class="epi-eval-head">
                    <div>
                        <h2 class="epi-eval-title">"5-Sigma 3-Way Evaluation Ladder"</h2>
                        <div class="epi-eval-sub">
                            {move || match eval_data.get() {
                                Some(ev) => format!(
                                    "Run: {} · {} total cases ({} unseen · {} calibration · {} multimodal) · {}",
                                    ev.label,
                                    ev.scorecard.total_cases,
                                    ev.scorecard.unseen_cases,
                                    ev.scorecard.calibration_cases,
                                    ev.scorecard.multimodal_cases,
                                    ev.created_at.format("%Y-%m-%d %H:%M UTC")
                                ),
                                None => "Single Model Call vs. Single Managed Agent vs. Epicenter Tournament".to_string(),
                            }}
                        </div>
                    </div>
                    <button
                        type="button"
                        class="epi-eval-btn"
                        disabled=move || loading.get()
                        on:click=on_refresh
                    >
                        {move || if loading.get() { "Refreshing…" } else { "Refresh Latest Eval" }}
                    </button>
                </div>

                {move || error_msg.get().map(|err| view! {
                    <div class="epi-err-banner">{err}</div>
                })}

                {move || {
                    if loading.get() && eval_data.get().is_none() {
                        return view! {
                            <div class="epi-empty-state">"Loading latest 5-Sigma evaluation run from /api/eval/latest…"</div>
                        }.into_any();
                    }

                    let Some(ev) = eval_data.get() else {
                        return view! {
                            <div class="epi-empty-state">
                                "No 5-Sigma evaluation artifact found on server yet. Run `epicenter-eval run` or paste any merged GitHub PR URL above to run a live 3-way blind test!"
                            </div>
                        }.into_any();
                    };

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

                    view! {
                        // Split Filter Pills
                        <div class="epi-filter-bar">
                            <button type="button" class=all_cls on:click=move |_| active_filter.set(EvalFilter::All)>
                                {format!("All ({total_cnt})")}
                            </button>
                            <button type="button" class=unseen_cls on:click=move |_| active_filter.set(EvalFilter::LockedUnseen)>
                                {format!("Locked Unseen ({unseen_cnt})")}
                            </button>
                            <button type="button" class=calib_cls on:click=move |_| active_filter.set(EvalFilter::Calibration)>
                                {format!("Calibration ({calib_cnt})")}
                            </button>
                            <button type="button" class=media_cls on:click=move |_| active_filter.set(EvalFilter::MultimodalOnly)>
                                {format!("Multimodal Media Only ({media_cnt})")}
                            </button>
                        </div>

                        // Metric Cards
                        {render_metric_cards(&scorecard)}

                        // Grouped <svg> 3-Way Comparison Bar Chart
                        {render_grouped_bars_svg(&scorecard)}

                        // 5-Sigma Rubric Proof: Calibration Findings + Remaining Failure Modes
                        <div class="epi-rubric-grid">
                            <div style="display:flex; flex-direction:column; gap:10px;">
                                <h3 class="epi-eval-title" style="font-size:13px; color:#38bdf8;">
                                    {format!("Calibration Iterations ({})", ev.calibration_findings.len())}
                                </h3>
                                {if ev.calibration_findings.is_empty() {
                                    view! {
                                        <div class="epi-empty-state" style="padding:18px;">
                                            "No calibration findings recorded on this run."
                                        </div>
                                    }.into_any()
                                } else {
                                    ev.calibration_findings
                                        .iter()
                                        .cloned()
                                        .map(render_calibration_finding)
                                        .collect_view()
                                        .into_any()
                                }}
                            </div>

                            <div style="display:flex; flex-direction:column; gap:10px;">
                                <h3 class="epi-eval-title" style="font-size:13px; color:#f59e0b;">
                                    {format!("Remaining Failure Modes ({})", ev.remaining_failure_modes.len())}
                                </h3>
                                {if ev.remaining_failure_modes.is_empty() {
                                    view! {
                                        <div class="epi-empty-state" style="padding:18px;">
                                            "No remaining failure modes logged."
                                        </div>
                                    }.into_any()
                                } else {
                                    ev.remaining_failure_modes
                                        .iter()
                                        .cloned()
                                        .map(|mode| view! {
                                            <div class="epi-failure-pill">
                                                <strong>"• "</strong>
                                                {mode}
                                            </div>
                                        })
                                        .collect_view()
                                        .into_any()
                                }}
                            </div>
                        </div>

                        // Per-Case Outcome Table
                        <div style="display:flex; flex-direction:column; gap:8px;">
                            <h3 class="epi-eval-title" style="font-size:13px;">
                                {format!("Per-Case Ground-Truth Outcomes ({} cases)", filtered_cases.len())}
                            </h3>
                            {render_case_table(&filtered_cases, &filtered_outcomes)}
                        </div>
                    }.into_any()
                }}
            </section>
        </div>
    }
}

fn render_metric_cards(scorecard: &EvalScorecard) -> impl IntoView {
    let arms = EvalArm::all();
    let render_card = |title: &'static str, fmt_val: fn(&ArmAggregateMetrics) -> String| {
        let rows = arms
            .iter()
            .map(|&arm| {
                let val_str = find_arm_metrics(scorecard, arm)
                    .map(fmt_val)
                    .unwrap_or_else(|| "—".to_string());
                let color = arm_color(arm);
                let val_weight = if arm == EvalArm::EpicenterTournament {
                    "700"
                } else {
                    "500"
                };
                view! {
                    <div class="epi-arm-row">
                        <span style="color:#94a3b8;">
                            <span class="epi-arm-dot" style=format!("background:{color};")></span>
                            {arm_short_label(arm)}
                        </span>
                        <span style=format!("color:{color}; font-weight:{val_weight};")>{val_str}</span>
                    </div>
                }
            })
            .collect_view();

        view! {
            <div class="epi-metric-card">
                <div class="epi-metric-name">{title}</div>
                {rows}
            </div>
        }
    };

    view! {
        <div class="epi-metric-grid">
            {render_card("Ground-Truth Pass %", |m| format!("{:.1}% ({}/{})", m.resolved_rate_pct, m.resolved_count, m.total_cases))}
            {render_card("Regression-Free %", |m| format!("{:.1}%", m.regression_free_rate_pct))}
            {render_card("Mean Impact Risk ↓", |m| m.mean_blast_risk_score.map(|v| format!("{v:.1}")).unwrap_or_else(|| "—".into()))}
            {render_card("Mean Root-Cause Depth ↑", |m| m.mean_root_cause_depth.map(|v| format!("{v:.1}")).unwrap_or_else(|| "—".into()))}
            {render_card("Median Speedup %", |m| m.median_speedup_pct.map(|v| format!("{v:+.1}%")).unwrap_or_else(|| "—".into()))}
        </div>
    }
}

fn render_grouped_bars_svg(scorecard: &EvalScorecard) -> impl IntoView {
    let width = 760.0_f64;
    let height = 270.0_f64;
    let pad_l = 48.0_f64;
    let pad_r = 22.0_f64;
    let pad_t = 34.0_f64;
    let pad_b = 46.0_f64;
    let plot_w = width - pad_l - pad_r;
    let plot_h = height - pad_t - pad_b;

    let arms = EvalArm::all();

    let groups: Vec<(&'static str, [Option<f64>; 3], bool)> = vec![
        (
            "Ground-Truth Pass %",
            [
                find_arm_metrics(scorecard, EvalArm::SingleModelCall).map(|m| m.resolved_rate_pct),
                find_arm_metrics(scorecard, EvalArm::SingleManagedAgent).map(|m| m.resolved_rate_pct),
                find_arm_metrics(scorecard, EvalArm::EpicenterTournament).map(|m| m.resolved_rate_pct),
            ],
            false,
        ),
        (
            "Regression-Free %",
            [
                find_arm_metrics(scorecard, EvalArm::SingleModelCall).map(|m| m.regression_free_rate_pct),
                find_arm_metrics(scorecard, EvalArm::SingleManagedAgent).map(|m| m.regression_free_rate_pct),
                find_arm_metrics(scorecard, EvalArm::EpicenterTournament).map(|m| m.regression_free_rate_pct),
            ],
            false,
        ),
        (
            "Mean Impact Risk ↓",
            [
                find_arm_metrics(scorecard, EvalArm::SingleModelCall)
                    .and_then(|m| m.mean_blast_risk_score.map(|v| v as f64)),
                find_arm_metrics(scorecard, EvalArm::SingleManagedAgent)
                    .and_then(|m| m.mean_blast_risk_score.map(|v| v as f64)),
                find_arm_metrics(scorecard, EvalArm::EpicenterTournament)
                    .and_then(|m| m.mean_blast_risk_score.map(|v| v as f64)),
            ],
            false,
        ),
        (
            "Root-Cause Depth ↑",
            [
                find_arm_metrics(scorecard, EvalArm::SingleModelCall)
                    .and_then(|m| m.mean_root_cause_depth.map(|v| v as f64)),
                find_arm_metrics(scorecard, EvalArm::SingleManagedAgent)
                    .and_then(|m| m.mean_root_cause_depth.map(|v| v as f64)),
                find_arm_metrics(scorecard, EvalArm::EpicenterTournament)
                    .and_then(|m| m.mean_root_cause_depth.map(|v| v as f64)),
            ],
            false,
        ),
        (
            "Median Speedup %",
            [
                find_arm_metrics(scorecard, EvalArm::SingleModelCall).and_then(|m| m.median_speedup_pct),
                find_arm_metrics(scorecard, EvalArm::SingleManagedAgent).and_then(|m| m.median_speedup_pct),
                find_arm_metrics(scorecard, EvalArm::EpicenterTournament).and_then(|m| m.median_speedup_pct),
            ],
            true,
        ),
    ];

    let group_w = plot_w / (groups.len() as f64);
    let bar_w = 24.0_f64;
    let bar_gap = 6.0_f64;
    let cluster_w = bar_w * 3.0 + bar_gap * 2.0;

    let y_of = move |val: f64| {
        let clamped = val.clamp(0.0, 100.0);
        pad_t + ((100.0 - clamped) / 100.0) * plot_h
    };
    let base_y = pad_t + plot_h;

    view! {
        <div class="epi-chart-frame">
            <svg class="epi-chart-svg" viewBox="0 0 760 270">
                // Top Legend
                <g transform="translate(48, 15)">
                    <rect x="0" y="-8" width="10" height="10" rx="2" fill="#94a3b8" />
                    <text x="15" y="0" fill="#cbd5e1" font-size="10.5" font-weight="600">"Arm 1: Single Model Call"</text>

                    <rect x="175" y="-8" width="10" height="10" rx="2" fill="#f59e0b" />
                    <text x="190" y="0" fill="#cbd5e1" font-size="10.5" font-weight="600">"Arm 2: Single Managed Agent"</text>

                    <rect x="380" y="-8" width="10" height="10" rx="2" fill="#10b981" />
                    <text x="395" y="0" fill="#10b981" font-size="10.5" font-weight="700">"Arm 3: Epicenter Tournament"</text>
                </g>

                // Horizontal gridlines 0..100
                {[100.0_f64, 75.0, 50.0, 25.0, 0.0].into_iter().map(|tick| {
                    let y = y_of(tick);
                    view! {
                        <g>
                            <line
                                x1=format!("{pad_l:.1}")
                                y1=format!("{y:.1}")
                                x2=format!("{:.1}", width - pad_r)
                                y2=format!("{y:.1}")
                                stroke="#1e293b"
                                stroke-width="1"
                                stroke-dasharray="3 4"
                            />
                            <text
                                x=format!("{:.1}", pad_l - 8.0)
                                y=format!("{:.1}", y + 3.5)
                                text-anchor="end"
                                fill="#64748b"
                                font-size="9.5"
                                font-family="ui-monospace, SFMono-Regular, Menlo, monospace"
                            >
                                {format!("{tick:.0}")}
                            </text>
                        </g>
                    }
                }).collect_view()}

                // X baseline
                <line
                    x1=format!("{pad_l:.1}")
                    y1=format!("{base_y:.1}")
                    x2=format!("{:.1}", width - pad_r)
                    y2=format!("{base_y:.1}")
                    stroke="#334155"
                    stroke-width="1.3"
                />

                // Grouped bars
                {groups.into_iter().enumerate().map(|(g_idx, (label, vals, is_signed))| {
                    let gx_center = pad_l + (g_idx as f64 + 0.5) * group_w;
                    let start_x = gx_center - cluster_w * 0.5;

                    view! {
                        <g>
                            {vals.into_iter().enumerate().map(|(a_idx, opt_v)| {
                                let bx = start_x + (a_idx as f64) * (bar_w + bar_gap);
                                let color = arm_color(arms[a_idx]);
                                match opt_v {
                                    Some(v) => {
                                        let plotted = v.clamp(0.0, 100.0);
                                        let top_y = y_of(plotted);
                                        let h = (base_y - top_y).max(2.0);
                                        let txt = if is_signed {
                                            format!("{v:+.0}%")
                                        } else {
                                            format!("{v:.0}")
                                        };
                                        view! {
                                            <g>
                                                <rect
                                                    x=format!("{bx:.1}")
                                                    y=format!("{top_y:.1}")
                                                    width=format!("{bar_w:.1}")
                                                    height=format!("{h:.1}")
                                                    rx="4"
                                                    fill=color
                                                    opacity="0.92"
                                                />
                                                <text
                                                    x=format!("{:.1}", bx + bar_w * 0.5)
                                                    y=format!("{:.1}", top_y - 5.0)
                                                    text-anchor="middle"
                                                    fill=color
                                                    font-size="9"
                                                    font-weight="700"
                                                    font-family="ui-monospace, SFMono-Regular, Menlo, monospace"
                                                >
                                                    {txt}
                                                </text>
                                            </g>
                                        }.into_any()
                                    }
                                    None => view! {
                                        <g>
                                            <rect
                                                x=format!("{bx:.1}")
                                                y=format!("{:.1}", base_y - 2.0)
                                                width=format!("{bar_w:.1}")
                                                height="2"
                                                fill="#334155"
                                            />
                                            <text
                                                x=format!("{:.1}", bx + bar_w * 0.5)
                                                y=format!("{:.1}", base_y - 6.0)
                                                text-anchor="middle"
                                                fill="#475569"
                                                font-size="9"
                                            >
                                                "—"
                                            </text>
                                        </g>
                                    }.into_any(),
                                }
                            }).collect_view()}

                            <text
                                x=format!("{gx_center:.1}")
                                y=format!("{:.1}", base_y + 20.0)
                                text-anchor="middle"
                                fill="#cbd5e1"
                                font-size="10.5"
                                font-weight="600"
                            >
                                {label}
                            </text>
                        </g>
                    }
                }).collect_view()}
            </svg>
        </div>
    }
}

fn render_calibration_finding(finding: CalibrationFinding) -> impl IntoView {
    view! {
        <div class="epi-finding-card">
            <div style="display:flex; flex-wrap:wrap; justify-content:space-between; gap:6px; align-items:center;">
                <span style="font-weight:700; color:#f8fafc;">{finding.failure_mode}</span>
                <span class="epi-badge epi-badge-amber">{finding.observed_on_case}</span>
            </div>
            <div class="epi-finding-step">
                <strong style="color:#38bdf8;">"Engineering Change: "</strong>
                {finding.system_change_made}
            </div>
            <div class="epi-finding-step">
                <strong style="color:#10b981;">"Impact: "</strong>
                {finding.impact_summary}
            </div>
        </div>
    }
}

fn render_arm_cell(outcome: Option<&ArmOutcome>) -> impl IntoView {
    let Some(o) = outcome else {
        return view! { <span style="color:#475569;">"—"</span> }.into_any();
    };

    let pass_cls = if o.resolved_ground_truth {
        "epi-badge epi-badge-pass"
    } else {
        "epi-badge epi-badge-fail"
    };
    let pass_text = if o.resolved_ground_truth { "PASS" } else { "FAIL" };
    let reg_text = if o.regression_free { "Reg-Free" } else { "Regressed" };
    let reg_cls = if o.regression_free {
        "epi-badge epi-badge-cyan"
    } else {
        "epi-badge epi-badge-fail"
    };

    let stats = format!(
        "Risk {} · Depth {} · Sp {}",
        o.blast_risk_score
            .map(|v| format!("{v:.0}"))
            .unwrap_or_else(|| "—".into()),
        o.root_cause_depth_score
            .map(|v| format!("{v:.0}"))
            .unwrap_or_else(|| "—".into()),
        o.speedup_pct
            .map(|v| format!("{v:+.1}%"))
            .unwrap_or_else(|| "—".into()),
    );

    let fail_cat = o.failure_category.clone();
    let notes = o.notes.clone();

    view! {
        <div style="display:flex; flex-direction:column; gap:4px; min-width:160px;">
            <div style="display:flex; flex-wrap:wrap; gap:4px;">
                <span class=pass_cls>{pass_text}</span>
                <span class=reg_cls>{reg_text}</span>
            </div>
            <div style="font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size:10.5px; color:#94a3b8;">
                {stats}
            </div>
            {fail_cat.map(|fc| view! {
                <div style="font-size:10.5px; color:#fca5a5;">{format!("Cause: {fc}")}</div>
            })}
            {notes.map(|n| view! {
                <div style="font-size:10.5px; color:#64748b;">{n}</div>
            })}
        </div>
    }
    .into_any()
}

fn render_case_table(cases: &[EvalIssueCase], outcomes: &[ArmOutcome]) -> impl IntoView {
    if cases.is_empty() {
        return view! {
            <div class="epi-empty-state" style="padding:20px;">
                "No cases match the active filter split."
            </div>
        }
        .into_any();
    }

    let rows = cases
        .iter()
        .map(|case| {
            let (sp_lbl, sp_cls) = split_badge(case.split);
            let repo_short = repo_short_name(&case.repo_url);
            let issue_href = if let Some(pr) = case.pr_number {
                format!("{}/pull/{pr}", case.repo_url.trim_end_matches('/'))
            } else {
                format!("{}/issues/{}", case.repo_url.trim_end_matches('/'), case.issue_number)
            };

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
                <tr>
                    <td style="min-width:210px;">
                        <div style="font-weight:700; color:#f8fafc;">{title}</div>
                        <div style="display:flex; flex-wrap:wrap; gap:6px; align-items:center; margin-top:4px;">
                            <a
                                href=issue_href
                                target="_blank"
                                rel="noopener noreferrer"
                                style="color:#38bdf8; font-family:ui-monospace, SFMono-Regular, Menlo, monospace; font-size:11px; text-decoration:none;"
                            >
                                {format!("{repo_short}#{}", case.issue_number)}
                            </a>
                            <span class=sp_cls>{sp_lbl}</span>
                            {case.has_multimodal_media.then(|| view! {
                                <span class="epi-badge epi-badge-violet">"Multimodal"</span>
                            })}
                        </div>
                    </td>
                    <td>{render_arm_cell(o1)}</td>
                    <td>{render_arm_cell(o2)}</td>
                    <td>{render_arm_cell(o3)}</td>
                </tr>
            }
        })
        .collect_view();

    view! {
        <div class="epi-table-scroll">
            <table class="epi-table">
                <thead>
                    <tr>
                        <th>"Issue / Benchmark Case"</th>
                        <th>"Arm 1 · Single Model"</th>
                        <th>"Arm 2 · Managed Agent"</th>
                        <th>"Arm 3 · Epicenter"</th>
                    </tr>
                </thead>
                <tbody>{rows}</tbody>
            </table>
        </div>
    }
    .into_any()
}
