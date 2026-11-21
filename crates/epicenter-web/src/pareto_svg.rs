// Owned by `neon-web-viz`: `<svg>` Speedup-vs-Risk Pareto scatter chart + Arena p50/p95 bars + 3 Crowned Badges.
use epicenter_common::{
    percentile_ms, CandidateBadge, CandidateResult, CandidateStatus, StrategyDepthTier,
    StrategySpec, TournamentSession,
};
use leptos::prelude::*;

fn tier_info(tier: StrategyDepthTier) -> (&'static str, &'static str) {
    match tier {
        StrategyDepthTier::SurgicalHotfix => ("Quick Fix", "epi-tag epi-tag-amber"),
        StrategyDepthTier::TargetedRefactor => ("Targeted Refactor", "epi-tag epi-tag-cyan"),
        StrategyDepthTier::ArchitecturalRootCause => ("Root-Cause Fix", "epi-tag epi-tag-violet"),
        StrategyDepthTier::AlgorithmicOptimization => ("Perf Optimization", "epi-tag epi-tag-cyan"),
    }
}

fn badge_style(badge: CandidateBadge) -> (&'static str, &'static str) {
    match badge {
        CandidateBadge::BestOverall => ("★ Best Overall", "epi-tag epi-tag-gold"),
        CandidateBadge::SafestEmergencyHotfix => ("◆ Safest Fix", "epi-tag epi-tag-cyan"),
        CandidateBadge::FastestThroughput => ("⚡ Fastest", "epi-tag epi-tag-emerald"),
    }
}

fn status_badge(cand: &CandidateResult) -> (&'static str, &'static str) {
    match cand.status {
        CandidateStatus::Pending => ("Pending", "epi-tag"),
        CandidateStatus::Running => ("Running", "epi-tag epi-tag-cyan"),
        CandidateStatus::FailedTests => ("Tests Failed", "epi-tag epi-tag-red"),
        CandidateStatus::PassedTests => {
            if cand.is_green() {
                ("Tests Passed", "epi-tag epi-tag-emerald")
            } else {
                ("Partial Pass", "epi-tag epi-tag-amber")
            }
        }
        CandidateStatus::BenchmarkDone => ("Benchmarked", "epi-tag epi-tag-cyan"),
        CandidateStatus::Audited => {
            if cand.is_green() {
                ("Verified", "epi-tag epi-tag-emerald")
            } else {
                ("Failed", "epi-tag epi-tag-red")
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PerfSubTab {
    TradeOff,
    Benchmark,
}

#[component]
pub fn CandidateApproachesStrip(
    session: Signal<Option<TournamentSession>>,
    selected_strategy_id: RwSignal<Option<String>>,
) -> impl IntoView {
    view! {
        {move || {
            let Some(sess) = session.get() else {
                return view! { <div></div> }.into_any();
            };

            let selected_id = active_candidate_id(&sess, selected_strategy_id.get().as_deref());
            let specs: Vec<StrategySpec> = sess
                .strategy_plan
                .as_ref()
                .map(|p| p.strategies.clone())
                .unwrap_or_default();
            let candidates = sess.candidates.clone();

            view! {
                <section class="epi-panel-card" aria-label="Candidate Approaches">
                    <div class="epi-panel-head">
                        <div>
                            <h2 class="epi-panel-title">
                                <span class="model-dot dot-amber" aria-hidden="true"></span>
                                {format!("Candidate Approaches ({})", candidates.len())}
                            </h2>
                            <div class="epi-panel-sub">
                                {sess
                                    .strategy_plan
                                    .as_ref()
                                    .map(|p| p.degree_of_freedom_rationale.clone())
                                    .filter(|s| !s.trim().is_empty())
                                    .unwrap_or_else(|| "Select an approach to inspect its code impact, performance, and diff below.".to_string())}
                            </div>
                        </div>
                    </div>

                    {if candidates.is_empty() {
                        view! {
                            <div class="epi-empty-box">
                                "Planning and building candidate approaches..."
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
                                        Some(r) if r <= 25.0 => "#6ee7b7",
                                        Some(r) if r <= 55.0 => "#fcd34d",
                                        Some(_) => "#fca5a5",
                                        None => "#8b9db5",
                                    };
                                    let depth_color = match depth {
                                        Some(d) if d >= 70.0 => "#6ee7b7",
                                        Some(d) if d >= 45.0 => "#7dd3fc",
                                        Some(_) => "#fcd34d",
                                        None => "#8b9db5",
                                    };
                                    let speed_color = match speedup {
                                        Some(sp) if sp > 2.0 => "#6ee7b7",
                                        Some(sp) if sp < -2.0 => "#fca5a5",
                                        Some(_) => "#e2e8f0",
                                        None => "#8b9db5",
                                    };

                                    let badges = cand.badges.clone();

                                    view! {
                                        <button
                                            type="button"
                                            class=card_cls
                                            aria-pressed=is_sel.to_string()
                                            on:click=move |_| selected_strategy_id.set(Some(id_for_click.clone()))
                                        >
                                            <div class="epi-cand-top">
                                                <div class="epi-cand-name">{title}</div>
                                                <span class=st_cls>{st_text}</span>
                                            </div>

                                            <div class="epi-tag-row">
                                                <span class=tier_cls>{tier_text}</span>
                                                {badges.into_iter().map(|b| {
                                                    let (b_text, b_cls) = badge_style(b);
                                                    view! { <span class=b_cls>{b_text}</span> }
                                                }).collect_view()}
                                                {cand.pareto_optimal.then(|| view! {
                                                    <span class="epi-tag epi-tag-cyan">"Top Trade-Off"</span>
                                                })}
                                                {cand.added_caller_tests.then(|| view! {
                                                    <span class="epi-tag epi-tag-emerald">"+ Caller Tests"</span>
                                                })}
                                                {cand.is_rescue_candidate.then(|| view! {
                                                    <span class="epi-tag epi-tag-violet">"Escalated Fix"</span>
                                                })}
                                                {has_bandaid.then(|| view! {
                                                    <span class="epi-tag epi-tag-amber">"Shallow Fix"</span>
                                                })}
                                            </div>

                                            <div class="epi-cand-metrics">
                                                <div class="epi-mini-stat">
                                                    <span class="epi-mini-label">"Impact Risk"</span>
                                                    <span class="epi-mini-val" style=format!("color:{risk_color};")>
                                                        {risk.map(|v| format!("{v:.0}/100")).unwrap_or_else(|| "—".into())}
                                                    </span>
                                                </div>
                                                <div class="epi-mini-stat">
                                                    <span class="epi-mini-label">"Root-Cause"</span>
                                                    <span class="epi-mini-val" style=format!("color:{depth_color};")>
                                                        {depth.map(|v| format!("{v:.0}/100")).unwrap_or_else(|| "—".into())}
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
            }.into_any()
        }}
    }
}

#[component]
pub fn ParetoAndArenaPanel(
    session: Signal<Option<TournamentSession>>,
    selected_strategy_id: RwSignal<Option<String>>,
) -> impl IntoView {
    let hovered_scatter_id = RwSignal::new(Option::<String>::None);
    let hovered_trial_idx = RwSignal::new(Option::<usize>::None);
    let active_subtab = RwSignal::new(PerfSubTab::TradeOff);

    view! {
        <section class="epi-panel-card" aria-label="Trade-Off and Performance">
            {move || {
                let Some(sess) = session.get() else {
                    return view! {
                        <div class="epi-panel-head">
                            <div>
                                <h2 class="epi-panel-title">
                                    <span class="model-dot dot-amber" aria-hidden="true"></span>
                                    "Trade-Off & Performance"
                                </h2>
                                <div class="epi-panel-sub">"Compare impact risk, fix depth, and runtime speed across approaches."</div>
                            </div>
                        </div>
                        <div class="epi-empty-box">
                            "Launch or select a comparison to view the risk-vs-speed trade-off and latency benchmarks."
                        </div>
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

                let subtab = active_subtab.get();
                let sub_desc = match subtab {
                    PerfSubTab::TradeOff => "Lower Impact Risk (left) and higher Speedup (up) are better. Larger bubbles indicate deeper root-cause fixes.",
                    PerfSubTab::Benchmark => "Baseline vs. candidate latency measured across interleaved benchmark trials.",
                };
                let active_tab_id = match subtab {
                    PerfSubTab::TradeOff => "perf-tab-tradeoff",
                    PerfSubTab::Benchmark => "perf-tab-benchmark",
                };

                view! {
                    <div class="epi-panel-head">
                        <div>
                            <h2 class="epi-panel-title">
                                <span class="model-dot dot-amber" aria-hidden="true"></span>
                                "Trade-Off & Performance"
                            </h2>
                            <div class="epi-panel-sub">{sub_desc}</div>
                        </div>
                        <div class="epi-subtab-bar" role="tablist" aria-label="Performance view">
                            <button
                                id="perf-tab-tradeoff"
                                type="button"
                                role="tab"
                                aria-selected=(subtab == PerfSubTab::TradeOff).to_string()
                                aria-controls="perf-tabpanel"
                                class=if subtab == PerfSubTab::TradeOff { "epi-subtab-btn active" } else { "epi-subtab-btn" }
                                on:click=move |_| active_subtab.set(PerfSubTab::TradeOff)
                            >
                                "Risk vs. Speed"
                            </button>
                            <button
                                id="perf-tab-benchmark"
                                type="button"
                                role="tab"
                                aria-selected=(subtab == PerfSubTab::Benchmark).to_string()
                                aria-controls="perf-tabpanel"
                                class=if subtab == PerfSubTab::Benchmark { "epi-subtab-btn active" } else { "epi-subtab-btn" }
                                on:click=move |_| active_subtab.set(PerfSubTab::Benchmark)
                            >
                                "Latency Benchmark"
                            </button>
                        </div>
                    </div>

                    <div id="perf-tabpanel" role="tabpanel" aria-labelledby=active_tab_id>
                        {match subtab {
                            PerfSubTab::TradeOff => render_pareto_scatter(
                                candidates,
                                specs,
                                selected_id,
                                selected_strategy_id,
                                hovered_scatter_id,
                            ).into_any(),
                            PerfSubTab::Benchmark => render_arena_panel(
                                selected_candidate,
                                specs,
                                candidates,
                                arena_summary,
                                hovered_trial_idx,
                            ).into_any(),
                        }}
                    </div>
                }.into_any()
            }}
        </section>
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
            <div class="epi-empty-box">
                "Waiting for benchmarks and risk audit to plot the risk-vs-speed trade-off..."
            </div>
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
        <div class="epi-pareto-wrap">
            <div class="epi-svg-box">
                <svg
                    class="epi-svg-fluid"
                    role="group"
                    aria-label="Risk versus Speed trade-off scatter chart"
                    viewBox="0 0 640 310"
                >
                    <title>"Risk versus Speed trade-off scatter chart comparing candidate approaches"</title>
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
                        fill="#6ee7b7"
                        font-size="9.5"
                        font-weight="700"
                        letter-spacing="0.8"
                    >
                        "SWEET SPOT · LOWER RISK & FASTER"
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
                                    fill="#8b9db5"
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
                        stroke="#64748b"
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
                                    fill="#8b9db5"
                                    font-size="10"
                                    font-family="ui-monospace, SFMono-Regular, Menlo, monospace"
                                >
                                    {format!("{val:.0}")}
                                </text>
                            </g>
                        }
                    }).collect_view()}

                    // Axes borders & titles
                    <line x1=format!("{pad_l:.1}") y1=format!("{pad_t:.1}") x2=format!("{pad_l:.1}") y2=format!("{:.1}", height - pad_b) stroke="#64748b" stroke-width="1.4" />
                    <line x1=format!("{pad_l:.1}") y1=format!("{:.1}", height - pad_b) x2=format!("{:.1}", width - pad_r) y2=format!("{:.1}", height - pad_b) stroke="#64748b" stroke-width="1.4" />

                    <text
                        x=format!("{:.1}", pad_l + plot_w * 0.5)
                        y=format!("{:.1}", height - 8.0)
                        text-anchor="middle"
                        fill="#a8b7cc"
                        font-size="10.5"
                        font-weight="600"
                    >
                        "Impact Risk (0 = Safest · 100 = Highest Risk)"
                    </text>
                    <text
                        x="15"
                        y=format!("{:.1}", pad_t + plot_h * 0.5)
                        text-anchor="middle"
                        fill="#a8b7cc"
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
                        let title_for_label = specs
                            .iter()
                            .find(|s| s.id == cid)
                            .map(|s| s.title.as_str())
                            .unwrap_or(cid.as_str());
                        let label = short_label(title_for_label, 18);

                        let depth_ring_color = if depth >= 70.0 {
                            "#10b981"
                        } else if depth >= 45.0 {
                            "#a78bfa"
                        } else {
                            "#f59e0b"
                        };
                        let bubble_stroke_w = if is_selected { "2.6" } else { "1.8" };

                        let cid_key = cid.clone();
                        let cid_focus = cid.clone();
                        let cid_blur = cid.clone();
                        let cid_ring = cid.clone();
                        let bubble_aria = format!(
                            "{title_for_label}: Impact Risk {risk:.0} of 100, Root-Cause Depth {depth:.0} of 100, Speedup {speedup:+.1}%"
                        );

                        view! {
                            <g
                                class="epi-node-group"
                                role="button"
                                tabindex="0"
                                aria-label=bubble_aria
                                aria-pressed=is_selected.to_string()
                                style="cursor: pointer;"
                                on:click=move |_| selected_strategy_id.set(Some(cid_click.clone()))
                                on:keydown=move |ev: web_sys::KeyboardEvent| {
                                    let key = ev.key();
                                    if key == "Enter" || key == " " {
                                        ev.prevent_default();
                                        selected_strategy_id.set(Some(cid_key.clone()));
                                    }
                                }
                                on:mouseenter=move |_| hovered_scatter_id.set(Some(cid_enter.clone()))
                                on:mouseleave=move |_| {
                                    if hovered_scatter_id.get_untracked().as_deref() == Some(cid_leave.as_str()) {
                                        hovered_scatter_id.set(None);
                                    }
                                }
                                on:focus=move |_| hovered_scatter_id.set(Some(cid_focus.clone()))
                                on:blur=move |_| {
                                    if hovered_scatter_id.get_untracked().as_deref() == Some(cid_blur.as_str()) {
                                        hovered_scatter_id.set(None);
                                    }
                                }
                            >
                                // Keyboard focus / active hover ring
                                {move || (hovered_scatter_id.get().as_deref() == Some(cid_ring.as_str())).then(|| view! {
                                    <circle
                                        cx=format!("{cx:.1}")
                                        cy=format!("{cy:.1}")
                                        r=format!("{:.1}", ring_r + 6.5)
                                        fill="none"
                                        stroke="#7dd3fc"
                                        stroke-width="2.5"
                                        stroke-dasharray="4 3"
                                    />
                                })}

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
            <div class="epi-scatter-hover-bar" role="status" aria-live="polite">
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
                                <span style="color:#38bdf8; font-weight:700;">{title}</span>
                                <span>{format!("Risk {:.0}/100 · Root-Cause {:.0}/100 · Speedup {:+.1}% · Score {:.1}", risk, depth, sp, util)}</span>
                            }.into_any()
                        }
                        None => view! {
                            <span>"Hover, focus, or select any point to inspect its breakdown."</span>
                        }.into_any(),
                    }
                }}
            </div>
        </div>
    }.into_any()
}

fn render_arena_panel(
    selected_candidate: Option<CandidateResult>,
    specs: Vec<StrategySpec>,
    all_candidates: Vec<CandidateResult>,
    arena_summary: Option<epicenter_common::ArenaSummary>,
    hovered_trial_idx: RwSignal<Option<usize>>,
) -> impl IntoView {
    let Some(cand) = selected_candidate else {
        return view! {
            <div class="epi-empty-box">
                "Select a candidate approach above to inspect its latency benchmark."
            </div>
        }.into_any();
    };
    let cand_title = specs
        .iter()
        .find(|s| s.id == cand.strategy_id)
        .map(|s| s.title.clone())
        .unwrap_or_else(|| cand.strategy_id.clone());
    let Some(metrics) = cand.arena_metrics.clone() else {
        return view! {
            <div class="epi-empty-box">
                {format!("No benchmark samples recorded for \"{cand_title}\" yet.")}
            </div>
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
        ("Baseline p50", baseline_p50, "#94a3b8"),
        ("Candidate p50", metrics.p50_ms, "#10b981"),
        ("Baseline p95", baseline_p95, "#64748b"),
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

    let bar_chart_aria = format!(
        "Latency comparison for {cand_title}: Baseline median {baseline_p50:.2} ms, Candidate median {:.2} ms, Baseline p95 {baseline_p95:.2} ms, Candidate p95 {:.2} ms",
        metrics.p50_ms, metrics.p95_ms
    );
    let bar_chart_title = bar_chart_aria.clone();

    view! {
        <div class="epi-pareto-wrap">
            <div style="display:flex; justify-content:space-between; align-items:center; gap:8px; flex-wrap:wrap; font-size:12px; color:#a8b7cc;">
                <span>{format!("{bench_name} · {n_trials} trials · {bench_count} benchmarked")}</span>
                <span class="epi-tag epi-tag-cyan">{short_label(&cand_title, 28)}</span>
            </div>

            // Key benchmark stats
            <div class="epi-arena-telemetry">
                <div class="epi-tele-card">
                    <span class="epi-tele-label">"Median Speedup"</span>
                    <span class="epi-tele-val" style=format!("color:{speed_color};")>
                        {format!("{:+.1}%", metrics.speedup_pct)}
                    </span>
                    <span class="epi-tele-sub">{format!("vs {:.2} ms base", baseline_p50)}</span>
                </div>
                <div class="epi-tele-card">
                    <span class="epi-tele-label">"Median Latency"</span>
                    <span class="epi-tele-val" style="color:#10b981;">{format!("{:.2} ms", metrics.p50_ms)}</span>
                    <span class="epi-tele-sub">{format!("p95: {:.2} ms", metrics.p95_ms)}</span>
                </div>
                <div class="epi-tele-card">
                    <span class="epi-tele-label">"Peak Memory"</span>
                    <span class="epi-tele-val">{format_memory(metrics.peak_memory_kb)}</span>
                    <span class="epi-tele-sub">"RSS peak"</span>
                </div>
                <div class="epi-tele-card">
                    <span class="epi-tele-label">"Hot-Path Ops"</span>
                    <span class="epi-tele-val">
                        {metrics
                            .call_or_query_count
                            .map(|c| c.to_string())
                            .unwrap_or_else(|| "—".into())}
                    </span>
                    <span class="epi-tele-sub">"Calls / queries"</span>
                </div>
            </div>

            // Horizontal Baseline vs Candidate p50 / p95 Bar Chart (<svg>)
            <div class="epi-svg-box">
                <svg
                    class="epi-svg-fluid"
                    role="img"
                    aria-label=bar_chart_aria
                    viewBox="0 0 640 132"
                >
                    <title>{bar_chart_title}</title>
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
                        <div style="display:flex; justify-content:space-between; align-items:center; margin-bottom:6px; font-size:11px; color:#a8b7cc;">
                            <span style="font-weight:700; color:#e2e8f0;">"Trial-by-Trial Latency (ms)"</span>
                            <div style="display:flex; gap:12px;">
                                <span style="color:#fcd34d;">"--- Baseline"</span>
                                <span style="color:#6ee7b7;">"━━ Candidate"</span>
                            </div>
                        </div>
                        <svg
                            class="epi-svg-fluid"
                            role="group"
                            aria-label=format!("Trial-by-trial latency chart across {n_trials} trials")
                            viewBox="0 0 640 185"
                        >
                            <title>{format!("Trial-by-trial latency comparison across {n_trials} trials")}</title>
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
                                            fill="#8b9db5"
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

                            // Trial dots & keyboard/hover targets
                            {(0..n_trials).map(|i| {
                                let x = trial_x(i);
                                let b_val = base_pts_vec.get(i).copied();
                                let c_val = cand_pts_vec.get(i).copied();
                                let show_tick = n_trials <= 12 || i == 0 || i + 1 == n_trials || i % 2 == 0;
                                let trial_aria = format!(
                                    "Trial {}: Baseline {}, Candidate {}",
                                    i + 1,
                                    b_val.map(|v| format!("{v:.2} ms")).unwrap_or_else(|| "not recorded".into()),
                                    c_val.map(|v| format!("{v:.2} ms")).unwrap_or_else(|| "not recorded".into()),
                                );
                                view! {
                                    <g
                                        class="epi-node-group"
                                        role="button"
                                        tabindex="0"
                                        aria-label=trial_aria
                                        style="cursor: crosshair;"
                                        on:mouseenter=move |_| hovered_trial_idx.set(Some(i))
                                        on:mouseleave=move |_| {
                                            if hovered_trial_idx.get_untracked() == Some(i) {
                                                hovered_trial_idx.set(None);
                                            }
                                        }
                                        on:focus=move |_| hovered_trial_idx.set(Some(i))
                                        on:blur=move |_| {
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
                                        {move || (hovered_trial_idx.get() == Some(i)).then(|| view! {
                                            <line
                                                x1=format!("{x:.1}")
                                                y1=format!("{sp_pt:.1}")
                                                x2=format!("{x:.1}")
                                                y2=format!("{:.1}", sp_pt + sp_ph)
                                                stroke="#7dd3fc"
                                                stroke-width="2"
                                                stroke-dasharray="3 3"
                                            />
                                        })}
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
                                                fill="#8b9db5"
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

                    <div class="epi-scatter-hover-bar" role="status" aria-live="polite">
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
                                    <span style="color:#38bdf8; font-weight:700;">{format!("Trial #{}", idx + 1)}</span>
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
                                    .unwrap_or_else(|| "Hover or focus any trial point to compare paired baseline and candidate timings.".into());
                                view! { <span>{notes}</span> }.into_any()
                            }
                        }}
                    </div>
                }
            })}
        </div>
    }.into_any()
}
