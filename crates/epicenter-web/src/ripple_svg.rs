// Owned by `neon-web-viz`: Interactive concentric `<svg>` Ring 0 / Ring 1 / Ring 2 Blast-Radius Ripple Map.
use epicenter_common::TournamentSession;
use leptos::prelude::*;

#[component]
pub fn BlastRadiusRippleMap(
    session: Signal<Option<TournamentSession>>,
    selected_strategy_id: RwSignal<Option<String>>,
) -> impl IntoView {
    let _ = (session, selected_strategy_id);
    view! { <div class="ripple-map-placeholder">"Blast-Radius Ripple Map"</div> }
}
