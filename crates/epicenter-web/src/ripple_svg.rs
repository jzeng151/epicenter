// Owned by `neon-web-viz`: Interactive concentric `<svg>` Ring 0 / Ring 1 / Ring 2 Blast-Radius Ripple Map.
use std::collections::{HashMap, HashSet, VecDeque};

use epicenter_common::{
    BlastRadiusReport, CandidateResult, EdgeRelation, GraphEdge, GraphNode, HazardSeverity,
    ImpactRing, SemanticHazard, StrategyDepthTier, TournamentSession, GEMINI_DECISION_MODEL,
};
use leptos::prelude::*;

const RING0_RADIUS: f32 = 56.0;
const RING1_RADIUS: f32 = 160.0;
const RING2_RADIUS: f32 = 280.0;

fn select_active_candidate<'a>(
    session: &'a TournamentSession,
    selected_id: Option<&str>,
) -> Option<&'a CandidateResult> {
    if let Some(id) = selected_id {
        if let Some(found) = session.candidates.iter().find(|c| c.strategy_id == id) {
            return Some(found);
        }
    }
    if let Some(rec_id) = session
        .arena_summary
        .as_ref()
        .and_then(|s| s.recommended_strategy_id.as_deref())
    {
        if let Some(found) = session.candidates.iter().find(|c| c.strategy_id == rec_id) {
            return Some(found);
        }
    }
    session
        .candidates
        .iter()
        .find(|c| c.blast_radius.is_some())
        .or_else(|| session.candidates.first())
}

fn depth_tier_label(tier: StrategyDepthTier) -> &'static str {
    match tier {
        StrategyDepthTier::SurgicalHotfix => "Quick Fix",
        StrategyDepthTier::TargetedRefactor => "Targeted Refactor",
        StrategyDepthTier::ArchitecturalRootCause => "Root-Cause Fix",
        StrategyDepthTier::AlgorithmicOptimization => "Perf Optimization",
    }
}

fn ring_label(ring: ImpactRing) -> &'static str {
    match ring {
        ImpactRing::Ring0 => "Modified Code",
        ImpactRing::Ring1 => "Direct Caller",
        ImpactRing::Ring2 => "Indirect Entrypoint",
    }
}

fn severity_label(sev: HazardSeverity) -> &'static str {
    match sev {
        HazardSeverity::Critical => "CRITICAL",
        HazardSeverity::High => "HIGH",
        HazardSeverity::Medium => "MEDIUM",
        HazardSeverity::Low => "LOW",
    }
}

fn severity_pill_class(sev: HazardSeverity) -> &'static str {
    match sev {
        HazardSeverity::Critical | HazardSeverity::High => "epi-pill epi-pill-red",
        HazardSeverity::Medium => "epi-pill epi-pill-amber",
        HazardSeverity::Low => "epi-pill epi-pill-cyan",
    }
}

/// Formats `bandaid_probability` (`0.0..=1.0`) as `("Workaround Risk: X%", pill_class)`.
/// Green when `<= 25%`, amber when `26%..=60%`, red when `> 60%`.
pub(crate) fn workaround_risk_pill(prob: f32) -> (String, &'static str) {
    let pct = crate::probability_percent(prob);
    let cls = if pct <= 25.0 {
        "epi-pill epi-pill-emerald"
    } else if pct <= 60.0 {
        "epi-pill epi-pill-amber"
    } else {
        "epi-pill epi-pill-red"
    };
    (format!("Workaround Risk: {pct:.0}%"), cls)
}

/// Compact candidate-card tag variant of [`workaround_risk_pill`].
pub(crate) fn workaround_risk_tag(prob: f32) -> (String, &'static str) {
    let pct = crate::probability_percent(prob);
    let cls = if pct <= 25.0 {
        "epi-tag epi-tag-emerald"
    } else if pct <= 60.0 {
        "epi-tag epi-tag-amber"
    } else {
        "epi-tag epi-tag-red"
    };
    (format!("Workaround Risk: {pct:.0}%"), cls)
}

/// Formats `decision_confidence` (`0.0..=1.0`) as `("Audit Confidence: X%", pill_class)`.
pub(crate) fn audit_confidence_pill(conf: f32) -> (String, &'static str) {
    let pct = crate::probability_percent(conf);
    let cls = if pct >= 75.0 {
        "epi-pill epi-pill-cyan"
    } else if pct >= 50.0 {
        "epi-pill"
    } else {
        "epi-pill epi-pill-amber"
    };
    (format!("Audit Confidence: {pct:.0}%"), cls)
}

/// Compact candidate-card tag variant of [`audit_confidence_pill`].
pub(crate) fn audit_confidence_tag(conf: f32) -> (String, &'static str) {
    let pct = crate::probability_percent(conf);
    let cls = if pct >= 75.0 {
        "epi-tag epi-tag-cyan"
    } else if pct >= 50.0 {
        "epi-tag"
    } else {
        "epi-tag epi-tag-amber"
    };
    (format!("Audit Confidence: {pct:.0}%"), cls)
}

fn edge_relation_label(rel: EdgeRelation) -> &'static str {
    match rel {
        EdgeRelation::Calls => "calls",
        EdgeRelation::Imports => "imports",
        EdgeRelation::References => "refs",
        EdgeRelation::Contains => "contains",
    }
}

fn truncate_symbol(sym: &str, max_chars: usize) -> String {
    let mut chars = sym.chars();
    let collected: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{collected}…")
    } else {
        collected
    }
}

fn hazard_matches_node(hazard: &SemanticHazard, node: &GraphNode) -> bool {
    let hs = hazard.symbol.trim();
    if hs.is_empty() {
        return false;
    }
    let ns = node.symbol.trim();
    hs.eq_ignore_ascii_case(ns)
        || ns.ends_with(&format!(".{hs}"))
        || hs.ends_with(&format!(".{ns}"))
        || ns.contains(hs)
        || hs.contains(ns)
}

/// Computes all edge indices on connected caller/callee chains passing through `focus_id`.
fn compute_highlighted_chain(
    edges: &[GraphEdge],
    focus_id: &str,
) -> (HashSet<usize>, HashSet<String>) {
    let mut connected_nodes = HashSet::new();
    let mut connected_edges = HashSet::new();
    connected_nodes.insert(focus_id.to_string());

    // Upstream callers (to_id == curr)
    let mut q_up = VecDeque::new();
    let mut visited_up = HashSet::new();
    q_up.push_back(focus_id.to_string());
    visited_up.insert(focus_id.to_string());
    while let Some(curr) = q_up.pop_front() {
        for (idx, edge) in edges.iter().enumerate() {
            if edge.to_id == curr {
                connected_edges.insert(idx);
                connected_nodes.insert(edge.from_id.clone());
                if visited_up.insert(edge.from_id.clone()) {
                    q_up.push_back(edge.from_id.clone());
                }
            }
        }
    }

    // Downstream callees (from_id == curr)
    let mut q_down = VecDeque::new();
    let mut visited_down = HashSet::new();
    q_down.push_back(focus_id.to_string());
    visited_down.insert(focus_id.to_string());
    while let Some(curr) = q_down.pop_front() {
        for (idx, edge) in edges.iter().enumerate() {
            if edge.from_id == curr {
                connected_edges.insert(idx);
                connected_nodes.insert(edge.to_id.clone());
                if visited_down.insert(edge.to_id.clone()) {
                    q_down.push_back(edge.to_id.clone());
                }
            }
        }
    }

    (connected_edges, connected_nodes)
}

fn node_visual_radius(node: &GraphNode) -> f32 {
    match node.ring {
        ImpactRing::Ring0 => 17.0,
        ImpactRing::Ring1 => {
            if node.is_untested_dark_zone {
                14.5
            } else {
                13.0
            }
        }
        ImpactRing::Ring2 => {
            if node.is_untested_dark_zone {
                13.5
            } else {
                11.5
            }
        }
    }
}

fn curved_edge_path(x1: f32, y1: f32, x2: f32, y2: f32, r_start: f32, r_end: f32) -> Option<String> {
    let dx = x2 - x1;
    let dy = y2 - y1;
    let dist = dx.hypot(dy);
    if dist < 2.0 {
        return None;
    }
    let ux = dx / dist;
    let uy = dy / dist;
    let sx = x1 + ux * r_start;
    let sy = y1 + uy * r_start;
    let ex = x2 - ux * (r_end + 5.0);
    let ey = y2 - uy * (r_end + 5.0);

    // Subtle perpendicular bend so dense radial lines stay distinct.
    let mx = (sx + ex) * 0.5;
    let my = (sy + ey) * 0.5;
    let bend = (dist * 0.08).clamp(4.0, 22.0);
    let cx = mx - uy * bend;
    let cy = my + ux * bend;
    Some(format!("M {sx:.1} {sy:.1} Q {cx:.1} {cy:.1} {ex:.1} {ey:.1}"))
}

#[component]
pub fn BlastRadiusRippleMap(
    session: Signal<Option<TournamentSession>>,
    selected_strategy_id: RwSignal<Option<String>>,
) -> impl IntoView {
    let zoom = RwSignal::new(1.0_f64);
    let pan_x = RwSignal::new(0.0_f64);
    let pan_y = RwSignal::new(0.0_f64);
    let dragging = RwSignal::new(false);
    let drag_origin = RwSignal::new((0_i32, 0_i32, 0.0_f64, 0.0_f64));

    let hovered_node_id = RwSignal::new(Option::<String>::None);
    let focused_node_id = RwSignal::new(Option::<String>::None);
    let selected_node_id = RwSignal::new(Option::<String>::None);

    let on_zoom_in = move |_| {
        zoom.update(|z| *z = (*z * 1.18).clamp(0.45, 3.2));
    };
    let on_zoom_out = move |_| {
        zoom.update(|z| *z = (*z / 1.18).clamp(0.45, 3.2));
    };
    let on_pan_left = move |_| {
        pan_x.update(|x| *x = (*x + 48.0).clamp(-420.0, 420.0));
    };
    let on_pan_right = move |_| {
        pan_x.update(|x| *x = (*x - 48.0).clamp(-420.0, 420.0));
    };
    let on_pan_up = move |_| {
        pan_y.update(|y| *y = (*y + 48.0).clamp(-420.0, 420.0));
    };
    let on_pan_down = move |_| {
        pan_y.update(|y| *y = (*y - 48.0).clamp(-420.0, 420.0));
    };
    let on_reset_view = move |_| {
        zoom.set(1.0);
        pan_x.set(0.0);
        pan_y.set(0.0);
    };

    let on_wheel = move |ev: web_sys::WheelEvent| {
        ev.prevent_default();
        let factor = if ev.delta_y() < 0.0 { 1.10 } else { 0.90 };
        zoom.update(|z| *z = (*z * factor).clamp(0.45, 3.2));
    };

    let on_mouse_down = move |ev: web_sys::MouseEvent| {
        dragging.set(true);
        drag_origin.set((
            ev.client_x(),
            ev.client_y(),
            pan_x.get_untracked(),
            pan_y.get_untracked(),
        ));
    };

    let on_mouse_move = move |ev: web_sys::MouseEvent| {
        if dragging.get_untracked() {
            let (sx, sy, px, py) = drag_origin.get_untracked();
            let z = zoom.get_untracked().max(0.15);
            let dx = (ev.client_x() - sx) as f64 / z;
            let dy = (ev.client_y() - sy) as f64 / z;
            pan_x.set((px + dx).clamp(-420.0, 420.0));
            pan_y.set((py + dy).clamp(-420.0, 420.0));
        }
    };

    let on_mouse_up = move |_| {
        dragging.set(false);
    };

    view! {
        <section class="epi-ripple-card" aria-label="Code Impact Map">
            {move || {
                let Some(sess) = session.get() else {
                    return view! {
                        <div class="epi-ripple-header">
                            <div class="epi-ripple-title-group">
                                <h2 class="epi-ripple-title">
                                    <span class="model-dot dot-cyan" aria-hidden="true"></span>
                                    "Code Impact Map"
                                </h2>
                                <div class="epi-ripple-subtitle">"Modified code, direct callers, indirect entrypoints, and test coverage."</div>
                            </div>
                        </div>
                        <div class="epi-ripple-stage">
                            <div class="epi-empty-radar">
                                <svg viewBox="-120 -120 240 240" width="144" height="144" aria-hidden="true" focusable="false">
                                    <circle cx="0" cy="0" r="102" fill="none" stroke="#1e293b" stroke-dasharray="5 5" />
                                    <circle cx="0" cy="0" r="62" fill="none" stroke="#1e293b" stroke-dasharray="4 4" />
                                    <circle cx="0" cy="0" r="24" fill="rgba(56,189,248,0.12)" stroke="#38bdf8" stroke-dasharray="3 3" />
                                    <circle cx="0" cy="0" r="6" fill="#38bdf8" />
                                </svg>
                                <div>"Start or select a comparison to inspect affected code, callers, and test coverage."</div>
                            </div>
                        </div>
                    }.into_any();
                };

                let sel_id = selected_strategy_id.get();
                let Some(candidate) = select_active_candidate(&sess, sel_id.as_deref()).cloned() else {
                    return view! {
                        <div class="epi-ripple-header">
                            <div class="epi-ripple-title-group">
                                <h2 class="epi-ripple-title">
                                    <span class="model-dot dot-cyan"></span>
                                    "Code Impact Map"
                                </h2>
                                <div class="epi-ripple-subtitle">{sess.intake.title}</div>
                            </div>
                        </div>
                        <div class="epi-ripple-stage">
                            <div class="epi-empty-radar">
                                <div>"Waiting for candidate approaches to be built..."</div>
                            </div>
                        </div>
                    }.into_any();
                };

                let spec = sess
                    .strategy_plan
                    .as_ref()
                    .and_then(|p| p.strategies.iter().find(|s| s.id == candidate.strategy_id))
                    .cloned();

                let strategy_title = spec
                    .as_ref()
                    .map(|s| s.title.clone())
                    .unwrap_or_else(|| candidate.strategy_id.clone());
                let depth_tier_pill = spec.as_ref().map(|s| depth_tier_label(s.depth_tier));

                let Some(report) = candidate.blast_radius.clone() else {
                    return view! {
                        <div class="epi-ripple-header">
                            <div class="epi-ripple-title-group">
                                <h2 class="epi-ripple-title">
                                    <span class="model-dot dot-cyan"></span>
                                    "Code Impact Map"
                                </h2>
                                <div class="epi-ripple-subtitle">{strategy_title}</div>
                            </div>
                        </div>
                        <div class="epi-ripple-stage">
                            <div class="epi-empty-radar">
                                <div>"Running code impact and test coverage audit..."</div>
                            </div>
                        </div>
                    }.into_any();
                };

                render_blast_report_view(
                    candidate,
                    strategy_title,
                    depth_tier_pill,
                    report,
                    zoom,
                    pan_x,
                    pan_y,
                    dragging,
                    hovered_node_id,
                    focused_node_id,
                    selected_node_id,
                    on_zoom_in,
                    on_zoom_out,
                    on_pan_left,
                    on_pan_up,
                    on_pan_down,
                    on_pan_right,
                    on_reset_view,
                    on_wheel,
                    on_mouse_down,
                    on_mouse_move,
                    on_mouse_up,
                )
                .into_any()
            }}
        </section>
    }
}

#[allow(clippy::too_many_arguments)]
fn render_blast_report_view(
    candidate: CandidateResult,
    strategy_title: String,
    depth_tier_pill: Option<&'static str>,
    report: BlastRadiusReport,
    zoom: RwSignal<f64>,
    pan_x: RwSignal<f64>,
    pan_y: RwSignal<f64>,
    dragging: RwSignal<bool>,
    hovered_node_id: RwSignal<Option<String>>,
    focused_node_id: RwSignal<Option<String>>,
    selected_node_id: RwSignal<Option<String>>,
    on_zoom_in: impl Fn(web_sys::MouseEvent) + 'static,
    on_zoom_out: impl Fn(web_sys::MouseEvent) + 'static,
    on_pan_left: impl Fn(web_sys::MouseEvent) + 'static,
    on_pan_up: impl Fn(web_sys::MouseEvent) + 'static,
    on_pan_down: impl Fn(web_sys::MouseEvent) + 'static,
    on_pan_right: impl Fn(web_sys::MouseEvent) + 'static,
    on_reset_view: impl Fn(web_sys::MouseEvent) + 'static,
    on_wheel: impl Fn(web_sys::WheelEvent) + 'static,
    on_mouse_down: impl Fn(web_sys::MouseEvent) + 'static,
    on_mouse_move: impl Fn(web_sys::MouseEvent) + 'static,
    on_mouse_up: impl Fn(web_sys::MouseEvent) + Copy + 'static,
) -> impl IntoView {
    let all_nodes: Vec<GraphNode> = report.all_nodes().cloned().collect();
    let total_nodes = report.total_nodes();
    let dark_zones = report.dark_zone_count();
    let hazard_count = report.semantic_hazards.len();
    let composite_risk = report.composite_risk_score;
    let root_cause_depth = report.root_cause_depth_score;

    let risk_pill_class = if composite_risk <= 25.0 {
        "epi-pill epi-pill-emerald"
    } else if composite_risk <= 55.0 {
        "epi-pill epi-pill-amber"
    } else {
        "epi-pill epi-pill-red"
    };
    let depth_pill_class = if root_cause_depth >= 70.0 {
        "epi-pill epi-pill-emerald"
    } else if root_cause_depth >= 45.0 {
        "epi-pill epi-pill-cyan"
    } else {
        "epi-pill epi-pill-amber"
    };

    let dark_zone_pill_class = if dark_zones > 0 {
        "epi-pill epi-pill-amber"
    } else {
        "epi-pill epi-pill-emerald"
    };
    let hazard_pill_class = if hazard_count > 0 {
        "epi-pill epi-pill-red"
    } else {
        "epi-pill epi-pill-emerald"
    };
    let workaround_pill = report.bandaid_probability.map(workaround_risk_pill);
    let confidence_pill = report.decision_confidence.map(audit_confidence_pill);

    let node_positions: HashMap<String, (f32, f32, f32)> = all_nodes
        .iter()
        .map(|n| (n.id.clone(), (n.radial_x, n.radial_y, node_visual_radius(n))))
        .collect();

    let node_map: HashMap<String, GraphNode> = all_nodes
        .iter()
        .map(|n| (n.id.clone(), n.clone()))
        .collect();
    let dark_zone_ids: HashSet<String> = all_nodes
        .iter()
        .filter(|n| n.is_untested_dark_zone)
        .map(|n| n.id.clone())
        .collect();

    let default_inspector_id = all_nodes
        .iter()
        .find(|n| n.is_untested_dark_zone)
        .or_else(|| report.ring0_mutated.first())
        .or_else(|| all_nodes.first())
        .map(|n| n.id.clone());

    let edges_for_svg = report.edges.clone();
    let nodes_for_svg = all_nodes.clone();
    let hazards_for_svg = report.semantic_hazards.clone();

    let edges_for_inspector = report.edges.clone();
    let hazards_for_inspector = report.semantic_hazards.clone();
    let bandaid_warning = report.bandaid_debt_warning.clone();

    let map_aria_label = format!(
        "Interactive code impact map for {strategy_title}: {total_nodes} affected symbols, {dark_zones} untested callers, {hazard_count} code hazards"
    );
    let map_svg_title = map_aria_label.clone();

    view! {
        <div class="epi-ripple-header">
            <div class="epi-ripple-title-group">
                <h2 class="epi-ripple-title">
                    <span class="model-dot dot-cyan" aria-hidden="true"></span>
                    "Code Impact Map"
                </h2>
                <div class="epi-ripple-subtitle">
                    <span style="color:#e2e8f0; font-weight:600;">{strategy_title}</span>
                    {depth_tier_pill.map(|tier| view! {
                        <span class="epi-pill epi-pill-violet">{tier}</span>
                    })}
                    {candidate.added_caller_tests.then(|| view! {
                        <span class="epi-pill epi-pill-emerald">"+ Caller Tests"</span>
                    })}
                </div>
            </div>
            <div class="epi-zoom-controls" role="group" aria-label="Map pan and zoom controls">
                <button type="button" class="epi-zoom-btn" on:click=on_pan_left aria-label="Pan map left" title="Pan Left">"←"</button>
                <button type="button" class="epi-zoom-btn" on:click=on_pan_up aria-label="Pan map up" title="Pan Up">"↑"</button>
                <button type="button" class="epi-zoom-btn" on:click=on_pan_down aria-label="Pan map down" title="Pan Down">"↓"</button>
                <button type="button" class="epi-zoom-btn" on:click=on_pan_right aria-label="Pan map right" title="Pan Right">"→"</button>
                <button type="button" class="epi-zoom-btn" on:click=on_zoom_out aria-label="Zoom out" title="Zoom Out">"-"</button>
                <span class="epi-zoom-readout" aria-live="polite">
                    {move || format!("{}%", (zoom.get() * 100.0).round() as i32)}
                </span>
                <button type="button" class="epi-zoom-btn" on:click=on_zoom_in aria-label="Zoom in" title="Zoom In">"+"</button>
                <button type="button" class="epi-zoom-btn" on:click=on_reset_view aria-label="Reset zoom and pan" title="Reset Zoom & Pan">"Reset"</button>
            </div>
        </div>

        <div class="epi-ripple-metrics-strip">
            <span class=risk_pill_class>{format!("Impact Risk: {:.0}/100", composite_risk)}</span>
            <span class=depth_pill_class>{format!("Root-Cause Depth: {:.0}/100", root_cause_depth)}</span>
            {workaround_pill.map(|(label, cls)| view! {
                <span
                    class=cls
                    title=format!("Calibrated symptom-workaround probability via {GEMINI_DECISION_MODEL}")
                >
                    {label}
                </span>
            })}
            {confidence_pill.map(|(label, cls)| view! {
                <span
                    class=cls
                    title=format!("Mean calibrated audit confidence via {GEMINI_DECISION_MODEL}")
                >
                    {label}
                </span>
            })}
            <span class="epi-pill">{format!("Symbols: {total_nodes} (Modified: {} · Direct: {} · Indirect: {})", report.ring0_mutated.len(), report.ring1_direct_callers.len(), report.ring2_transitive_entrypoints.len())}</span>
            <span class=dark_zone_pill_class>
                {format!("Untested Callers: {dark_zones}")}
            </span>
            <span class=hazard_pill_class>
                {format!("Code Hazards: {hazard_count}")}
            </span>
        </div>

        {bandaid_warning.map(|warning| view! {
            <div class="epi-bandaid-alert" role="note">
                <span class="epi-bandaid-title">"Shallow Fix Warning"</span>
                <span>{warning}</span>
            </div>
        })}

        <div class="epi-ripple-stage">
            <svg
                class="epi-ripple-svg"
                role="group"
                aria-label=map_aria_label
                viewBox="-360 -360 720 720"
                style=move || {
                    if dragging.get() {
                        "cursor: grabbing;"
                    } else {
                        "cursor: grab;"
                    }
                }
                on:wheel=on_wheel
                on:mousedown=on_mouse_down
                on:mousemove=on_mouse_move
                on:mouseup=on_mouse_up
                on:mouseleave=on_mouse_up
            >
                <title>{map_svg_title}</title>
                <defs>
                    <radialGradient id="epi-epicenter-radial" cx="50%" cy="50%" r="50%">
                        <stop offset="0%" stop-color="#38bdf8" stop-opacity="0.28" />
                        <stop offset="60%" stop-color="#818cf8" stop-opacity="0.14" />
                        <stop offset="100%" stop-color="#0f1624" stop-opacity="0.0" />
                    </radialGradient>
                    <filter id="epi-glow-cyan" x="-45%" y="-45%" width="190%" height="190%">
                        <feGaussianBlur stdDeviation="4.5" result="blur" />
                        <feMerge>
                            <feMergeNode in="blur" />
                            <feMergeNode in="SourceGraphic" />
                        </feMerge>
                    </filter>
                    <filter id="epi-glow-amber" x="-45%" y="-45%" width="190%" height="190%">
                        <feGaussianBlur stdDeviation="4.0" result="blur" />
                        <feMerge>
                            <feMergeNode in="blur" />
                            <feMergeNode in="SourceGraphic" />
                        </feMerge>
                    </filter>
                    <filter id="epi-glow-emerald" x="-45%" y="-45%" width="190%" height="190%">
                        <feGaussianBlur stdDeviation="3.2" result="blur" />
                        <feMerge>
                            <feMergeNode in="blur" />
                            <feMergeNode in="SourceGraphic" />
                        </feMerge>
                    </filter>
                    <marker id="epi-arrow-dim" viewBox="0 0 10 10" refX="8" refY="5" markerWidth="6" markerHeight="6" orient="auto-start-reverse">
                        <path d="M 0 1.5 L 9 5 L 0 8.5 z" fill="#475569" />
                    </marker>
                    <marker id="epi-arrow-hi" viewBox="0 0 10 10" refX="8" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse">
                        <path d="M 0 1.5 L 9 5 L 0 8.5 z" fill="#38bdf8" />
                    </marker>
                    <marker id="epi-arrow-warn" viewBox="0 0 10 10" refX="8" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse">
                        <path d="M 0 1.5 L 9 5 L 0 8.5 z" fill="#f59e0b" />
                    </marker>
                </defs>

                <g transform=move || format!("translate({:.2} {:.2}) scale({:.3})", pan_x.get(), pan_y.get(), zoom.get())>
                    // Subtle radar axes
                    <line x1="-335" y1="0" x2="335" y2="0" stroke="#1e293b" stroke-width="1" stroke-dasharray="3 6" opacity="0.7" />
                    <line x1="0" y1="-335" x2="0" y2="335" stroke="#1e293b" stroke-width="1" stroke-dasharray="3 6" opacity="0.7" />
                    <line x1="-238" y1="-238" x2="238" y2="238" stroke="#1e293b" stroke-width="0.9" stroke-dasharray="2 8" opacity="0.45" />
                    <line x1="-238" y1="238" x2="238" y2="-238" stroke="#1e293b" stroke-width="0.9" stroke-dasharray="2 8" opacity="0.45" />

                    // Concentric dashed rings (Ring 2 -> Ring 1 -> Ring 0)
                    <circle cx="0" cy="0" r=RING2_RADIUS.to_string() fill="none" stroke="#334155" stroke-width="1.3" stroke-dasharray="6 6" />
                    <circle cx="0" cy="0" r=RING1_RADIUS.to_string() fill="none" stroke="#38bdf8" stroke-opacity="0.38" stroke-width="1.4" stroke-dasharray="5 5" />
                    <circle cx="0" cy="0" r=RING0_RADIUS.to_string() fill="url(#epi-epicenter-radial)" stroke="#818cf8" stroke-opacity="0.75" stroke-width="1.6" stroke-dasharray="4 4" />

                    // Ring Labels
                    <text x="0" y="-288" text-anchor="middle" fill="#8b9db5" font-size="10" font-weight="700" letter-spacing="1.4">
                        "INDIRECT ENTRYPOINTS"
                    </text>
                    <text x="0" y="-168" text-anchor="middle" fill="#7dd3fc" font-size="10" font-weight="700" letter-spacing="1.2">
                        "DIRECT CALLERS"
                    </text>
                    <text x="0" y="-64" text-anchor="middle" fill="#c4b5fd" font-size="9.5" font-weight="700" letter-spacing="1.1">
                        "MODIFIED CODE"
                    </text>

                    // Directed edges highlighting connected caller/callee chains
                    {move || {
                        let focus_id = hovered_node_id
                            .get()
                            .or_else(|| focused_node_id.get())
                            .or_else(|| selected_node_id.get());
                        let (hi_edges, _) = match focus_id.as_deref() {
                            Some(fid) => compute_highlighted_chain(&edges_for_svg, fid),
                            None => (HashSet::new(), HashSet::new()),
                        };
                        let has_focus = focus_id.is_some();

                        edges_for_svg
                            .iter()
                            .enumerate()
                            .filter_map(|(idx, edge)| {
                                let &(x1, y1, r1) = node_positions.get(&edge.from_id)?;
                                let &(x2, y2, r2) = node_positions.get(&edge.to_id)?;
                                let path_d = curved_edge_path(x1, y1, x2, y2, r1, r2)?;
                                let is_hi = hi_edges.contains(&idx);
                                let touches_dark = dark_zone_ids.contains(&edge.from_id)
                                    || dark_zone_ids.contains(&edge.to_id);

                                let (stroke, stroke_width, opacity, marker) = if is_hi {
                                    if touches_dark {
                                        ("#f59e0b", "2.4", "0.95", "url(#epi-arrow-warn)")
                                    } else {
                                        ("#38bdf8", "2.4", "0.95", "url(#epi-arrow-hi)")
                                    }
                                } else if has_focus {
                                    ("#334155", "1.1", "0.24", "url(#epi-arrow-dim)")
                                } else if touches_dark {
                                    ("#b45309", "1.5", "0.62", "url(#epi-arrow-warn)")
                                } else {
                                    ("#475569", "1.35", "0.55", "url(#epi-arrow-dim)")
                                };

                                Some(view! {
                                    <path
                                        d=path_d
                                        fill="none"
                                        stroke=stroke
                                        stroke-width=stroke_width
                                        opacity=opacity
                                        marker-end=marker
                                    />
                                })
                            })
                            .collect_view()
                    }}

                    // Graph Nodes
                    {move || {
                        let focus_id = hovered_node_id
                            .get()
                            .or_else(|| focused_node_id.get())
                            .or_else(|| selected_node_id.get());
                        let (_, hi_nodes) = match focus_id.as_deref() {
                            Some(fid) => compute_highlighted_chain(&report.edges, fid),
                            None => (HashSet::new(), HashSet::new()),
                        };
                        let has_focus = focus_id.is_some();
                        let active_sel = selected_node_id.get();

                        nodes_for_svg
                            .iter()
                            .map(|node| {
                                let node_id_click = node.id.clone();
                                let node_id_key = node.id.clone();
                                let node_id_enter = node.id.clone();
                                let node_id_leave = node.id.clone();
                                let node_id_focus = node.id.clone();
                                let node_id_blur = node.id.clone();

                                let x = node.radial_x;
                                let y = node.radial_y;
                                let r = node_visual_radius(node);
                                let is_selected = active_sel.as_deref() == Some(node.id.as_str());
                                let is_hovered = hovered_node_id.get().as_deref() == Some(node.id.as_str());
                                let is_focused = focused_node_id.get().as_deref() == Some(node.id.as_str());
                                let is_in_chain = !has_focus || hi_nodes.contains(&node.id);
                                let node_opacity = if is_in_chain { "1.0" } else { "0.35" };

                                let has_hazard = hazards_for_svg.iter().any(|h| hazard_matches_node(h, node));
                                let short_sym = truncate_symbol(&node.symbol, 18);

                                let (fill, stroke, filter_attr) = if node.ring == ImpactRing::Ring0 {
                                    ("#1e1b4b", "#38bdf8", "url(#epi-glow-cyan)")
                                } else if node.is_untested_dark_zone {
                                    ("#451a03", "#ef4444", "url(#epi-glow-amber)")
                                } else {
                                    ("#064e3b", "#10b981", "url(#epi-glow-emerald)")
                                };

                                let cov_badge = if node.ring == ImpactRing::Ring0 {
                                    match node.test_coverage_pct {
                                        Some(pct) => format!("MODIFIED · {:.0}%", pct),
                                        None => "MODIFIED".to_string(),
                                    }
                                } else if node.is_untested_dark_zone {
                                    "0% TESTED".to_string()
                                } else {
                                    match node.test_coverage_pct {
                                        Some(pct) => format!("{:.0}% TESTED", pct),
                                        None => "CALLER".to_string(),
                                    }
                                };

                                let node_aria = format!(
                                    "{} ({}, {}, {}{})",
                                    node.symbol,
                                    ring_label(node.ring),
                                    node.file_path,
                                    cov_badge,
                                    if has_hazard { ", has code hazard" } else { "" }
                                );

                                let badge_fill = if node.is_untested_dark_zone {
                                    "#fca5a5"
                                } else if node.ring == ImpactRing::Ring0 {
                                    "#c4b5fd"
                                } else {
                                    "#6ee7b7"
                                };

                                let label_y = y + r + 13.0;
                                let badge_y = y + r + 24.5;

                                view! {
                                    <g
                                        class="epi-node-group"
                                        role="button"
                                        tabindex="0"
                                        aria-label=node_aria
                                        aria-pressed=is_selected.to_string()
                                        opacity=node_opacity
                                        on:click=move |ev: web_sys::MouseEvent| {
                                            ev.stop_propagation();
                                            selected_node_id.set(Some(node_id_click.clone()));
                                        }
                                        on:keydown=move |ev: web_sys::KeyboardEvent| {
                                            let key = ev.key();
                                            if key == "Enter" || key == " " {
                                                ev.prevent_default();
                                                selected_node_id.set(Some(node_id_key.clone()));
                                            }
                                        }
                                        on:mouseenter=move |_| {
                                            hovered_node_id.set(Some(node_id_enter.clone()));
                                        }
                                        on:mouseleave=move |_| {
                                            if hovered_node_id.get_untracked().as_deref() == Some(node_id_leave.as_str()) {
                                                hovered_node_id.set(None);
                                            }
                                        }
                                        on:focus=move |_| {
                                            focused_node_id.set(Some(node_id_focus.clone()));
                                        }
                                        on:blur=move |_| {
                                            if focused_node_id.get_untracked().as_deref() == Some(node_id_blur.as_str()) {
                                                focused_node_id.set(None);
                                            }
                                        }
                                    >
                                        // Persistent + brief pulse warning halo for untested callers
                                        {node.is_untested_dark_zone.then(|| view! {
                                            <>
                                                <circle
                                                    cx=format!("{x:.1}")
                                                    cy=format!("{y:.1}")
                                                    r=format!("{:.1}", r + 4.5)
                                                    fill="none"
                                                    stroke="#f59e0b"
                                                    stroke-width="1.6"
                                                    stroke-dasharray="3 3"
                                                    opacity="0.85"
                                                />
                                                <circle
                                                    class="epi-darkzone-pulse-ring"
                                                    cx=format!("{x:.1}")
                                                    cy=format!("{y:.1}")
                                                    r=format!("{:.1}", r + 5.0)
                                                    fill="none"
                                                    stroke="#f59e0b"
                                                    stroke-width="2.2"
                                                />
                                            </>
                                        })}

                                        // Ring 0 halo
                                        {(node.ring == ImpactRing::Ring0).then(|| view! {
                                            <circle
                                                class="epi-epicenter-glow-ring"
                                                cx=format!("{x:.1}")
                                                cy=format!("{y:.1}")
                                                r=format!("{:.1}", r + 6.0)
                                                fill="none"
                                                stroke="#c4b5fd"
                                                stroke-width="1.8"
                                                stroke-dasharray="3 3"
                                            />
                                        })}

                                        // Selection / Hover / Keyboard Focus ring
                                        {(is_selected || is_hovered || is_focused).then(|| view! {
                                            <circle
                                                cx=format!("{x:.1}")
                                                cy=format!("{y:.1}")
                                                r=format!("{:.1}", r + if is_focused { 6.5 } else { 4.0 })
                                                fill="none"
                                                stroke=if is_focused { "#7dd3fc" } else { "#f8fafc" }
                                                stroke-width=if is_focused { "2.8" } else if is_selected { "2.2" } else { "1.4" }
                                            />
                                        })}

                                        // Core symbol circle
                                        <circle
                                            cx=format!("{x:.1}")
                                            cy=format!("{y:.1}")
                                            r=format!("{r:.1}")
                                            fill=fill
                                            stroke=stroke
                                            stroke-width=if is_selected { "2.8" } else { "2.0" }
                                            filter=filter_attr
                                        />

                                        // Inner accent dot
                                        <circle
                                            cx=format!("{x:.1}")
                                            cy=format!("{y:.1}")
                                            r=format!("{:.1}", (r * 0.34).max(3.2))
                                            fill=stroke
                                        />

                                        // Hazard exclamation pip
                                        {has_hazard.then(|| view! {
                                            <g>
                                                <circle
                                                    cx=format!("{:.1}", x + r * 0.78)
                                                    cy=format!("{:.1}", y - r * 0.78)
                                                    r="6.5"
                                                    fill="#ef4444"
                                                    stroke="#0f1624"
                                                    stroke-width="1.5"
                                                />
                                                <text
                                                    x=format!("{:.1}", x + r * 0.78)
                                                    y=format!("{:.1}", y - r * 0.78 + 3.2)
                                                    text-anchor="middle"
                                                    fill="#ffffff"
                                                    font-size="9"
                                                    font-weight="800"
                                                >"!"</text>
                                            </g>
                                        })}

                                        // Symbol label
                                        <text
                                            x=format!("{x:.1}")
                                            y=format!("{label_y:.1}")
                                            text-anchor="middle"
                                            fill="#e2e8f0"
                                            font-size="10.5"
                                            font-weight="600"
                                            font-family="ui-monospace, SFMono-Regular, Menlo, monospace"
                                            paint-order="stroke"
                                            stroke="#080c14"
                                            stroke-width="3.2"
                                        >
                                            {short_sym}
                                        </text>

                                        // Coverage / Ring micro-label
                                        <text
                                            x=format!("{x:.1}")
                                            y=format!("{badge_y:.1}")
                                            text-anchor="middle"
                                            fill=badge_fill
                                            font-size="8.5"
                                            font-weight="700"
                                            font-family="ui-monospace, SFMono-Regular, Menlo, monospace"
                                            paint-order="stroke"
                                            stroke="#080c14"
                                            stroke-width="2.6"
                                        >
                                            {cov_badge}
                                        </text>
                                    </g>
                                }
                            })
                            .collect_view()
                    }}
                </g>
            </svg>
            <div class="epi-ripple-hint">"Drag or use pan/zoom buttons · Click or Tab + Enter on a node to inspect"</div>
        </div>

        <div class="epi-ripple-legend">
            <span class="epi-legend-item">
                <span class="epi-legend-dot" aria-hidden="true" style="background: #38bdf8; box-shadow: 0 0 8px #a78bfa;"></span>
                "Modified Code"
            </span>
            <span class="epi-legend-item">
                <span class="epi-legend-dot" aria-hidden="true" style="background: #10b981; box-shadow: 0 0 8px #10b981;"></span>
                "Covered Caller"
            </span>
            <span class="epi-legend-item">
                <span class="epi-legend-dot" aria-hidden="true" style="background: #ef4444; box-shadow: 0 0 8px #f59e0b;"></span>
                "Untested Caller (0%)"
            </span>
            <span class="epi-legend-item">
                <span class="epi-legend-dot" aria-hidden="true" style="background: #dc2626; border: 1px solid #fff;"></span>
                "! Code Hazard"
            </span>
        </div>

        // Node Inspector Card
        {move || {
            let active_id = selected_node_id
                .get()
                .filter(|id| node_map.contains_key(id))
                .or_else(|| focused_node_id.get().filter(|id| node_map.contains_key(id)))
                .or_else(|| hovered_node_id.get().filter(|id| node_map.contains_key(id)))
                .or_else(|| default_inspector_id.clone());

            let inspected = active_id.as_deref().and_then(|id| node_map.get(id)).cloned();

            view! {
                <div class="epi-inspector-card" role="region" aria-live="polite" aria-label="Selected symbol inspector">
                    {match inspected {
                        Some(node) => {
                            let cov_pct = node.test_coverage_pct;
                            let cov_width = cov_pct.unwrap_or(0.0).clamp(0.0, 100.0);
                            let cov_color = if node.is_untested_dark_zone {
                                "#fca5a5"
                            } else if cov_pct.map(|p| p >= 75.0).unwrap_or(false) {
                                "#6ee7b7"
                            } else if cov_pct.is_some() {
                                "#fcd34d"
                            } else {
                                "#8b9db5"
                            };
                            let cov_text = if node.is_untested_dark_zone {
                                "0% (Untested)".to_string()
                            } else {
                                match cov_pct {
                                    Some(p) => format!("{:.0}% line coverage", p),
                                    None => "Not measured".to_string(),
                                }
                            };

                            let ring_class = match node.ring {
                                ImpactRing::Ring0 => "epi-pill epi-pill-violet",
                                ImpactRing::Ring1 => "epi-pill epi-pill-cyan",
                                ImpactRing::Ring2 => "epi-pill",
                            };

                            let incoming: Vec<(String, String, EdgeRelation)> = edges_for_inspector
                                .iter()
                                .filter(|e| e.to_id == node.id)
                                .filter_map(|e| {
                                    node_map.get(&e.from_id).map(|src| {
                                        (src.id.clone(), src.symbol.clone(), e.relation)
                                    })
                                })
                                .collect();
                            let outgoing: Vec<(String, String, EdgeRelation)> = edges_for_inspector
                                .iter()
                                .filter(|e| e.from_id == node.id)
                                .filter_map(|e| {
                                    node_map.get(&e.to_id).map(|dst| {
                                        (dst.id.clone(), dst.symbol.clone(), e.relation)
                                    })
                                })
                                .collect();

                            let matching_hazards: Vec<SemanticHazard> = hazards_for_inspector
                                .iter()
                                .filter(|h| hazard_matches_node(h, &node))
                                .cloned()
                                .collect();
                            let other_hazards: Vec<SemanticHazard> = hazards_for_inspector
                                .iter()
                                .filter(|h| !hazard_matches_node(h, &node))
                                .cloned()
                                .collect();

                            view! {
                                <div class="epi-inspector-head">
                                    <div>
                                        <div class="epi-inspector-symbol">{node.symbol.clone()}</div>
                                        <div class="epi-inspector-loc">{format!("{}:{}", node.file_path, node.line)}</div>
                                    </div>
                                    <div style="display:flex; flex-wrap:wrap; gap:6px;">
                                        <span class=ring_class>{ring_label(node.ring)}</span>
                                        {node.is_untested_dark_zone.then(|| view! {
                                            <span class="epi-pill epi-pill-red">"Untested Caller"</span>
                                        })}
                                    </div>
                                </div>

                                <div style="display:flex; flex-direction:column; gap:4px;">
                                    <div style="display:flex; justify-content:space-between; font-size:11.5px; color:#94a3b8;">
                                        <span>"Test Coverage"</span>
                                        <span style=format!("color:{cov_color}; font-weight:600;")>{cov_text}</span>
                                    </div>
                                    <div class="epi-cov-bar-track">
                                        <div
                                            class="epi-cov-bar-fill"
                                            style=format!("width:{cov_width:.1}%; background:{cov_color};")
                                        ></div>
                                    </div>
                                </div>

                                {(!incoming.is_empty()).then(|| view! {
                                    <div class="epi-neighbor-row">
                                        <span>"Called by:"</span>
                                        {incoming.into_iter().map(|(id, sym, rel)| {
                                            let jump_id = id.clone();
                                            view! {
                                                <button
                                                    type="button"
                                                    class="epi-neighbor-chip"
                                                    on:click=move |_| selected_node_id.set(Some(jump_id.clone()))
                                                >
                                                    {format!("{sym} ({})", edge_relation_label(rel))}
                                                </button>
                                            }
                                        }).collect_view()}
                                    </div>
                                })}

                                {(!outgoing.is_empty()).then(|| view! {
                                    <div class="epi-neighbor-row">
                                        <span>"Calls:"</span>
                                        {outgoing.into_iter().map(|(id, sym, rel)| {
                                            let jump_id = id.clone();
                                            view! {
                                                <button
                                                    type="button"
                                                    class="epi-neighbor-chip"
                                                    on:click=move |_| selected_node_id.set(Some(jump_id.clone()))
                                                >
                                                    {format!("→ {sym} ({})", edge_relation_label(rel))}
                                                </button>
                                            }
                                        }).collect_view()}
                                    </div>
                                })}

                                <div class="epi-hazard-list">
                                    <div style="font-size:11px; font-weight:700; text-transform:uppercase; letter-spacing:0.05em; color:#a8b7cc;">
                                        {format!("Code Hazards ({} on this symbol)", matching_hazards.len())}
                                    </div>
                                    {if matching_hazards.is_empty() {
                                        view! {
                                            <div style="font-size:12px; color:#8b9db5;">
                                                "No hazards flagged on this symbol."
                                            </div>
                                        }.into_any()
                                    } else {
                                        matching_hazards.into_iter().map(render_hazard_item).collect_view().into_any()
                                    }}

                                    {(!other_hazards.is_empty()).then(|| view! {
                                        <div style="margin-top:6px; font-size:11px; font-weight:700; text-transform:uppercase; letter-spacing:0.05em; color:#a8b7cc;">
                                            {format!("Other Hazards ({})", other_hazards.len())}
                                        </div>
                                        {other_hazards.into_iter().map(render_hazard_item).collect_view()}
                                    })}
                                </div>
                            }.into_any()
                        }
                        None => view! {
                            <div style="font-size:12px; color:#94a3b8;">
                                "No symbols extracted for this diff yet."
                            </div>
                        }.into_any(),
                    }}
                </div>
            }
        }}
    }
}

fn render_hazard_item(hazard: SemanticHazard) -> impl IntoView {
    let pill_cls = severity_pill_class(hazard.severity);
    let sev_text = severity_label(hazard.severity);
    view! {
        <div class="epi-hazard-item">
            <div class="epi-hazard-top">
                <span class=pill_cls>{sev_text}</span>
                <span class="epi-pill epi-pill-cyan">{hazard.category}</span>
                <span style="font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size:11px; color:#e2e8f0;">
                    {hazard.symbol}
                </span>
            </div>
            <div class="epi-hazard-expl">{hazard.explanation}</div>
            <div class="epi-hazard-mit">
                <strong>"Mitigation: "</strong>
                {hazard.mitigation}
            </div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::{
        audit_confidence_pill, audit_confidence_tag, workaround_risk_pill, workaround_risk_tag,
    };

    #[test]
    fn workaround_risk_thresholds_match_spec() {
        assert_eq!(
            workaround_risk_pill(0.0),
            ("Workaround Risk: 0%".to_string(), "epi-pill epi-pill-emerald")
        );
        assert_eq!(
            workaround_risk_pill(0.25),
            ("Workaround Risk: 25%".to_string(), "epi-pill epi-pill-emerald")
        );
        assert_eq!(
            workaround_risk_pill(0.26),
            ("Workaround Risk: 26%".to_string(), "epi-pill epi-pill-amber")
        );
        assert_eq!(
            workaround_risk_pill(0.60),
            ("Workaround Risk: 60%".to_string(), "epi-pill epi-pill-amber")
        );
        assert_eq!(
            workaround_risk_pill(0.61),
            ("Workaround Risk: 61%".to_string(), "epi-pill epi-pill-red")
        );
        assert_eq!(
            workaround_risk_pill(1.0),
            ("Workaround Risk: 100%".to_string(), "epi-pill epi-pill-red")
        );

        assert_eq!(
            workaround_risk_tag(0.18),
            ("Workaround Risk: 18%".to_string(), "epi-tag epi-tag-emerald")
        );
        assert_eq!(
            workaround_risk_tag(0.45),
            ("Workaround Risk: 45%".to_string(), "epi-tag epi-tag-amber")
        );
        assert_eq!(
            workaround_risk_tag(0.82),
            ("Workaround Risk: 82%".to_string(), "epi-tag epi-tag-red")
        );
    }

    #[test]
    fn audit_confidence_formats_and_tiers_cleanly() {
        assert_eq!(
            audit_confidence_pill(0.91),
            ("Audit Confidence: 91%".to_string(), "epi-pill epi-pill-cyan")
        );
        assert_eq!(
            audit_confidence_pill(0.75),
            ("Audit Confidence: 75%".to_string(), "epi-pill epi-pill-cyan")
        );
        assert_eq!(
            audit_confidence_pill(0.62),
            ("Audit Confidence: 62%".to_string(), "epi-pill")
        );
        assert_eq!(
            audit_confidence_pill(0.42),
            ("Audit Confidence: 42%".to_string(), "epi-pill epi-pill-amber")
        );

        assert_eq!(
            audit_confidence_tag(0.88),
            ("Audit Confidence: 88%".to_string(), "epi-tag epi-tag-cyan")
        );
        assert_eq!(
            audit_confidence_tag(0.55),
            ("Audit Confidence: 55%".to_string(), "epi-tag")
        );
        assert_eq!(
            audit_confidence_tag(0.30),
            ("Audit Confidence: 30%".to_string(), "epi-tag epi-tag-amber")
        );
    }
}
