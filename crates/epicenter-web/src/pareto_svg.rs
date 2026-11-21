// Owned by `neon-web-viz`: `<svg>` Speedup-vs-Risk Pareto scatter chart + Arena p50/p95 bars + 3 Crowned Badges.
use epicenter_common::TournamentSession;
use leptos::prelude::*;

#[component]
pub fn ParetoAndArenaPanel(
    session: Signal<Option<TournamentSession>>,
    selected_strategy_id: RwSignal<Option<String>>,
) -> impl IntoView {
    let _ = (session, selected_strategy_id);
    view! { <div class="pareto-panel-placeholder">"Pareto & Arena Panel"</div> }
}
