pub mod api_client;
pub mod briefing;
pub mod capture;
pub mod eval_view;
pub mod pareto_svg;
pub mod ripple_svg;

use crate::api_client::{SseSubscription, TelemetryLogEntry};
use crate::briefing::BriefingAndHandoffDeck;
use crate::capture::MultimodalIntakeDeck;
use crate::eval_view::FiveSigmaEvalView;
use crate::pareto_svg::ParetoAndArenaPanel;
use crate::ripple_svg::BlastRadiusRippleMap;
use epicenter_common::{
    LiveEvent, LogLevel, ServerStatusResponse, SessionListItem, TaskKind, TournamentSession,
    TournamentStage,
};
use leptos::prelude::*;
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StudioTab {
    MissionControl,
    FiveSigmaEval,
}

const PIPELINE_STAGES: [(TournamentStage, &str); 7] = [
    (TournamentStage::Intake, "1. Intake"),
    (TournamentStage::RedGate, "2. Red-Gate"),
    (TournamentStage::Planning, "3. Plan K"),
    (TournamentStage::Executing, "4. Sandboxes"),
    (TournamentStage::Benchmarking, "5. Arena"),
    (TournamentStage::Auditing, "6. Blast Audit"),
    (TournamentStage::Completed, "7. Briefing"),
];

fn stage_rank(stage: TournamentStage) -> usize {
    match stage {
        TournamentStage::Intake => 0,
        TournamentStage::RedGate => 1,
        TournamentStage::Planning => 2,
        TournamentStage::Executing => 3,
        TournamentStage::Benchmarking => 4,
        TournamentStage::Auditing => 5,
        TournamentStage::Completed => 6,
        TournamentStage::Failed => 6,
    }
}

fn stage_label(stage: TournamentStage) -> &'static str {
    match stage {
        TournamentStage::Intake => "Intake",
        TournamentStage::RedGate => "Red-Gate",
        TournamentStage::Planning => "Planning",
        TournamentStage::Executing => "Executing",
        TournamentStage::Benchmarking => "Benchmarking",
        TournamentStage::Auditing => "Auditing",
        TournamentStage::Completed => "Completed",
        TournamentStage::Failed => "Failed",
    }
}

fn log_level_class(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Info => "log-lvl info",
        LogLevel::Warn => "log-lvl warn",
        LogLevel::Error => "log-lvl error",
        LogLevel::Debug => "log-lvl debug",
    }
}

fn log_level_text(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Info => "INFO",
        LogLevel::Warn => "WARN",
        LogLevel::Error => "ERR",
        LogLevel::Debug => "DBG",
    }
}

fn main() {
    console_error_panic_hook::set_once();
    mount_to_body(App);
}

#[component]
pub fn App() -> impl IntoView {
    let active_tab = RwSignal::new(StudioTab::MissionControl);

    let server_status = RwSignal::new(Option::<ServerStatusResponse>::None);
    let server_err = RwSignal::new(Option::<String>::None);

    let sessions_list = RwSignal::new(Vec::<SessionListItem>::new());
    let active_session = RwSignal::new(Option::<TournamentSession>::None);
    let active_session_signal: Signal<Option<TournamentSession>> = active_session.into();
    let selected_strategy_id = RwSignal::new(Option::<String>::None);
    let has_github_token_signal = Signal::derive(move || {
        server_status
            .get()
            .map(|s| s.has_github_token)
            .unwrap_or(false)
    });

    let show_intake = RwSignal::new(true);
    let show_telemetry = RwSignal::new(true);
    let telemetry_logs = RwSignal::new(Vec::<TelemetryLogEntry>::new());
    let sse_status = RwSignal::new("IDLE".to_string());
    let loading_session = RwSignal::new(false);
    let session_error = RwSignal::new(Option::<String>::None);

    let sse_handle = StoredValue::new_local(Option::<SseSubscription>::None);

    on_cleanup(move || {
        sse_handle.update_value(|slot| {
            let _ = slot.take();
        });
    });

    let refresh_sessions = move || {
        spawn_local(async move {
            if let Ok(items) = api_client::list_sessions().await {
                sessions_list.set(items);
            }
        });
    };

    let sync_full_session = move |session_id: Uuid| {
        spawn_local(async move {
            if let Ok(full) = api_client::get_session(session_id).await {
                let rec = full
                    .arena_summary
                    .as_ref()
                    .and_then(|s| s.recommended_strategy_id.clone())
                    .or_else(|| full.candidates.first().map(|c| c.strategy_id.clone()));
                active_session.set(Some(full));
                if selected_strategy_id.get_untracked().is_none() {
                    selected_strategy_id.set(rec);
                }
            }
        });
    };

    let connect_sse = move |session_id: Uuid| {
        // Close any previous session EventSource before opening the new stream.
        sse_handle.update_value(|slot| {
            let _ = slot.take();
        });
        sse_status.set("LIVE SSE".to_string());

        let sub = api_client::subscribe_session_events(
            session_id,
            move |evt: LiveEvent| {
                let entry = api_client::live_event_to_log(&evt);
                telemetry_logs.update(|logs| {
                    logs.push(entry);
                    if logs.len() > 300 {
                        let drop_n = logs.len() - 300;
                        logs.drain(0..drop_n);
                    }
                });

                let mut needs_refresh = false;
                let mut is_terminal = false;
                active_session.update(|slot| {
                    if let Some(sess) = slot.as_mut() {
                        if sess.id == session_id {
                            needs_refresh = api_client::apply_live_event(sess, &evt);
                            is_terminal = matches!(
                                sess.stage,
                                TournamentStage::Completed | TournamentStage::Failed
                            );
                        }
                    }
                });

                if let LiveEvent::TournamentCompleted {
                    recommended_strategy_id,
                    ..
                } = &evt
                {
                    if let Some(rec) = recommended_strategy_id {
                        selected_strategy_id.set(Some(rec.clone()));
                    }
                    sse_status.set("SYNCED".to_string());
                }

                if needs_refresh || is_terminal {
                    sync_full_session(session_id);
                    refresh_sessions();
                }
            },
            move |err_msg: String| {
                let stage_done = active_session
                    .get_untracked()
                    .map(|s| {
                        matches!(
                            s.stage,
                            TournamentStage::Completed | TournamentStage::Failed
                        )
                    })
                    .unwrap_or(false);
                if stage_done {
                    sse_status.set("SYNCED".to_string());
                    sse_handle.update_value(|slot| {
                        let _ = slot.take();
                    });
                } else {
                    sse_status.set("RECONNECTING".to_string());
                    telemetry_logs.update(|logs| {
                        logs.push(TelemetryLogEntry {
                            timestamp: chrono::Utc::now(),
                            level: LogLevel::Warn,
                            source: "sse".to_string(),
                            message: err_msg,
                        });
                    });
                }
            },
        );

        match sub {
            Ok(handle) => sse_handle.set_value(Some(handle)),
            Err(err) => {
                sse_status.set("OFFLINE".to_string());
                session_error.set(Some(err));
            }
        }
    };

    let load_session_by_id = move |session_id: Uuid| {
        loading_session.set(true);
        session_error.set(None);
        telemetry_logs.set(Vec::new());
        selected_strategy_id.set(None);
        show_intake.set(false);

        spawn_local(async move {
            match api_client::get_session(session_id).await {
                Ok(sess) => {
                    let rec = sess
                        .arena_summary
                        .as_ref()
                        .and_then(|s| s.recommended_strategy_id.clone())
                        .or_else(|| sess.candidates.first().map(|c| c.strategy_id.clone()));
                    let is_done = matches!(
                        sess.stage,
                        TournamentStage::Completed | TournamentStage::Failed
                    );
                    active_session.set(Some(sess));
                    selected_strategy_id.set(rec);
                    loading_session.set(false);
                    if is_done {
                        sse_handle.update_value(|slot| {
                            let _ = slot.take();
                        });
                        sse_status.set("SYNCED".to_string());
                    } else {
                        connect_sse(session_id);
                    }
                }
                Err(err) => {
                    loading_session.set(false);
                    session_error.set(Some(err));
                }
            }
        });
    };

    let on_session_created = move |sess: TournamentSession| {
        let session_id = sess.id;
        telemetry_logs.set(vec![TelemetryLogEntry {
            timestamp: chrono::Utc::now(),
            level: LogLevel::Info,
            source: "studio".to_string(),
            message: format!("Session {session_id} created — subscribing to live SSE stream."),
        }]);
        selected_strategy_id.set(None);
        active_session.set(Some(sess));
        show_intake.set(false);
        show_telemetry.set(true);
        session_error.set(None);
        connect_sse(session_id);
        refresh_sessions();
    };

    // Initial server health + recent sessions fetch on mount.
    Effect::new(move |_| {
        spawn_local(async move {
            match api_client::fetch_server_status().await {
                Ok(st) => {
                    server_status.set(Some(st));
                    server_err.set(None);
                }
                Err(e) => {
                    server_err.set(Some(e));
                }
            }
            if let Ok(items) = api_client::list_sessions().await {
                sessions_list.set(items);
            }
        });
    });

    view! {
        <div class="epicenter-app">
            // Top Header Bar
            <header class="studio-header">
                <div class="studio-header-inner">
                    <div class="brand-cluster">
                        <div class="brand-emblem" aria-hidden="true">
                            <svg width="24" height="24" viewBox="0 0 24 24" fill="none">
                                <circle cx="12" cy="12" r="9.5" stroke="#38bdf8" stroke-opacity="0.35" stroke-width="1.2" stroke-dasharray="2.5 2.5" />
                                <circle cx="12" cy="12" r="5.5" stroke="#38bdf8" stroke-opacity="0.65" stroke-width="1.4" />
                                <circle cx="12" cy="12" r="2.2" fill="#10b981" />
                            </svg>
                        </div>
                        <div class="brand-title-group">
                            <span class="brand-wordmark">"EPICENTER"</span>
                            <span class="brand-sub">"Gemini 4 Tournament & Blast-Radius Console"</span>
                        </div>
                    </div>

                    <div class="model-lineup" aria-label="Active Gemini 4 Model Lineup">
                        <span class="model-pill">
                            <span class="model-dot dot-cyan"></span>
                            <strong>"Hydrogen"</strong>
                            "Intake"
                        </span>
                        <span class="model-pill">
                            <span class="model-dot dot-violet"></span>
                            <strong>"Carbon"</strong>
                            "Architect/Auditor"
                        </span>
                        <span class="model-pill">
                            <span class="model-dot dot-emerald"></span>
                            <strong>"Neon"</strong>
                            "Sandboxes"
                        </span>
                        <span class="model-pill">
                            <span class="model-dot dot-amber"></span>
                            <strong>"Flash TTS"</strong>
                            "Briefing"
                        </span>
                    </div>

                    <div class="header-right">
                        <div class="status-pills">
                            {move || match server_status.get() {
                                Some(st) => view! {
                                    <>
                                        <span class="status-chip ok">
                                            <span class="model-dot dot-emerald"></span>
                                            {format!("v{}", st.version)}
                                        </span>
                                        <span class=if st.has_gemini_key { "status-chip ok" } else { "status-chip warn" }>
                                            {if st.has_gemini_key { "Gemini Live" } else { "No Gemini Key" }}
                                        </span>
                                        <span class=if st.has_github_token { "status-chip ok" } else { "status-chip" }>
                                            {if st.has_github_token { "GitHub PR Ready" } else { "Export Only" }}
                                        </span>
                                    </>
                                }.into_any(),
                                None => view! {
                                    <span class=if server_err.get().is_some() { "status-chip err" } else { "status-chip" }>
                                        {move || if server_err.get().is_some() { "Server Offline" } else { "Connecting..." }}
                                    </span>
                                }.into_any(),
                            }}
                        </div>

                        <nav class="tab-toggle" aria-label="Studio Mode">
                            <button
                                type="button"
                                class=move || if active_tab.get() == StudioTab::MissionControl { "tab-btn active" } else { "tab-btn" }
                                on:click=move |_| active_tab.set(StudioTab::MissionControl)
                            >
                                "Mission Control"
                            </button>
                            <button
                                type="button"
                                class=move || if active_tab.get() == StudioTab::FiveSigmaEval { "tab-btn active" } else { "tab-btn" }
                                on:click=move |_| active_tab.set(StudioTab::FiveSigmaEval)
                            >
                                "5 Sigma Benchmark & Live Judge"
                            </button>
                        </nav>
                    </div>
                </div>
            </header>

            // Main Studio Surface
            <main class="studio-main">
                {move || match active_tab.get() {
                    StudioTab::FiveSigmaEval => view! {
                        <FiveSigmaEvalView />
                    }.into_any(),
                    StudioTab::MissionControl => view! {
                        <>
                            // Recent Sessions Selector & New Tournament Toggle Bar
                            <section class="panel session-bar">
                                <div class="session-bar-left">
                                    <span class="field-label" style="margin:0;white-space:nowrap;">"Recent Sessions"</span>
                                    <select
                                        class="select mono session-select"
                                        prop:value=move || {
                                            active_session
                                                .get()
                                                .map(|s| s.id.to_string())
                                                .unwrap_or_default()
                                        }
                                        on:change=move |ev| {
                                            let val = event_target_value(&ev);
                                            if let Ok(id) = Uuid::parse_str(&val) {
                                                load_session_by_id(id);
                                            }
                                        }
                                    >
                                        <option value="">"— Select a persisted tournament session —"</option>
                                        {move || {
                                            sessions_list
                                                .get()
                                                .into_iter()
                                                .map(|item| {
                                                    let id_str = item.id.to_string();
                                                    let label = format!(
                                                        "[{}] {} · {} cand · {}",
                                                        stage_label(item.stage),
                                                        item.title,
                                                        item.candidate_count,
                                                        item.created_at.format("%H:%M:%S")
                                                    );
                                                    view! { <option value=id_str>{label}</option> }
                                                })
                                                .collect_view()
                                        }}
                                    </select>
                                    <button
                                        type="button"
                                        class="btn btn-sm"
                                        on:click=move |_| refresh_sessions()
                                    >
                                        "Refresh"
                                    </button>
                                </div>

                                <div style="display:flex;align-items:center;gap:8px;flex-wrap:wrap;">
                                    {move || active_session.get().map(|sess| view! {
                                        <span class="badge badge-cyan">
                                            {format!("ID {}", &sess.id.to_string()[..8])}
                                        </span>
                                    })}
                                    <button
                                        type="button"
                                        class=move || if show_intake.get() { "btn btn-sm" } else { "btn btn-primary btn-sm" }
                                        on:click=move |_| show_intake.update(|v| *v = !*v)
                                    >
                                        {move || if show_intake.get() { "Hide Intake Deck" } else { "+ New Tournament" }}
                                    </button>
                                </div>
                            </section>

                            {move || session_error.get().map(|err| view! {
                                <div class="alert-banner err">{err}</div>
                            })}

                            // Collapsible Multimodal Intake Deck
                            {move || show_intake.get().then(|| view! {
                                <MultimodalIntakeDeck
                                    on_session_created=Callback::new(on_session_created)
                                />
                            })}

                            // Live 7-Stage Progress Seismograph Bar + Telemetry Drawer
                            {move || active_session.get().map(|sess| {
                                let current_stage = sess.stage;
                                let current_rank = stage_rank(current_stage);
                                let is_failed = current_stage == TournamentStage::Failed;
                                let is_done = current_stage == TournamentStage::Completed;
                                let kind_label = match sess.intake.task_kind {
                                    TaskKind::Bug => "BUG",
                                    TaskKind::Feature => "FEATURE",
                                };
                                let repo_display = sess.intake.repo_url.clone();
                                let title_display = sess.intake.title.clone();
                                let cand_count = sess.candidates.len();

                                view! {
                                    <section class="panel">
                                        <div class="seismograph-wrap">
                                            <div class="seismograph-top">
                                                <div class="seismograph-meta">
                                                    <span class="badge badge-cyan">{kind_label}</span>
                                                    <strong style="font-size:14px;color:#f8fafc;">{title_display}</strong>
                                                    <span class="mono" style="font-size:11.5px;color:var(--text-muted);">
                                                        {repo_display}
                                                    </span>
                                                    <span class="badge">
                                                        {format!("{cand_count} candidates")}
                                                    </span>
                                                </div>

                                                <div style="display:flex;align-items:center;gap:8px;flex-wrap:wrap;">
                                                    <span class=move || {
                                                        match sse_status.get().as_str() {
                                                            "LIVE SSE" => "badge badge-emerald",
                                                            "RECONNECTING" => "badge badge-amber",
                                                            "OFFLINE" => "badge badge-red",
                                                            _ => "badge badge-cyan",
                                                        }
                                                    }>
                                                        {move || sse_status.get()}
                                                    </span>
                                                    <button
                                                        type="button"
                                                        class="btn btn-sm"
                                                        on:click=move |_| show_telemetry.update(|v| *v = !*v)
                                                    >
                                                        {move || format!(
                                                            "{} Telemetry ({})",
                                                            if show_telemetry.get() { "Hide" } else { "Show" },
                                                            telemetry_logs.get().len()
                                                        )}
                                                    </button>
                                                    <button
                                                        type="button"
                                                        class="btn btn-ghost btn-sm"
                                                        on:click=move |_| telemetry_logs.set(Vec::new())
                                                    >
                                                        "Clear"
                                                    </button>
                                                </div>
                                            </div>

                                            <div class="seismograph-stages">
                                                {PIPELINE_STAGES.into_iter().enumerate().map(|(idx, (_st, label))| {
                                                    let cls = if is_failed && idx == current_rank {
                                                        "stage-step failed"
                                                    } else if is_done || idx < current_rank {
                                                        "stage-step done"
                                                    } else if idx == current_rank {
                                                        "stage-step active"
                                                    } else {
                                                        "stage-step"
                                                    };
                                                    let marker = if is_failed && idx == current_rank {
                                                        "!".to_string()
                                                    } else if is_done || idx < current_rank {
                                                        "✓".to_string()
                                                    } else {
                                                        format!("{}", idx + 1)
                                                    };
                                                    view! {
                                                        <div class=cls>
                                                            <span class="stage-index">{marker}</span>
                                                            <span class="stage-name">{label}</span>
                                                        </div>
                                                    }
                                                }).collect_view()}
                                            </div>
                                        </div>

                                        {move || show_telemetry.get().then(|| {
                                            let entries = telemetry_logs.get();
                                            view! {
                                                <div class="telemetry-drawer">
                                                    {if entries.is_empty() {
                                                        view! {
                                                            <div style="color:var(--text-dim);">
                                                                "Listening for streaming Gemini 4 agent telemetry frames..."
                                                            </div>
                                                        }.into_any()
                                                    } else {
                                                        view! {
                                                            <div>
                                                                {entries.into_iter().rev().map(|entry| {
                                                                    let ts = entry.timestamp.format("%H:%M:%S").to_string();
                                                                    let lvl_cls = log_level_class(entry.level);
                                                                    let lvl_txt = log_level_text(entry.level);
                                                                    view! {
                                                                        <div class="telemetry-row">
                                                                            <span class="log-ts">{ts}</span>
                                                                            <span class=lvl_cls>{lvl_txt}</span>
                                                                            <span class="log-src">{entry.source}</span>
                                                                            <span class="log-msg">{entry.message}</span>
                                                                        </div>
                                                                    }
                                                                }).collect_view()}
                                                            </div>
                                                        }.into_any()
                                                    }}
                                                </div>
                                            }
                                        })}
                                    </section>
                                }
                            })}

                            {move || loading_session.get().then(|| view! {
                                <div class="alert-banner">
                                    "Loading tournament session state from epicenter-server..."
                                </div>
                            })}

                            // Split-Screen Analysis Studio: Ripple Map + Pareto/Arena Panel
                            <section class="analysis-split">
                                <div class="panel viz-host">
                                    <div class="panel-header">
                                        <div>
                                            <h2 class="panel-title">
                                                <span class="model-dot dot-cyan"></span>
                                                "Concentric Blast-Radius Ripple Map"
                                            </h2>
                                            <div class="panel-subtitle">
                                                "Ring 0 mutated epicenter -> Ring 1 direct callers -> Ring 2 entrypoints & Dark Zones"
                                            </div>
                                        </div>
                                    </div>
                                    <div class="panel-body">
                                        <BlastRadiusRippleMap
                                            session=active_session_signal
                                            selected_strategy_id=selected_strategy_id
                                        />
                                    </div>
                                </div>

                                <div class="panel viz-host">
                                    <div class="panel-header">
                                        <div>
                                            <h2 class="panel-title">
                                                <span class="model-dot dot-amber"></span>
                                                "Dual-Score Pareto Frontier & Arena Benchmarks"
                                            </h2>
                                            <div class="panel-subtitle">
                                                "Click any strategy card or Pareto point to inspect its ripple map, diff, and hazards"
                                            </div>
                                        </div>
                                    </div>
                                    <div class="panel-body">
                                        <ParetoAndArenaPanel
                                            session=active_session_signal
                                            selected_strategy_id=selected_strategy_id
                                        />
                                    </div>
                                </div>
                            </section>

                            // Bottom Deck: 2-Speaker War-Room TTS Audio + Unified Diff + Export / 1-Click PR
                            <BriefingAndHandoffDeck
                                session=active_session_signal
                                selected_strategy_id=selected_strategy_id
                                has_github_token=has_github_token_signal
                            />
                        </>
                    }.into_any(),
                }}
            </main>
        </div>
    }
}
