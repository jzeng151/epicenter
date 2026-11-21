use crate::api_client::{self, TelemetryLogEntry};
use epicenter_common::{
    CandidateBadge, CandidateResult, CreatePrRequest, CreatePrResponse, DialogueSpeaker, LogLevel,
    RedGateResolution, RedGateSpec, StrategyDepthTier, TournamentSession, TournamentStage,
};
use leptos::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::{spawn_local, JsFuture};
use web_sys::{Blob, BlobPropertyBag, HtmlAnchorElement, Url};

fn trigger_text_download(filename: &str, content: &str, mime_type: &str) -> Result<(), String> {
    let window = web_sys::window().ok_or_else(|| "Browser window unavailable".to_string())?;
    let document = window
        .document()
        .ok_or_else(|| "Browser document unavailable".to_string())?;

    let parts = js_sys::Array::of1(&wasm_bindgen::JsValue::from_str(content));
    let opts = BlobPropertyBag::new();
    opts.set_type(mime_type);
    let blob = Blob::new_with_str_sequence_and_options(&parts, &opts)
        .map_err(|e| format!("Failed to construct download Blob: {e:?}"))?;

    let object_url = Url::create_object_url_with_blob(&blob)
        .map_err(|e| format!("Failed to create object URL: {e:?}"))?;

    let el = document
        .create_element("a")
        .map_err(|e| format!("Failed to create anchor element: {e:?}"))?;
    let anchor: HtmlAnchorElement = el
        .dyn_into()
        .map_err(|_| "Created element is not an HtmlAnchorElement".to_string())?;
    anchor.set_href(&object_url);
    anchor.set_download(filename);
    anchor.click();

    let _ = Url::revoke_object_url(&object_url);
    Ok(())
}

fn copy_to_clipboard(text: String, on_done: impl FnOnce(Result<(), String>) + 'static) {
    spawn_local(async move {
        let Some(window) = web_sys::window() else {
            on_done(Err("Browser window unavailable".to_string()));
            return;
        };
        let clipboard = window.navigator().clipboard();
        let promise = clipboard.write_text(&text);
        match JsFuture::from(promise).await {
            Ok(_) => on_done(Ok(())),
            Err(e) => on_done(Err(format!("Clipboard write denied: {e:?}"))),
        }
    });
}

fn resolve_active_candidate<'a>(
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
        .find(|c| c.is_green())
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

fn red_gate_badge_meta(resolution: RedGateResolution) -> (&'static str, &'static str) {
    match resolution {
        RedGateResolution::VerifiedNeon => ("Repro Test Verified", "badge badge-emerald"),
        RedGateResolution::EscalatedCarbon => ("Repro Test (Escalated)", "badge badge-amber"),
        RedGateResolution::CharacterizationFallback => {
            ("Baseline Test", "badge badge-violet")
        }
    }
}

fn badge_style(badge: CandidateBadge) -> &'static str {
    match badge {
        CandidateBadge::BestOverall => "badge badge-emerald",
        CandidateBadge::SafestEmergencyHotfix => "badge badge-cyan",
        CandidateBadge::FastestThroughput => "badge badge-amber",
    }
}

fn badge_label(badge: CandidateBadge) -> &'static str {
    match badge {
        CandidateBadge::BestOverall => "★ Best Overall",
        CandidateBadge::SafestEmergencyHotfix => "◆ Safest Fix",
        CandidateBadge::FastestThroughput => "⚡ Fastest",
    }
}

fn truncate_label(s: &str, max: usize) -> String {
    let mut it = s.chars();
    let out: String = it.by_ref().take(max).collect();
    if it.next().is_some() {
        format!("{out}…")
    } else {
        out
    }
}

fn build_fallback_audit_markdown(session: &TournamentSession, cand: &CandidateResult) -> String {
    let risk = cand
        .blast_radius
        .as_ref()
        .map(|b| format!("{:.1}/100", b.composite_risk_score))
        .unwrap_or_else(|| "n/a".to_string());
    let depth = cand
        .blast_radius
        .as_ref()
        .map(|b| format!("{:.1}/100", b.root_cause_depth_score))
        .unwrap_or_else(|| "n/a".to_string());
    let speedup = cand
        .arena_metrics
        .as_ref()
        .map(|m| format!("{:+.1}%", m.speedup_pct))
        .unwrap_or_else(|| "n/a".to_string());
    format!(
        "# EPICENTER Blast-Radius Audit\n\n- **Session**: `{}` ({})\n- **Repository**: {}\n- **Strategy**: `{}`\n- **Tests**: repro={}, regression={}\n- **Impact Risk**: {}\n- **Root-Cause Depth**: {}\n- **Arena Speedup**: {}\n",
        session.id,
        session.intake.title,
        session.intake.repo_url,
        cand.strategy_id,
        cand.repro_passed,
        cand.regression_suite_passed,
        risk,
        depth,
        speedup
    )
}

#[component]
pub fn BriefingAndHandoffDeck(
    session: Signal<Option<TournamentSession>>,
    selected_strategy_id: RwSignal<Option<String>>,
    has_github_token: Signal<bool>,
    #[prop(optional)] telemetry_logs: Option<Signal<Vec<TelemetryLogEntry>>>,
) -> impl IntoView {
    let action_busy = RwSignal::new(Option::<String>::None);
    let action_notice = RwSignal::new(Option::<String>::None);
    let action_error = RwSignal::new(Option::<String>::None);
    let created_pr = RwSignal::new(Option::<(String, CreatePrResponse)>::None);

    let on_download_patch = move |_| {
        let Some(sess) = session.get_untracked() else {
            return;
        };
        let sel = selected_strategy_id.get_untracked();
        let Some(cand) = resolve_active_candidate(&sess, sel.as_deref()).cloned() else {
            return;
        };
        let session_id = sess.id;
        let strategy_id = cand.strategy_id.clone();

        action_busy.set(Some("patch".to_string()));
        action_error.set(None);
        action_notice.set(None);

        spawn_local(async move {
            let (filename, diff_text) =
                match api_client::export_candidate_bundle(session_id, &strategy_id).await {
                    Ok(bundle) => (bundle.patch_filename, bundle.git_diff),
                    Err(_) => (format!("epicenter-{strategy_id}.patch"), cand.git_diff),
                };
            if diff_text.trim().is_empty() {
                action_error.set(Some("Candidate has no unified git diff yet.".to_string()));
            } else {
                match trigger_text_download(&filename, &diff_text, "text/x-diff;charset=utf-8") {
                    Ok(()) => action_notice.set(Some(format!("Downloaded {filename}"))),
                    Err(e) => action_error.set(Some(e)),
                }
            }
            action_busy.set(None);
        });
    };

    let on_download_audit = move |_| {
        let Some(sess) = session.get_untracked() else {
            return;
        };
        let sel = selected_strategy_id.get_untracked();
        let Some(cand) = resolve_active_candidate(&sess, sel.as_deref()).cloned() else {
            return;
        };
        let session_id = sess.id;
        let strategy_id = cand.strategy_id.clone();

        action_busy.set(Some("audit".to_string()));
        action_error.set(None);
        action_notice.set(None);

        spawn_local(async move {
            let audit_md =
                match api_client::export_candidate_bundle(session_id, &strategy_id).await {
                    Ok(bundle) if !bundle.audit_markdown.trim().is_empty() => bundle.audit_markdown,
                    _ => build_fallback_audit_markdown(&sess, &cand),
                };
            match trigger_text_download(
                "BLAST_RADIUS_AUDIT.md",
                &audit_md,
                "text/markdown;charset=utf-8",
            ) {
                Ok(()) => action_notice.set(Some("Downloaded BLAST_RADIUS_AUDIT.md".to_string())),
                Err(e) => action_error.set(Some(e)),
            }
            action_busy.set(None);
        });
    };

    let on_copy_git_apply = move |_| {
        let Some(sess) = session.get_untracked() else {
            return;
        };
        let sel = selected_strategy_id.get_untracked();
        let Some(cand) = resolve_active_candidate(&sess, sel.as_deref()).cloned() else {
            return;
        };
        let session_id = sess.id;
        let strategy_id = cand.strategy_id.clone();

        action_busy.set(Some("copy".to_string()));
        action_error.set(None);
        action_notice.set(None);

        spawn_local(async move {
            let cmd = match api_client::export_candidate_bundle(session_id, &strategy_id).await {
                Ok(bundle) if !bundle.git_apply_command.trim().is_empty() => {
                    bundle.git_apply_command
                }
                _ => format!("git apply --3way epicenter-{strategy_id}.patch"),
            };
            copy_to_clipboard(cmd.clone(), move |res| {
                match res {
                    Ok(()) => action_notice.set(Some(format!("Copied command: {cmd}"))),
                    Err(e) => action_error.set(Some(e)),
                }
                action_busy.set(None);
            });
        });
    };

    let on_open_github_pr = move |_| {
        let Some(sess) = session.get_untracked() else {
            return;
        };
        let sel = selected_strategy_id.get_untracked();
        let Some(cand) = resolve_active_candidate(&sess, sel.as_deref()).cloned() else {
            return;
        };
        let session_id = sess.id;
        let strategy_id = cand.strategy_id.clone();

        action_busy.set(Some("pr".to_string()));
        action_error.set(None);
        action_notice.set(None);

        spawn_local(async move {
            let req = CreatePrRequest {
                title: None,
                target_branch: None,
            };
            match api_client::create_candidate_pr(session_id, &strategy_id, &req).await {
                Ok(resp) => {
                    action_notice.set(Some(format!(
                        "Opened GitHub PR #{} on branch {}",
                        resp.number, resp.branch_name
                    )));
                    created_pr.set(Some((strategy_id, resp)));
                }
                Err(err) => {
                    action_error.set(Some(format!("GitHub PR creation failed: {err}")));
                }
            }
            action_busy.set(None);
        });
    };

    view! {
        <section class="panel" aria-labelledby="briefing-heading">
            <div class="panel-header">
                <div>
                    <h2 id="briefing-heading" class="panel-title">
                        <span class="model-dot dot-emerald" aria-hidden="true"></span>
                        "Summary, Diff & Export"
                    </h2>
                    <div class="panel-subtitle">
                        "Review the audio walkthrough, inspect code changes, or export the patch."
                    </div>
                </div>
            </div>

            <div class="panel-body">
                {move || match session.get() {
                    None => view! {
                        <div class="empty-state">
                            "Start or select a comparison to review the audio walkthrough, code diff, and export options."
                        </div>
                    }.into_any(),
                    Some(sess) => {
                        let is_failed = sess.stage == TournamentStage::Failed;
                        let sel_id = selected_strategy_id.get();
                        let active_cand = resolve_active_candidate(&sess, sel_id.as_deref()).cloned();
                        let strategy_specs = sess
                            .strategy_plan
                            .as_ref()
                            .map(|p| p.strategies.clone())
                            .unwrap_or_default();
                        let active_spec = active_cand.as_ref().and_then(|c| {
                            strategy_specs
                                .iter()
                                .find(|s| s.id == c.strategy_id)
                                .cloned()
                        });
                        let audio_url = sess
                            .audio_briefing_url
                            .as_deref()
                            .map(api_client::api_url);
                        let script = sess.war_room_script.clone();
                        let red_gate = sess.red_gate.clone();
                        let candidates_list = sess.candidates.clone();
                        let current_strategy_key = active_cand
                            .as_ref()
                            .map(|c| c.strategy_id.clone())
                            .unwrap_or_default();

                        let failed_red_gate_excerpt = match &sess.red_gate {
                            Some(RedGateSpec::Bug(bug)) if !bug.failure_excerpt.trim().is_empty() => {
                                Some(format!(
                                    "Repro test command: $ {}\nConfirmed failing before fix: {}\n\n{}",
                                    bug.test_command,
                                    bug.verified_fails_on_head,
                                    bug.failure_excerpt.trim()
                                ))
                            }
                            _ => None,
                        };
                        let failed_candidate_excerpt = sess
                            .candidates
                            .iter()
                            .find(|c| !c.test_output_tail.trim().is_empty())
                            .map(|c| {
                                format!(
                                    "Candidate '{}' test output:\n{}",
                                    c.strategy_id,
                                    c.test_output_tail.trim()
                                )
                            });
                        let failed_telemetry_excerpt = if is_failed {
                            telemetry_logs.and_then(|sig| {
                                let lines: Vec<String> = sig
                                    .get()
                                    .into_iter()
                                    .filter(|e| {
                                        matches!(e.level, LogLevel::Error | LogLevel::Warn)
                                            || e.source.contains("failed")
                                            || e.source.contains("repro")
                                            || e.source.contains("red-gate")
                                    })
                                    .rev()
                                    .take(8)
                                    .collect::<Vec<_>>()
                                    .into_iter()
                                    .rev()
                                    .map(|e| {
                                        format!(
                                            "[{}] [{}] {}",
                                            e.timestamp.format("%H:%M:%S"),
                                            e.source,
                                            e.message
                                        )
                                    })
                                    .collect();
                                (!lines.is_empty()).then(|| lines.join("\n"))
                            })
                        } else {
                            None
                        };

                        let diagnostic_excerpt = if is_failed {
                            let mut parts = Vec::new();
                            if let Some(t) = failed_telemetry_excerpt {
                                parts.push(t);
                            }
                            if let Some(rg) = failed_red_gate_excerpt {
                                parts.push(rg);
                            }
                            if let Some(c) = failed_candidate_excerpt {
                                parts.push(c);
                            }
                            (!parts.is_empty()).then(|| parts.join("\n\n---\n\n"))
                        } else {
                            None
                        };

                        view! {
                            {is_failed.then(|| view! {
                                <div class="honest-halt-banner" role="alert">
                                    <div class="honest-halt-head">
                                        <div style="display:flex;align-items:center;gap:8px;flex-wrap:wrap;">
                                            <span class="badge badge-red">"STOPPED"</span>
                                            <strong class="honest-halt-title">
                                                "Comparison Stopped Before Completion"
                                            </strong>
                                        </div>
                                    </div>
                                    <p class="honest-halt-body">
                                        "This comparison stopped because a verification step failed (for example, the bug could not be reproduced with a failing test, or no candidate passed the test suite). See the captured output below for details."
                                    </p>
                                    {diagnostic_excerpt.map(|excerpt| view! {
                                        <div style="margin-top:10px;">
                                            <span class="field-label" style="color:#fca5a5;">
                                                "Captured Error Details"
                                            </span>
                                            <pre class="terminal-box" tabindex="0" style="border-color:rgba(239,68,68,0.45);">{excerpt}</pre>
                                        </div>
                                    })}
                                </div>
                            })}

                            <div class="briefing-grid">
                                // Left Column: Audio Walkthrough + Transcript
                                <div>
                                    <div class="audio-player-box">
                                        <div style="display:flex;align-items:center;justify-content:space-between;gap:8px;flex-wrap:wrap;">
                                            <strong style="font-size:13.5px;">"Audio Walkthrough"</strong>
                                            <span class="badge badge-cyan">"2-Speaker Summary"</span>
                                        </div>
                                        {match audio_url {
                                            Some(url) => view! {
                                                <audio controls=true preload="metadata" aria-label="2-speaker audio walkthrough" src=url></audio>
                                            }.into_any(),
                                            None => view! {
                                                <div style="margin-top:8px;font-size:12px;color:var(--text-muted);">
                                                    {if is_failed {
                                                        "Audio walkthrough skipped because this comparison stopped early."
                                                    } else {
                                                        "Generating audio walkthrough..."
                                                    }}
                                                </div>
                                            }.into_any(),
                                        }}
                                    </div>

                                    <div style="margin-bottom:8px;display:flex;align-items:center;justify-content:space-between;">
                                        <span class="field-label" style="margin:0;">"Transcript"</span>
                                        <span class="mono" style="font-size:11px;color:var(--text-dim);">
                                            {format!("{} turns", script.len())}
                                        </span>
                                    </div>

                                    {if script.is_empty() {
                                        view! {
                                            <div class="empty-state" style="padding:20px 14px;">
                                                {if is_failed {
                                                    "No transcript generated because this comparison stopped early."
                                                } else {
                                                    "Architect & Auditor walkthrough transcript will appear once auditing finishes."
                                                }}
                                            </div>
                                        }.into_any()
                                    } else {
                                        view! {
                                            <div class="dialogue-feed" tabindex="0" role="region" aria-label="Audio walkthrough transcript">
                                                {script.into_iter().map(|turn| {
                                                    let is_arch = turn.speaker == DialogueSpeaker::Architect;
                                                    let cls = if is_arch {
                                                        "dialogue-turn architect"
                                                    } else {
                                                        "dialogue-turn auditor"
                                                    };
                                                    let speaker_label = turn.speaker.label();
                                                    view! {
                                                        <div class=cls>
                                                            <div class="dialogue-speaker">
                                                                <span>{speaker_label}</span>
                                                            </div>
                                                            <p class="dialogue-text">{turn.text}</p>
                                                        </div>
                                                    }
                                                }).collect_view()}
                                            </div>
                                        }.into_any()
                                    }}
                                </div>

                                // Right Column: Repro Test + Export + Code Diff + Test Output
                                <div>
                                    {red_gate.map(|rg| match rg {
                                        RedGateSpec::Bug(bug) => {
                                            let (rg_label, rg_cls) = red_gate_badge_meta(bug.resolution);
                                            view! {
                                                <div class="redgate-card">
                                                    <div style="display:flex;align-items:center;justify-content:space-between;gap:8px;flex-wrap:wrap;">
                                                        <div style="display:flex;align-items:center;gap:8px;flex-wrap:wrap;">
                                                            <span class=rg_cls>{rg_label}</span>
                                                            <span class=if bug.verified_fails_on_head { "badge badge-emerald" } else { "badge badge-amber" }>
                                                                {if bug.verified_fails_on_head { "Confirmed Failing Before Fix" } else { "Unverified Failure" }}
                                                            </span>
                                                        </div>
                                                        <span class="mono" style="font-size:11px;color:var(--text-muted);">
                                                            {bug.repro_test_path}
                                                        </span>
                                                    </div>
                                                    <div class="mono" style="font-size:11.5px;color:var(--text-muted);">
                                                        {format!("$ {}", bug.test_command)}
                                                    </div>
                                                    {(!bug.failure_excerpt.trim().is_empty()).then(|| view! {
                                                        <pre class="terminal-box" tabindex="0" style="max-height:84px;">{bug.failure_excerpt}</pre>
                                                    })}
                                                </div>
                                            }.into_any()
                                        }
                                        RedGateSpec::Feature(feat) => view! {
                                            <div class="redgate-card">
                                                <div style="display:flex;align-items:center;gap:8px;flex-wrap:wrap;">
                                                    <span class="badge badge-cyan">"Feature Acceptance Test"</span>
                                                    <span class="mono" style="font-size:11.5px;color:var(--text-muted);">
                                                        {format!("$ {}", feat.test_command)}
                                                    </span>
                                                </div>
                                                <div style="font-size:12px;color:var(--text-muted);">
                                                    {feat.benchmark_entrypoint_contract}
                                                </div>
                                            </div>
                                        }.into_any(),
                                    })}

                                    {(candidates_list.len() > 1).then(|| {
                                        let active_id_cmp = current_strategy_key.clone();
                                        let specs_for_btns = strategy_specs.clone();
                                        view! {
                                            <div role="group" aria-label="Select candidate approach" style="display:flex;gap:6px;flex-wrap:wrap;margin-bottom:12px;">
                                                {candidates_list.into_iter().map(|c| {
                                                    let cid = c.strategy_id.clone();
                                                    let cid_click = cid.clone();
                                                    let is_sel = cid == active_id_cmp;
                                                    let green = c.is_green();
                                                    let label = specs_for_btns
                                                        .iter()
                                                        .find(|s| s.id == cid)
                                                        .map(|s| truncate_label(&s.title, 26))
                                                        .unwrap_or_else(|| cid.clone());
                                                    let btn_cls = if is_sel {
                                                        "btn btn-sm btn-primary"
                                                    } else {
                                                        "btn btn-sm"
                                                    };
                                                    view! {
                                                        <button
                                                            type="button"
                                                            class=btn_cls
                                                            aria-pressed=is_sel.to_string()
                                                            on:click=move |_| selected_strategy_id.set(Some(cid_click.clone()))
                                                        >
                                                            <span class=if green { "model-dot dot-emerald" } else { "model-dot dot-amber" } aria-hidden="true"></span>
                                                            <span>{label}</span>
                                                            <span class="sr-only">{if green { " (Passed)" } else { " (Failed)" }}</span>
                                                        </button>
                                                    }
                                                }).collect_view()}
                                            </div>
                                        }
                                    })}

                                    {match active_cand {
                                        None => view! {
                                            <div class="empty-state">
                                                {if is_failed {
                                                    "No candidate patches produced because this comparison stopped early."
                                                } else {
                                                    "Waiting for candidate patches..."
                                                }}
                                            </div>
                                        }.into_any(),
                                        Some(cand) => {
                                            let cand_id = cand.strategy_id.clone();
                                            let cand_id_pr_match = cand_id.clone();
                                            let heading_title = active_spec
                                                .as_ref()
                                                .map(|s| s.title.clone())
                                                .unwrap_or_else(|| cand_id.clone());
                                            let diff_lines: Vec<String> = cand
                                                .git_diff
                                                .lines()
                                                .map(|l| l.to_string())
                                                .collect();
                                            let added_count = diff_lines
                                                .iter()
                                                .filter(|l| l.starts_with('+') && !l.starts_with("+++"))
                                                .count();
                                            let removed_count = diff_lines
                                                .iter()
                                                .filter(|l| l.starts_with('-') && !l.starts_with("---"))
                                                .count();

                                            view! {
                                                <div>
                                                    <div style="display:flex;align-items:center;justify-content:space-between;gap:10px;flex-wrap:wrap;margin-bottom:8px;">
                                                        <div style="display:flex;align-items:center;gap:8px;flex-wrap:wrap;">
                                                            <strong style="font-size:14px;color:#f8fafc;">{heading_title}</strong>
                                                            {active_spec.as_ref().map(|spec| view! {
                                                                <span class="badge badge-cyan">{depth_tier_label(spec.depth_tier)}</span>
                                                            })}
                                                            {cand.badges.iter().map(|b| view! {
                                                                <span class=badge_style(*b)>{badge_label(*b)}</span>
                                                            }).collect_view()}
                                                            {cand.pareto_optimal.then(|| view! {
                                                                <span class="badge badge-violet">"Top Trade-Off"</span>
                                                            })}
                                                        </div>
                                                        <div style="display:flex;align-items:center;gap:6px;">
                                                            <span class=if cand.repro_passed { "badge badge-emerald" } else { "badge badge-red" }>
                                                                {if cand.repro_passed { "Repro Passed" } else { "Repro Failed" }}
                                                            </span>
                                                            <span class=if cand.regression_suite_passed { "badge badge-emerald" } else { "badge badge-red" }>
                                                                {if cand.regression_suite_passed { "Suite Passed" } else { "Suite Failed" }}
                                                            </span>
                                                        </div>
                                                    </div>

                                                    {active_spec.as_ref().filter(|s| !s.thesis.trim().is_empty()).map(|spec| view! {
                                                        <div style="font-size:12.5px;color:var(--text-muted);margin-bottom:12px;">
                                                            {spec.thesis.clone()}
                                                        </div>
                                                    })}

                                                    <div class="handoff-toolbar">
                                                        <button
                                                            type="button"
                                                            class="btn btn-sm"
                                                            disabled=move || action_busy.get().is_some()
                                                            on:click=on_download_patch
                                                        >
                                                            "Download .patch"
                                                        </button>
                                                        <button
                                                            type="button"
                                                            class="btn btn-sm"
                                                            disabled=move || action_busy.get().is_some()
                                                            on:click=on_download_audit
                                                        >
                                                            "Download Audit Report"
                                                        </button>
                                                        <button
                                                            type="button"
                                                            class="btn btn-sm"
                                                            disabled=move || action_busy.get().is_some()
                                                            on:click=on_copy_git_apply
                                                        >
                                                            "Copy git apply"
                                                        </button>
                                                        <button
                                                            type="button"
                                                            class="btn btn-emerald btn-sm"
                                                            style="margin-left:auto;"
                                                            title=move || if has_github_token.get() {
                                                                "Create a branch, commit this diff, and open a GitHub PR"
                                                            } else {
                                                                "Configure GITHUB_TOKEN to enable creating GitHub PRs"
                                                            }
                                                            disabled=move || !has_github_token.get() || action_busy.get().is_some()
                                                            on:click=on_open_github_pr
                                                        >
                                                            {move || if action_busy.get().as_deref() == Some("pr") {
                                                                "Opening GitHub PR..."
                                                            } else {
                                                                "Open GitHub PR"
                                                            }}
                                                        </button>
                                                    </div>

                                                    {move || {
                                                        let matching_pr = created_pr.get().and_then(|(sid, pr)| {
                                                            (sid == cand_id_pr_match).then_some(pr)
                                                        });
                                                        matching_pr.map(|pr| view! {
                                                            <div class="alert-banner ok" role="status" aria-live="polite" style="margin-bottom:10px;display:flex;align-items:center;justify-content:space-between;gap:8px;flex-wrap:wrap;">
                                                                <span>
                                                                    {format!("PR #{} created on branch ", pr.number)}
                                                                    <code class="mono">{pr.branch_name}</code>
                                                                </span>
                                                                <a
                                                                    href=pr.pr_url
                                                                    target="_blank"
                                                                    rel="noopener noreferrer"
                                                                    class="btn btn-emerald btn-sm"
                                                                    style="text-decoration:none;"
                                                                >
                                                                    "View PR on GitHub ->"
                                                                    <span class="sr-only">" (opens in a new tab)"</span>
                                                                </a>
                                                            </div>
                                                        })
                                                    }}

                                                    {move || action_error.get().map(|err| view! {
                                                        <div class="alert-banner err" role="alert" style="margin-bottom:10px;">{err}</div>
                                                    })}
                                                    {move || action_notice.get().map(|msg| view! {
                                                        <div class="alert-banner ok" role="status" aria-live="polite" style="margin-bottom:10px;">{msg}</div>
                                                    })}

                                                    <div style="display:flex;align-items:center;justify-content:space-between;margin-bottom:6px;">
                                                        <span class="field-label" style="margin:0;">"Code Diff"</span>
                                                        <span class="mono" style="font-size:11px;">
                                                            <span style="color:#6ee7b7;">{format!("+{added_count}")}</span>
                                                            " / "
                                                            <span style="color:#fca5a5;">{format!("-{removed_count}")}</span>
                                                            <span style="color:var(--text-dim);">{format!(" ({} lines)", diff_lines.len())}</span>
                                                        </span>
                                                    </div>

                                                    {if diff_lines.is_empty() {
                                                        view! {
                                                            <div class="empty-state" style="padding:18px;">
                                                                "No diff produced for this candidate yet."
                                                            </div>
                                                        }.into_any()
                                                    } else {
                                                        view! {
                                                            <div class="diff-container" tabindex="0" role="region" aria-label="Code diff viewer">
                                                                {diff_lines.into_iter().enumerate().map(|(idx, line)| {
                                                                    let cls = if line.starts_with("diff --git")
                                                                        || line.starts_with("index ")
                                                                        || line.starts_with("--- ")
                                                                        || line.starts_with("+++ ")
                                                                    {
                                                                        "diff-line diff-line-meta"
                                                                    } else if line.starts_with("@@") {
                                                                        "diff-line diff-line-hunk"
                                                                    } else if line.starts_with('+') {
                                                                        "diff-line diff-line-add"
                                                                    } else if line.starts_with('-') {
                                                                        "diff-line diff-line-del"
                                                                    } else {
                                                                        "diff-line diff-line-ctx"
                                                                    };
                                                                    view! {
                                                                        <div class=cls>
                                                                            <span class="diff-ln">{idx + 1}</span>
                                                                            <span>{line}</span>
                                                                        </div>
                                                                    }
                                                                }).collect_view()}
                                                            </div>
                                                        }.into_any()
                                                    }}

                                                    {(!cand.test_output_tail.trim().is_empty()).then(|| view! {
                                                        <div style="margin-top:12px;">
                                                            <span class="field-label">"Test Output"</span>
                                                            <pre class="terminal-box" tabindex="0">{cand.test_output_tail}</pre>
                                                        </div>
                                                    })}
                                                </div>
                                            }.into_any()
                                        }
                                    }}
                                </div>
                            </div>
                        }.into_any()
                    }
                }}
            </div>
        </section>
    }
}
