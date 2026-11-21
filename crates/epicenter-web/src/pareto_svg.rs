// Owned by `neon-web-viz`: `<svg>` Speedup-vs-Risk Pareto scatter chart + Arena p50/p95 bars + 3 Crowned Badges.
use epicenter_common::{
    percentile_ms, CandidateBadge, CandidateResult, CandidateStatus, StrategyDepthTier,
    StrategySpec, TournamentSession,
};
use leptos::prelude::*;

const PARETO_STYLES: &str = r#"
.epi-pareto-wrap {
  display: flex;
  flex-direction: column;
  gap: 14px;
  color: #e2e8f0;
  font-family: ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
}
.epi-panel-card {
  background: #0f1624;
  border: 1px solid #1e293b;
  border-radius: 14px;
  padding: 16px;
  display: flex;
  flex-direction: column;
  gap: 12px;
  box-shadow: 0 12px 32px rgba(2, 6, 23, 0.45);
}
.epi-panel-head {
  display: flex;
  flex-wrap: wrap;
  justify-content: space-between;
  align-items: center;
  gap: 8px;
  border-bottom: 1px solid #1e293b;
  padding-bottom: 10px;
}
.epi-panel-title {
  margin: 0;
  font-size: 14px;
  font-weight: 700;
  letter-spacing: 0.03em;
  text-transform: uppercase;
  color: #f8fafc;
}
.epi-panel-sub {
  font-size: 12px;
  color: #94a3b8;
}

.epi-cand-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(230px, 1fr));
  gap: 10px;
}
.epi-cand-card {
  background: #0b1220;
  border: 1px solid #1e293b;
  border-radius: 11px;
  padding: 12px;
  text-align: left;
  color: #e2e8f0;
  cursor: pointer;
  display: flex;
  flex-direction: column;
  gap: 8px;
  transition: border-color 140ms ease, transform 140ms ease, box-shadow 140ms ease;
}
.epi-cand-card:hover {
  border-color: rgba(56, 189, 248, 0.55);
  transform: translateY(-1px);
}
.epi-cand-card.selected {
  border-color: #38bdf8;
  box-shadow: 0 0 0 1px rgba(56, 189, 248, 0.45), 0 8px 24px rgba(56, 189, 248, 0.14);
  background: #101b30;
}
.epi-cand-card.failed {
  border-color: rgba(239, 68, 68, 0.32);
  opacity: 0.88;
}
.epi-cand-top {
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
  gap: 6px;
}
.epi-cand-name {
  font-size: 13px;
  font-weight: 700;
  color: #f8fafc;
  line-height: 1.3;
}
.epi-cand-id {
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
  font-size: 10.5px;
  color: #64748b;
}
.epi-tag-row {
  display: flex;
  flex-wrap: wrap;
  gap: 5px;
}
.epi-tag {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  padding: 2px 7px;
  border-radius: 999px;
  font-size: 10.5px;
  font-weight: 600;
  border: 1px solid #1e293b;
  background: #0f1624;
  color: #cbd5e1;
}
.epi-tag-cyan { color: #38bdf8; border-color: rgba(56, 189, 248, 0.38); background: rgba(56, 189, 248, 0.10); }
.epi-tag-emerald { color: #10b981; border-color: rgba(16, 185, 129, 0.40); background: rgba(16, 185, 129, 0.12); }
.epi-tag-amber { color: #f59e0b; border-color: rgba(245, 158, 11, 0.42); background: rgba(245, 158, 11, 0.12); }
.epi-tag-red { color: #ef4444; border-color: rgba(239, 68, 68, 0.42); background: rgba(239, 68, 68, 0.12); }
.epi-tag-violet { color: #a78bfa; border-color: rgba(167, 139, 250, 0.40); background: rgba(167, 139, 250, 0.12); }
.epi-tag-gold { color: #fbbf24; border-color: rgba(251, 191, 36, 0.55); background: rgba(251, 191, 36, 0.15); }

.epi-cand-metrics {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 6px;
  padding-top: 6px;
  border-top: 1px solid #1e293b;
}
.epi-mini-stat {
  display: flex;
  flex-direction: column;
  gap: 2px;
}
.epi-mini-label {
  font-size: 9.5px;
  text-transform: uppercase;
  letter-spacing: 0.04em;
  color: #64748b;
}
.epi-mini-val {
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
  font-size: 12.5px;
  font-weight: 700;
}

.epi-svg-box {
  background: #080d18;
  border: 1px solid #1e293b;
  border-radius: 11px;
  padding: 10px;
}
.epi-svg-fluid {
  width: 100%;
  height: auto;
  display: block;
}
.epi-scatter-hover-bar {
  display: flex;
  flex-wrap: wrap;
  justify-content: space-between;
  align-items: center;
  gap: 8px;
  font-size: 11.5px;
  color: #94a3b8;
  background: #0b1220;
  border: 1px solid #1e293b;
  border-radius: 8px;
  padding: 7px 11px;
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
}

.epi-arena-telemetry {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(120px, 1fr));
  gap: 8px;
}
.epi-tele-card {
  background: #0b1220;
  border: 1px solid #1e293b;
  border-radius: 9px;
  padding: 9px 11px;
  display: flex;
  flex-direction: column;
  gap: 3px;
}
.epi-tele-label {
  font-size: 10px;
  text-transform: uppercase;
  letter-spacing: 0.04em;
  color: #64748b;
}
.epi-tele-val {
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
  font-size: 14px;
  font-weight: 700;
  color: #f8fafc;
}
.epi-tele-sub {
  font-size: 10.5px;
  color: #94a3b8;
}
.epi-empty-box {
  padding: 28px 16px;
  text-align: center;
  font-size: 12.5px;
  color: #94a3b8;
  background: #0b1220;
  border: 1px dashed #1e293b;
  border-radius: 10px;
}
"#;

fn tier_info(tier: StrategyDepthTier) -> (&'static str, &'static str) {
    match tier {
        StrategyDepthTier::SurgicalHotfix => ("Surgical Hotfix", "epi-tag epi-tag-amber"),
        StrategyDepthTier::TargetedRefactor => ("Targeted Refactor", "epi-tag epi-tag-cyan"),
        StrategyDepthTier::ArchitecturalRootCause => {
            ("Architectural Root-Cause", "epi-tag epi-tag-violet")
        }
        StrategyDepthTier::AlgorithmicOptimization => {
            ("Algorithmic Opt", "epi-tag epi-tag-cyan")
        }
    }
}

fn badge_style(badge: CandidateBadge) -> (&'static str, &'static str) {
    match badge {
        CandidateBadge::BestOverall => ("★ Best Overall", "epi-tag epi-tag-gold"),
        CandidateBadge::SafestEmergencyHotfix => ("◆ Safest Hotfix", "epi-tag epi-tag-cyan"),
        CandidateBadge::FastestThroughput => ("⚡ Fastest", "epi-tag epi-tag-emerald"),
    }
}

fn status_badge(cand: &CandidateResult) -> (&'static str, &'static str) {
    match cand.status {
        CandidateStatus::Pending => ("Pending", "epi-tag"),
        CandidateStatus::Running => ("Running", "epi-tag epi-tag-cyan"),
        CandidateStatus::FailedTests => ("Failed Tests", "epi-tag epi-tag-red"),
        CandidateStatus::PassedTests => {
            if cand.is_green() {
                ("Tests Green", "epi-tag epi-tag-emerald")
            } else {
                ("Partial Pass", "epi-tag epi-tag-amber")
            }
        }
        CandidateStatus::BenchmarkDone => ("Benchmarked", "epi-tag epi-tag-cyan"),
        CandidateStatus::Audited => {
            if cand.is_green() {
                ("Audited · Green", "epi-tag epi-tag-emerald")
            } else {
                ("Audited · Red", "epi-tag epi-tag-red")
            }
        }
    }
}

fn format_memory(kb: Option<u64>) -> String {
    match kb {
        Some(k) if k >= 1024 => format!("{:.1} MB", k as f64 / 1024.0),
        Some(k) => format!("{k} KB"),
        None => "—".to_string(),
    }
}

fn active_candidate_id(session: &TournamentSession, selected: Option<&str>) -> Option<String> {
    if let Some(id) = selected {
        if session.candidates.iter().any(|c| c.strategy_id == id) {
            return Some(id.to_string());
        }
    }
    if let Some(rec) = session
        .arena_summary
        .as_ref()
        .and_then(|s| s.recommended_strategy_id.as_ref())
    {
        if session.candidates.iter().any(|c| &c.strategy_id == rec) {
            return Some(rec.clone());
        }
    }
    session.candidates.first().map(|c| c.strategy_id.clone())
}

fn short_label(s: &str, max: usize) -> String {
    let mut it = s.chars();
    let out: String = it.by_ref().take(max).collect();
    if it.next().is_some() {
        format!("{out}…")
    } else {
        out
    }
}

#[component]
pub fn ParetoAndArenaPanel(
    session: Signal<Option<TournamentSession>>,
    selected_strategy_id: RwSignal<Option<String>>,
) -> impl IntoView {
    let hovered_scatter_id = RwSignal::new(Option::<String>::None);
    let hovered_trial_idx = RwSignal::new(Option::<usize>::None);

    view! {
        <style>{PARETO_STYLES}</style>
        <div class="epi-pareto-wrap">
            {move || {
                let Some(sess) = session.get() else {
                    return view! {
                        <section class="epi-panel-card">
                            <div class="epi-panel-head">
                                <h2 class="epi-panel-title">"Pareto Frontier & Benchmark Arena"</h2>
                                <span class="epi-panel-sub">"Dual-Score Strategy Tournament"</span>
                            </div>
                            <div class="epi-empty-box">
                                "Launch a tournament session to compare 1–5 orthogonal strategies across Impact Risk, Root-Cause Depth, and Interleaved Arena Speedup."
                            </div>
                        </section>
                    }.into_any();
                };

                let selected_id = active_candidate_id(&sess, selected_strategy_id.get().as_deref());
                let specs: Vec<StrategySpec> = sess
                    .strategy_plan
                    .as_ref()
                    .map(|p| p.strategies.clone())
                    .unwrap_or_default();

                let candidates = sess.candidates.clone();
                let arena_summary = sess.arena_summary.clone();

                let selected_candidate = selected_id
                    .as_deref()
                    .and_then(|id| candidates.iter().find(|c| c.strategy_id == id))
                    .cloned();

                view! {
                    // 1. Candidate Strategy Selector Cards
                    <section class="epi-panel-card">
                        <div class="epi-panel-head">
                            <div>
                                <h2 class="epi-panel-title">
                                    {format!("Candidate Tournament ({} Strategies)", candidates.len())}
                                </h2>
                                <div class="epi-panel-sub">
                                    {sess
                                        .strategy_plan
                                        .as_ref()
                                        .map(|p| p.degree_of_freedom_rationale.clone())
                                        .unwrap_or_else(|| "Click any strategy to inspect its Blast-Radius Ripple Map & Arena telemetry.".to_string())}
                                </div>
                            </div>
                        </div>

                        {if candidates.is_empty() {
                            view! {
                                <div class="epi-empty-box">
                                    "Waiting for Stage 3 Carbon Architect & Stage 4 Parallel Neon Workers…"
                                </div>
                            }.into_any()
                        } else {
                            view! {
                                <div class="epi-cand-grid">
                                    {candidates.iter().map(|cand| {
                                        let id_for_click = cand.strategy_id.clone();
                                        let is_sel = selected_id.as_deref() == Some(cand.strategy_id.as_str());
                                        let spec = specs.iter().find(|s| s.id == cand.strategy_id);
                                        let title = spec
                                            .map(|s| s.title.clone())
                                            .unwrap_or_else(|| cand.strategy_id.clone());
                                        let tier = spec
                                            .map(|s| s.depth_tier)
                                            .unwrap_or_default();
                                        let (tier_text, tier_cls) = tier_info(tier);
                                        let (st_text, st_cls) = status_badge(cand);

                                        let risk = cand.blast_radius.as_ref().map(|b| b.composite_risk_score);
                                        let depth = cand.blast_radius.as_ref().map(|b| b.root_cause_depth_score);
                                        let speedup = cand.arena_metrics.as_ref().map(|m| m.speedup_pct);
                                        let has_bandaid = cand
                                            .blast_radius
                                            .as_ref()
                                            .and_then(|b| b.bandaid_debt_warning.as_ref())
                                            .is_some();

                                        let card_cls = match (is_sel, cand.status == CandidateStatus::FailedTests) {
                                            (true, true) => "epi-cand-card selected failed",
                                            (true, false) => "epi-cand-card selected",
                                            (false, true) => "epi-cand-card failed",
                                            (false, false) => "epi-cand-card",
                                        };

                                        let risk_color = match risk {
                                            Some(r) if r <= 25.0 => "#10b981",
                                            Some(r) if r <= 55.0 => "#f59e0b",
                                            Some(_) => "#ef4444",
                                            None => "#64748b",
                                        };
                                        let depth_color = match depth {
                                            Some(d) if d >= 70.0 => "#10b981",
                                            Some(d) if d >= 45.0 => "#38bdf8",
                                            Some(_) => "#f59e0b",
                                            None => "#64748b",
                                        };
                                        let speed_color = match speedup {
                                            Some(sp) if sp > 2.0 => "#10b981",
                                            Some(sp) if sp < -2.0 => "#ef4444",
                                            Some(_) => "#e2e8f0",
                                            None => "#64748b",
                                        };

                                        let badges = cand.badges.clone();

                                        view! {
                                            <button
                                                type="button"
                                                class=card_cls
                                                on:click=move |_| selected_strategy_id.set(Some(id_for_click.clone()))
                                            >
                                                <div class="epi-cand-top">
                                                    <div>
                                                        <div class="epi-cand-name">{title}</div>
                                                        <div class="epi-cand-id">{cand.strategy_id.clone()}</div>
                                                    </div>
                                                    <span class=st_cls>{st_text}</span>
                                                </div>

                                                <div class="epi-tag-row">
                                                    <span class=tier_cls>{tier_text}</span>
                                                    {badges.into_iter().map(|b| {
                                                        let (b_text, b_cls) = badge_style(b);
                                                        view! { <span class=b_cls>{b_text}</span> }
                                                    }).collect_view()}
                                                    {cand.pareto_optimal.then(|| view! {
                                                        <span class="epi-tag epi-tag-cyan">"Pareto"</span>
                                                    })}
                                                    {cand.added_caller_tests.then(|| view! {
                                                        <span class="epi-tag epi-tag-emerald">"+ Caller Tests"</span>
                                                    })}
                                                    {cand.is_rescue_candidate.then(|| view! {
                                                        <span class="epi-tag epi-tag-violet">"Carbon Rescue"</span>
                                                    })}
                                                    {has_bandaid.then(|| view! {
                                                        <span class="epi-tag epi-tag-amber">"⚠ Bandaid Debt"</span>
                                                    })}
                                                </div>

                                                <div class="epi-cand-metrics">
                                                    <div class="epi-mini-stat">
                                                        <span class="epi-mini-label">"Risk ↓"</span>
                                                        <span class="epi-mini-val" style=format!("color:{risk_color};")>
                                                            {risk.map(|v| format!("{v:.1}")).unwrap_or_else(|| "—".into())}
                                                        </span>
                                                    </div>
                                                    <div class="epi-mini-stat">
                                                        <span class="epi-mini-label">"Depth ↑"</span>
                                                        <span class="epi-mini-val" style=format!("color:{depth_color};")>
                                                            {depth.map(|v| format!("{v:.0}")).unwrap_or_else(|| "—".into())}
                                                        </span>
                                                    </div>
                                                    <div class="epi-mini-stat">
                                                        <span class="epi-mini-label">"Speedup"</span>
                                                        <span class="epi-mini-val" style=format!("color:{speed_color};")>
                                                            {speedup.map(|v| format!("{v:+.1}%")).unwrap_or_else(|| "—".into())}
                                                        </span>
                                                    </div>
                                                </div>
                                            </button>
                                        }
                                    }).collect_view()}
                                </div>
                            }.into_any()
                        }}
                    </section>

                    // 2. Reactive <svg> Dual-Score / Pareto Scatter Plot
                    {render_pareto_scatter(
                        candidates.clone(),
                        specs.clone(),
                        selected_id.clone(),
                        selected_strategy_id,
                        hovered_scatter_id,
                    )}

                    // 3. Arena Interleaved Benchmark Bars & Sparkline
                    {render_arena_panel(
                        selected_candidate,
                        candidates,
                        arena_summary,
                        hovered_trial_idx,
                    )}
                }.into_any()
            }}
        </div>
    }
}

fn render_pareto_scatter(
    candidates: Vec<CandidateResult>,
    specs: Vec<StrategySpec>,
    selected_id: Option<String>,
    selected_strategy_id: RwSignal<Option<String>>,
    hovered_scatter_id: RwSignal<Option<String>>,
) -> impl IntoView {
    let plottable: Vec<&CandidateResult> = candidates
        .iter()
        .filter(|c| c.blast_radius.is_some() || c.arena_metrics.is_some())
        .collect();

    if plottable.is_empty() {
        return view! {
            <section class="epi-panel-card">
                <div class="epi-panel-head">
                    <h2 class="epi-panel-title">"Dual-Score Pareto Frontier"</h2>
                    <span class="epi-panel-sub">"Impact Risk (X) vs. Speedup % (Y) · Bubble Size = Root-Cause Depth"</span>
                </div>
                <div class="epi-empty-box">
                    "Awaiting Stage 5 Arena benchmarks & Stage 6 Carbon Dual-Score audits to plot Pareto frontier…"
                </div>
            </section>
        }.into_any();
    }

    // SVG chart layout
    let width = 640.0_f64;
    let height = 310.0_f64;
    let pad_l = 58.0_f64;
    let pad_r = 28.0_f64;
    let pad_t = 24.0_f64;
    let pad_b = 46.0_f64;
    let plot_w = width - pad_l - pad_r;
    let plot_h = height - pad_t - pad_b;

    let min_sp = plottable
        .iter()
        .filter_map(|c| c.arena_metrics.as_ref().map(|m| m.speedup_pct))
        .fold(0.0_f64, f64::min);
    let max_sp = plottable
        .iter()
        .filter_map(|c| c.arena_metrics.as_ref().map(|m| m.speedup_pct))
        .fold(0.0_f64, f64::max);

    let y_min = (min_sp.min(-5.0) - 5.0).floor().min(-10.0);
    let y_max = (max_sp.max(30.0) + 10.0).ceil().max(40.0);
    let y_span = (y_max - y_min).max(10.0);

    let x_pos = move |risk: f64| pad_l + (risk.clamp(0.0, 100.0) / 100.0) * plot_w;
    let y_pos = move |sp: f64| pad_t + ((y_max - sp.clamp(y_min, y_max)) / y_span) * plot_h;

    // Sort pareto-optimal candidates by risk asc (tie-break speedup desc) for dashed frontier line
    let mut frontier_pts: Vec<(f64, f64)> = plottable
        .iter()
        .filter(|c| c.pareto_optimal)
        .map(|c| {
            let risk = c
                .blast_radius
                .as_ref()
                .map(|b| b.composite_risk_score as f64)
                .unwrap_or(50.0);
            let sp = c
                .arena_metrics
                .as_ref()
                .map(|m| m.speedup_pct)
                .unwrap_or(0.0);
            (risk, sp)
        })
        .collect();
    frontier_pts.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| b.1.total_cmp(&a.1)));

    let frontier_polyline = frontier_pts
        .iter()
        .map(|&(r, s)| format!("{:.1},{:.1}", x_pos(r), y_pos(s)))
        .collect::<Vec<_>>()
        .join(" ");

    let zero_y = y_pos(0.0);
    let sweet_w = x_pos(35.0) - pad_l;
    let sweet_h = (zero_y - pad_t).max(0.0);

    let y_ticks = [
        y_max,
        y_min + y_span * 0.75,
        y_min + y_span * 0.50,
        y_min + y_span * 0.25,
        y_min,
    ];
    let x_ticks = [0.0_f64, 25.0, 50.0, 75.0, 100.0];

    let candidates_for_hover = candidates.clone();
    let specs_for_hover = specs.clone();
    let selected_id_hover = selected_id.clone();

    view! {
        <section class="epi-panel-card">
            <div class="epi-panel-head">
                <div>
                    <h2 class="epi-panel-title">"Dual-Score Pareto Frontier"</h2>
                    <div class="epi-panel-sub">
                        "X: Impact Risk (0..100, lower is safer) · Y: Speedup % · Radius & Ring: Root-Cause Depth (0..100)"
                    </div>
                </div>
            </div>

            <div class="epi-svg-box">
                <svg class="epi-svg-fluid" viewBox="0 0 640 310">
                    <defs>
                        <linearGradient id="epi-sweet-grad" x1="0%" y1="0%" x2="100%" y2="100%">
                            <stop offset="0%" stop-color="#10b981" stop-opacity="0.16" />
                            <stop offset="100%" stop-color="#38bdf8" stop-opacity="0.02" />
                        </linearGradient>
                    </defs>

                    // Optimal sweet-spot quadrant (low risk, positive speedup)
                    <rect
                        x=format!("{pad_l:.1}")
                        y=format!("{pad_t:.1}")
                        width=format!("{sweet_w:.1}")
                        height=format!("{sweet_h:.1}")
                        fill="url(#epi-sweet-grad)"
                    />
                    <text
                        x=format!("{:.1}", pad_l + 8.0)
                        y=format!("{:.1}", pad_t + 14.0)
                        fill="#10b981"
                        fill-opacity="0.72"
                        font-size="9.5"
                        font-weight="700"
                        letter-spacing="0.8"
                    >
                        "OPTIMAL QUADRANT · LOW RISK + FASTER"
                    </text>

                    // Horizontal gridlines & Y-axis labels
                    {y_ticks.into_iter().map(|val| {
                        let y = y_pos(val);
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
                                    font-size="10"
                                    font-family="ui-monospace, SFMono-Regular, Menlo, monospace"
                                >
                                    {format!("{val:+.0}%")}
                                </text>
                            </g>
                        }
                    }).collect_view()}

                    // Baseline 0% reference line
                    <line
                        x1=format!("{pad_l:.1}")
                        y1=format!("{zero_y:.1}")
                        x2=format!("{:.1}", width - pad_r)
                        y2=format!("{zero_y:.1}")
                        stroke="#475569"
                        stroke-width="1.2"
                        stroke-dasharray="5 4"
                    />

                    // Vertical gridlines & X-axis labels
                    {x_ticks.into_iter().map(|val| {
                        let x = x_pos(val);
                        view! {
                            <g>
                                <line
                                    x1=format!("{x:.1}")
                                    y1=format!("{pad_t:.1}")
                                    x2=format!("{x:.1}")
                                    y2=format!("{:.1}", height - pad_b)
                                    stroke="#1e293b"
                                    stroke-width="1"
                                    stroke-dasharray="3 4"
                                />
                                <text
                                    x=format!("{x:.1}")
                                    y=format!("{:.1}", height - pad_b + 16.0)
                                    text-anchor="middle"
                                    fill="#64748b"
                                    font-size="10"
                                    font-family="ui-monospace, SFMono-Regular, Menlo, monospace"
                                >
                                    {format!("{val:.0}")}
                                </text>
                            </g>
                        }
                    }).collect_view()}

                    // Axes borders & titles
                    <line x1=format!("{pad_l:.1}") y1=format!("{pad_t:.1}") x2=format!("{pad_l:.1}") y2=format!("{:.1}", height - pad_b) stroke="#334155" stroke-width="1.4" />
                    <line x1=format!("{pad_l:.1}") y1=format!("{:.1}", height - pad_b) x2=format!("{:.1}", width - pad_r) y2=format!("{:.1}", height - pad_b) stroke="#334155" stroke-width="1.4" />

                    <text
                        x=format!("{:.1}", pad_l + plot_w * 0.5)
                        y=format!("{:.1}", height - 8.0)
                        text-anchor="middle"
                        fill="#94a3b8"
                        font-size="10.5"
                        font-weight="600"
                    >
                        "Composite Impact Risk (0 = Safest · 100 = High Blast Risk)"
                    </text>
                    <text
                        x="15"
                        y=format!("{:.1}", pad_t + plot_h * 0.5)
                        text-anchor="middle"
                        fill="#94a3b8"
                        font-size="10.5"
                        font-weight="600"
                        transform=format!("rotate(-90 15 {:.1})", pad_t + plot_h * 0.5)
                    >
                        "Speedup %"
                    </text>

                    // Dashed Pareto frontier polyline
                    {(!frontier_pts.is_empty() && frontier_pts.len() >= 2).then(|| view! {
                        <polyline
                            points=frontier_polyline
                            fill="none"
                            stroke="#38bdf8"
                            stroke-width="2.2"
                            stroke-dasharray="6 4"
                            opacity="0.88"
                        />
                    })}

                    // Clickable Candidate Bubbles
                    {plottable.into_iter().map(|cand| {
                        let cid = cand.strategy_id.clone();
                        let cid_click = cid.clone();
                        let cid_enter = cid.clone();
                        let cid_leave = cid.clone();

                        let risk = cand
                            .blast_radius
                            .as_ref()
                            .map(|b| b.composite_risk_score as f64)
                            .unwrap_or(50.0);
                        let depth = cand
                            .blast_radius
                            .as_ref()
                            .map(|b| b.root_cause_depth_score as f64)
                            .unwrap_or(50.0);
                        let speedup = cand
                            .arena_metrics
                            .as_ref()
                            .map(|m| m.speedup_pct)
                            .unwrap_or(0.0);

                        let cx = x_pos(risk);
                        let cy = y_pos(speedup);
                        // Bubble radius encodes Root-Cause Depth (0..100)
                        let r = 8.0 + (depth.clamp(0.0, 100.0) / 100.0) * 13.0;
                        let is_selected = selected_id.as_deref() == Some(cid.as_str());
                        let is_best = cand.badges.contains(&CandidateBadge::BestOverall);
                        let is_green = cand.is_green();

                        let (fill, stroke) = if !is_green {
                            ("rgba(239, 68, 68, 0.25)", "#ef4444")
                        } else if is_best {
                            ("rgba(16, 185, 129, 0.42)", "#fbbf24")
                        } else if cand.pareto_optimal {
                            ("rgba(56, 189, 248, 0.34)", "#38bdf8")
                        } else {
                            ("rgba(148, 163, 184, 0.24)", "#94a3b8")
                        };

                        // Radial depth arc circumference around bubble
                        let ring_r = r + 3.5;
                        let circum = 2.0 * std::f64::consts::PI * ring_r;
                        let dash_len = circum * (depth.clamp(0.0, 100.0) / 100.0);
                        let label = short_label(&cid, 16);

                        let depth_ring_color = if depth >= 70.0 {
                            "#10b981"
                        } else if depth >= 45.0 {
                            "#a78bfa"
                        } else {
                            "#f59e0b"
                        };
                        let bubble_stroke_w = if is_selected { "2.6" } else { "1.8" };

                        view! {
                            <g
                                style="cursor: pointer;"
                                on:click=move |_| selected_strategy_id.set(Some(cid_click.clone()))
                                on:mouseenter=move |_| hovered_scatter_id.set(Some(cid_enter.clone()))
                                on:mouseleave=move |_| {
                                    if hovered_scatter_id.get_untracked().as_deref() == Some(cid_leave.as_str()) {
                                        hovered_scatter_id.set(None);
                                    }
                                }
                            >
                                // Selection outer halo
                                {is_selected.then(|| view! {
                                    <circle
                                        cx=format!("{cx:.1}")
                                        cy=format!("{cy:.1}")
                                        r=format!("{:.1}", ring_r + 4.0)
                                        fill="none"
                                        stroke="#f8fafc"
                                        stroke-width="2"
                                    />
                                })}

                                // Root-cause depth proportional ring
                                <circle
                                    cx=format!("{cx:.1}")
                                    cy=format!("{cy:.1}")
                                    r=format!("{ring_r:.1}")
                                    fill="none"
                                    stroke=depth_ring_color
                                    stroke-width="2.2"
                                    stroke-dasharray=format!("{dash_len:.1} {:.1}", circum)
                                    transform=format!("rotate(-90 {cx:.1} {cy:.1})")
                                    opacity="0.88"
                                />

                                // Main candidate bubble
                                <circle
                                    cx=format!("{cx:.1}")
                                    cy=format!("{cy:.1}")
                                    r=format!("{r:.1}")
                                    fill=fill
                                    stroke=stroke
                                    stroke-width=bubble_stroke_w
                                />

                                // Center depth value
                                <text
                                    x=format!("{cx:.1}")
                                    y=format!("{:.1}", cy + 3.2)
                                    text-anchor="middle"
                                    fill="#f8fafc"
                                    font-size="9"
                                    font-weight="700"
                                    font-family="ui-monospace, SFMono-Regular, Menlo, monospace"
                                >
                                    {format!("{depth:.0}")}
                                </text>

                                // Strategy ID label
                                <text
                                    x=format!("{cx:.1}")
                                    y=format!("{:.1}", cy - ring_r - 6.0)
                                    text-anchor="middle"
                                    fill="#e2e8f0"
                                    font-size="10"
                                    font-weight="600"
                                    font-family="ui-monospace, SFMono-Regular, Menlo, monospace"
                                    paint-order="stroke"
                                    stroke="#080d18"
                                    stroke-width="3"
                                >
                                    {label}
                                </text>
                            </g>
                        }
                    }).collect_view()}
                </svg>
            </div>

            // Interactive Hover / Selected Readout Strip
            <div class="epi-scatter-hover-bar">
                {move || {
                    let focus = hovered_scatter_id
                        .get()
                        .or_else(|| selected_id_hover.clone());
                    let cand = focus
                        .as_deref()
                        .and_then(|id| candidates_for_hover.iter().find(|c| c.strategy_id == id));
                    match cand {
                        Some(c) => {
                            let title = specs_for_hover
                                .iter()
                                .find(|s| s.id == c.strategy_id)
                                .map(|s| s.title.clone())
                                .unwrap_or_else(|| c.strategy_id.clone());
                            let risk = c.blast_radius.as_ref().map(|b| b.composite_risk_score).unwrap_or(50.0);
                            let depth = c.blast_radius.as_ref().map(|b| b.root_cause_depth_score).unwrap_or(50.0);
                            let sp = c.arena_metrics.as_ref().map(|m| m.speedup_pct).unwrap_or(0.0);
                            let util = c.dual_utility_score();
                            view! {
                                <span style="color:#38bdf8; font-weight:700;">{format!("{title} ({})", c.strategy_id)}</span>
                                <span>{format!("Risk {:.1} · Depth {:.0} · Speedup {:+.1}% · Dual Utility {:.1}", risk, depth, sp, util)}</span>
                            }.into_any()
                        }
                        None => view! {
                            <span>"Hover or click any Pareto bubble to inspect its Dual-Score breakdown."</span>
                        }.into_any(),
                    }
                }}
            </div>
        </section>
    }.into_any()
}

fn render_arena_panel(
    selected_candidate: Option<CandidateResult>,
    all_candidates: Vec<CandidateResult>,
    arena_summary: Option<epicenter_common::ArenaSummary>,
    hovered_trial_idx: RwSignal<Option<usize>>,
) -> impl IntoView {
    let Some(cand) = selected_candidate else {
        return view! { <div /> }.into_any();
    };
    let Some(metrics) = cand.arena_metrics.clone() else {
        return view! {
            <section class="epi-panel-card">
                <div class="epi-panel-head">
                    <h2 class="epi-panel-title">"Interleaved Benchmark Arena"</h2>
                    <span class="epi-panel-sub">{cand.strategy_id}</span>
                </div>
                <div class="epi-empty-box">
                    "No Arena benchmark samples recorded for this candidate yet."
                </div>
            </section>
        }.into_any();
    };

    let bench_name = arena_summary
        .as_ref()
        .map(|s| s.benchmark_name.clone())
        .unwrap_or_else(|| "Interleaved Hot-Path Benchmark".to_string());
    let baseline_p50 = arena_summary
        .as_ref()
        .map(|s| s.baseline_p50_ms)
        .filter(|&v| v > 0.0)
        .unwrap_or_else(|| percentile_ms(&metrics.baseline_samples_ms, 50.0));
    let baseline_p95 = arena_summary
        .as_ref()
        .map(|s| s.baseline_p95_ms)
        .filter(|&v| v > 0.0)
        .unwrap_or_else(|| percentile_ms(&metrics.baseline_samples_ms, 95.0));

    let max_bar_ms = [baseline_p50, baseline_p95, metrics.p50_ms, metrics.p95_ms]
        .into_iter()
        .fold(1.0_f64, f64::max);

    let bar_rows = vec![
        ("Baseline p50", baseline_p50, "#64748b"),
        ("Candidate p50", metrics.p50_ms, "#10b981"),
        ("Baseline p95", baseline_p95, "#475569"),
        ("Candidate p95", metrics.p95_ms, "#38bdf8"),
    ];

    // Interleaved trial-by-trial sparkline data
    let n_trials = metrics
        .baseline_samples_ms
        .len()
        .max(metrics.candidate_samples_ms.len());
    let sample_min = metrics
        .baseline_samples_ms
        .iter()
        .chain(metrics.candidate_samples_ms.iter())
        .copied()
        .fold(f64::INFINITY, f64::min);
    let sample_max = metrics
        .baseline_samples_ms
        .iter()
        .chain(metrics.candidate_samples_ms.iter())
        .copied()
        .fold(0.0_f64, f64::max);
    let (s_lo, s_hi) = if sample_min.is_finite() && sample_max > sample_min {
        let pad = (sample_max - sample_min) * 0.15;
        ((sample_min - pad).max(0.0), sample_max + pad)
    } else {
        (0.0, sample_max.max(10.0) * 1.2)
    };
    let s_span = (s_hi - s_lo).max(1.0);

    let sp_w = 640.0_f64;
    let sp_h = 185.0_f64;
    let sp_pl = 54.0_f64;
    let sp_pr = 20.0_f64;
    let sp_pt = 20.0_f64;
    let sp_pb = 32.0_f64;
    let sp_pw = sp_w - sp_pl - sp_pr;
    let sp_ph = sp_h - sp_pt - sp_pb;

    let trial_x = move |i: usize| {
        if n_trials <= 1 {
            sp_pl + sp_pw * 0.5
        } else {
            sp_pl + (i as f64 / (n_trials - 1) as f64) * sp_pw
        }
    };
    let trial_y = move |val: f64| sp_pt + ((s_hi - val.clamp(s_lo, s_hi)) / s_span) * sp_ph;

    let base_poly = metrics
        .baseline_samples_ms
        .iter()
        .enumerate()
        .map(|(i, &v)| format!("{:.1},{:.1}", trial_x(i), trial_y(v)))
        .collect::<Vec<_>>()
        .join(" ");
    let cand_poly = metrics
        .candidate_samples_ms
        .iter()
        .enumerate()
        .map(|(i, &v)| format!("{:.1},{:.1}", trial_x(i), trial_y(v)))
        .collect::<Vec<_>>()
        .join(" ");

    let base_samples_for_readout = metrics.baseline_samples_ms.clone();
    let cand_samples_for_readout = metrics.candidate_samples_ms.clone();

    let speed_color = if metrics.speedup_pct > 2.0 {
        "#10b981"
    } else if metrics.speedup_pct < -2.0 {
        "#ef4444"
    } else {
        "#38bdf8"
    };

    let bench_count = all_candidates
        .iter()
        .filter(|c| c.arena_metrics.is_some())
        .count();

    view! {
        <section class="epi-panel-card">
            <div class="epi-panel-head">
                <div>
                    <h2 class="epi-panel-title">"Single-VM Interleaved Benchmark Arena"</h2>
                    <div class="epi-panel-sub">
                        {format!("{} · {} trials · {bench_count} benchmarked candidates", bench_name, n_trials)}
                    </div>
                </div>
                <span class="epi-tag epi-tag-cyan">{cand.strategy_id.clone()}</span>
            </div>

            // Telemetry cards
            <div class="epi-arena-telemetry">
                <div class="epi-tele-card">
                    <span class="epi-tele-label">"p50 Speedup"</span>
                    <span class="epi-tele-val" style=format!("color:{speed_color};")>
                        {format!("{:+.1}%", metrics.speedup_pct)}
                    </span>
                    <span class="epi-tele-sub">{format!("vs {:.2} ms base", baseline_p50)}</span>
                </div>
                <div class="epi-tele-card">
                    <span class="epi-tele-label">"Candidate p50"</span>
                    <span class="epi-tele-val" style="color:#10b981;">{format!("{:.2} ms", metrics.p50_ms)}</span>
                    <span class="epi-tele-sub">{format!("p95: {:.2} ms", metrics.p95_ms)}</span>
                </div>
                <div class="epi-tele-card">
                    <span class="epi-tele-label">"Peak Memory"</span>
                    <span class="epi-tele-val">{format_memory(metrics.peak_memory_kb)}</span>
                    <span class="epi-tele-sub">"RSS high-water"</span>
                </div>
                <div class="epi-tele-card">
                    <span class="epi-tele-label">"Calls / Queries"</span>
                    <span class="epi-tele-val">
                        {metrics
                            .call_or_query_count
                            .map(|c| c.to_string())
                            .unwrap_or_else(|| "—".into())}
                    </span>
                    <span class="epi-tele-sub">"Hot-path ops"</span>
                </div>
            </div>

            // Horizontal Baseline vs Candidate p50 / p95 Bar Chart (<svg>)
            <div class="epi-svg-box">
                <svg class="epi-svg-fluid" viewBox="0 0 640 132">
                    {bar_rows.into_iter().enumerate().map(|(idx, (label, val, color))| {
                        let y = 14.0 + (idx as f64) * 29.0;
                        let bar_max_w = 400.0_f64;
                        let bar_w = ((val / max_bar_ms) * bar_max_w).clamp(3.0, bar_max_w);
                        view! {
                            <g>
                                <text
                                    x="110"
                                    y=format!("{:.1}", y + 13.0)
                                    text-anchor="end"
                                    fill="#cbd5e1"
                                    font-size="11"
                                    font-weight="600"
                                >
                                    {label}
                                </text>
                                <rect
                                    x="122"
                                    y=format!("{y:.1}")
                                    width=format!("{bar_max_w:.1}")
                                    height="18"
                                    rx="5"
                                    fill="#0f172a"
                                />
                                <rect
                                    x="122"
                                    y=format!("{y:.1}")
                                    width=format!("{bar_w:.1}")
                                    height="18"
                                    rx="5"
                                    fill=color
                                />
                                <text
                                    x=format!("{:.1}", 122.0 + bar_w + 8.0)
                                    y=format!("{:.1}", y + 13.0)
                                    fill="#f8fafc"
                                    font-size="11"
                                    font-weight="700"
                                    font-family="ui-monospace, SFMono-Regular, Menlo, monospace"
                                >
                                    {format!("{val:.2} ms")}
                                </text>
                            </g>
                        }
                    }).collect_view()}
                </svg>
            </div>

            // Interleaved Trial-by-Trial Polyline Sparkline
            {(n_trials > 0).then(|| {
                let y_mid = (s_lo + s_hi) * 0.5;
                let base_pts_vec = metrics.baseline_samples_ms.clone();
                let cand_pts_vec = metrics.candidate_samples_ms.clone();

                view! {
                    <div class="epi-svg-box">
                        <div style="display:flex; justify-content:space-between; align-items:center; margin-bottom:6px; font-size:11px; color:#94a3b8;">
                            <span style="font-weight:700; color:#e2e8f0;">"Interleaved Trial-by-Trial Latency (ms)"</span>
                            <div style="display:flex; gap:12px;">
                                <span style="color:#f59e0b;">"--- Baseline"</span>
                                <span style="color:#10b981;">"━━ Candidate"</span>
                            </div>
                        </div>
                        <svg class="epi-svg-fluid" viewBox="0 0 640 185">
                            // Gridlines
                            {[s_hi, y_mid, s_lo].into_iter().map(|v| {
                                let y = trial_y(v);
                                view! {
                                    <g>
                                        <line
                                            x1=format!("{sp_pl:.1}")
                                            y1=format!("{y:.1}")
                                            x2=format!("{:.1}", sp_w - sp_pr)
                                            y2=format!("{y:.1}")
                                            stroke="#1e293b"
                                            stroke-width="1"
                                            stroke-dasharray="3 4"
                                        />
                                        <text
                                            x=format!("{:.1}", sp_pl - 7.0)
                                            y=format!("{:.1}", y + 3.5)
                                            text-anchor="end"
                                            fill="#64748b"
                                            font-size="9.5"
                                            font-family="ui-monospace, SFMono-Regular, Menlo, monospace"
                                        >
                                            {format!("{v:.1}")}
                                        </text>
                                    </g>
                                }
                            }).collect_view()}

                            // Baseline polyline (amber dashed)
                            {(!base_poly.is_empty()).then(|| view! {
                                <polyline
                                    points=base_poly
                                    fill="none"
                                    stroke="#f59e0b"
                                    stroke-width="2"
                                    stroke-dasharray="5 3"
                                    opacity="0.88"
                                />
                            })}

                            // Candidate polyline (emerald solid)
                            {(!cand_poly.is_empty()).then(|| view! {
                                <polyline
                                    points=cand_poly
                                    fill="none"
                                    stroke="#10b981"
                                    stroke-width="2.4"
                                />
                            })}

                            // Trial dots & hover targets
                            {(0..n_trials).map(|i| {
                                let x = trial_x(i);
                                let b_val = base_pts_vec.get(i).copied();
                                let c_val = cand_pts_vec.get(i).copied();
                                let show_tick = n_trials <= 12 || i == 0 || i + 1 == n_trials || i % 2 == 0;
                                view! {
                                    <g
                                        style="cursor: crosshair;"
                                        on:mouseenter=move |_| hovered_trial_idx.set(Some(i))
                                        on:mouseleave=move |_| {
                                            if hovered_trial_idx.get_untracked() == Some(i) {
                                                hovered_trial_idx.set(None);
                                            }
                                        }
                                    >
                                        <rect
                                            x=format!("{:.1}", x - 12.0)
                                            y=format!("{sp_pt:.1}")
                                            width="24"
                                            height=format!("{sp_ph:.1}")
                                            fill="transparent"
                                        />
                                        {b_val.map(|v| view! {
                                            <circle cx=format!("{x:.1}") cy=format!("{:.1}", trial_y(v)) r="3.5" fill="#f59e0b" stroke="#080d18" stroke-width="1" />
                                        })}
                                        {c_val.map(|v| view! {
                                            <circle cx=format!("{x:.1}") cy=format!("{:.1}", trial_y(v)) r="4.0" fill="#10b981" stroke="#080d18" stroke-width="1.2" />
                                        })}
                                        {show_tick.then(|| view! {
                                            <text
                                                x=format!("{x:.1}")
                                                y=format!("{:.1}", sp_h - 10.0)
                                                text-anchor="middle"
                                                fill="#64748b"
                                                font-size="9.5"
                                                font-family="ui-monospace, SFMono-Regular, Menlo, monospace"
                                            >
                                                {format!("T{}", i + 1)}
                                            </text>
                                        })}
                                    </g>
                                }
                            }).collect_view()}
                        </svg>
                    </div>

                    <div class="epi-scatter-hover-bar">
                        {move || match hovered_trial_idx.get() {
                            Some(idx) => {
                                let b = base_samples_for_readout.get(idx).copied();
                                let c = cand_samples_for_readout.get(idx).copied();
                                let delta = match (b, c) {
                                    (Some(bv), Some(cv)) if bv > 0.0 => {
                                        format!("Δ {:+.1}%", ((bv - cv) / bv) * 100.0)
                                    }
                                    _ => "—".to_string(),
                                };
                                view! {
                                    <span style="color:#38bdf8; font-weight:700;">{format!("Interleaved Trial #{}", idx + 1)}</span>
                                    <span>{format!(
                                        "Baseline: {} · Candidate: {} · {}",
                                        b.map(|v| format!("{v:.2} ms")).unwrap_or_else(|| "—".into()),
                                        c.map(|v| format!("{v:.2} ms")).unwrap_or_else(|| "—".into()),
                                        delta
                                    )}</span>
                                }.into_any()
                            }
                            None => {
                                let notes = arena_summary
                                    .as_ref()
                                    .map(|s| s.summary_notes.clone())
                                    .filter(|s| !s.is_empty())
                                    .unwrap_or_else(|| "Hover any trial point on the interleaved sparkline to inspect paired VM timings.".into());
                                view! { <span>{notes}</span> }.into_any()
                            }
                        }}
                    </div>
                }
            })}
        </section>
    }.into_any()
}
