use epicenter_common::{Gemini4Model, ImpactRing, TournamentStage};
use leptos::prelude::*;

#[component]
fn App() -> impl IntoView {
    let models = Gemini4Model::all();
    let initial_stage = TournamentStage::Intake;
    let ring = ImpactRing::Ring0;

    view! {
        <main style="padding: 2rem; max-width: 960px; margin: 0 auto;">
            <header style="display: flex; align-items: baseline; justify-content: space-between; border-bottom: 1px solid #1e293b; padding-bottom: 1rem;">
                <h1 style="margin: 0; font-size: 1.75rem; letter-spacing: 0.04em;">"EPICENTER"</h1>
                <span style="font-size: 0.85rem; color: #94a3b8;">
                    {format!("Stage: {:?} | Focus: {:?}", initial_stage, ring)}
                </span>
            </header>
            <section style="margin-top: 1.5rem;">
                <p style="color: #cbd5e1;">
                    "100% Pure-Rust Gemini 4 Strategy Tournament, Arena Benchmark & Blast-Radius Console."
                </p>
                <div style="display: flex; gap: 0.5rem; flex-wrap: wrap; margin-top: 1rem;">
                    {models
                        .iter()
                        .map(|m| {
                            view! {
                                <span style="padding: 0.25rem 0.65rem; border-radius: 999px; background: #1e293b; font-size: 0.8rem;">
                                    {m.display_name()}
                                </span>
                            }
                        })
                        .collect_view()}
                </div>
            </section>
        </main>
    }
}

fn main() {
    console_error_panic_hook::set_once();
    mount_to_body(App);
}
