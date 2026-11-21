pub mod api_client;
pub mod briefing;
pub mod capture;
pub mod eval_view;
pub mod pareto_svg;
pub mod ripple_svg;

use leptos::prelude::*;

fn main() {
    console_error_panic_hook::set_once();
    mount_to_body(App);
}

#[component]
pub fn App() -> impl IntoView {
    view! {
        <div class="epicenter-app">
            <h1>"Epicenter Studio"</h1>
        </div>
    }
}
