use std::collections::HashMap;

use crate::api_client::{self, KeyframeItem};
use epicenter_common::{AttachmentMeta, MediaKind, MultimodalFinding, TournamentSession};
use leptos::prelude::*;
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;

#[derive(Debug, Clone, PartialEq, Eq)]
enum KeyframeFetchState {
    Loading,
    Ready(Vec<KeyframeItem>),
    Error(String),
}

fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    let b = bytes as f64;
    if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

fn media_kind_meta(kind: MediaKind) -> (&'static str, &'static str) {
    match kind {
        MediaKind::Screenshot => ("IMAGE", "badge badge-cyan"),
        MediaKind::ScreenRecording => ("VIDEO", "badge badge-violet"),
        MediaKind::VoiceNote => ("VOICE", "badge badge-amber"),
        MediaKind::Pdf => ("PDF", "badge badge-emerald"),
        MediaKind::Log => ("LOG", "badge"),
        MediaKind::Other => ("FILE", "badge"),
    }
}

fn render_attachment_card(
    att: AttachmentMeta,
    keyframes_cache: RwSignal<HashMap<Uuid, KeyframeFetchState>>,
) -> impl IntoView {
    let att_id = att.id;
    let content_url = api_client::attachment_content_url(att_id);
    let open_url = content_url.clone();
    let filename = att.filename.clone();
    let filename_title = filename.clone();
    let mime_type = att.mime_type.clone();
    let size_text = format_bytes(att.size_bytes);
    let (kind_label, kind_cls) = media_kind_meta(att.media_kind);
    let has_file_uri = att.file_uri.is_some();

    let media_body = match att.media_kind {
        MediaKind::Screenshot => view! {
            <div class="evidence-media-wrap">
                <a href=open_url.clone() target="_blank" rel="noopener noreferrer" title="Open full-resolution image">
                    <img
                        class="evidence-img"
                        src=content_url
                        alt=filename.clone()
                        loading="lazy"
                    />
                </a>
            </div>
        }
        .into_any(),
        MediaKind::ScreenRecording => view! {
            <div class="evidence-media-wrap">
                <video
                    class="evidence-video"
                    controls=true
                    preload="metadata"
                    src=content_url
                ></video>

                <div class="keyframe-section">
                    <div class="keyframe-section-head">
                        <span class="field-label" style="margin:0;">"Extracted Video Keyframes"</span>
                        {move || match keyframes_cache.get().get(&att_id) {
                            Some(KeyframeFetchState::Ready(frames)) => view! {
                                <span class="badge badge-violet">{format!("{} frames", frames.len())}</span>
                            }.into_any(),
                            Some(KeyframeFetchState::Loading) => view! {
                                <span class="badge">"Extracting..."</span>
                            }.into_any(),
                            _ => view! { <span></span> }.into_any(),
                        }}
                    </div>

                    {move || match keyframes_cache.get().get(&att_id).cloned() {
                        Some(KeyframeFetchState::Loading) | None => view! {
                            <div class="keyframe-status">
                                "Fetching timestamped keyframes from /api/attachments/{id}/keyframes..."
                            </div>
                        }
                        .into_any(),
                        Some(KeyframeFetchState::Error(err)) => view! {
                            <div class="keyframe-status">
                                {format!("Keyframe strip unavailable ({err}) — full video remains playable above.")}
                            </div>
                        }
                        .into_any(),
                        Some(KeyframeFetchState::Ready(frames)) if frames.is_empty() => view! {
                            <div class="keyframe-status">
                                "No discrete keyframes returned for this clip — inspect the full video player above."
                            </div>
                        }
                        .into_any(),
                        Some(KeyframeFetchState::Ready(frames)) => view! {
                            <div class="keyframe-strip">
                                {frames
                                    .into_iter()
                                    .map(|kf| {
                                        let ts = kf.timestamp_hint.clone();
                                        let idx = kf.index;
                                        let alt_text = format!("Keyframe #{idx} at {ts}");
                                        view! {
                                            <div class="keyframe-thumb">
                                                <img src=kf.data_url alt=alt_text loading="lazy" />
                                                <div class="keyframe-meta">
                                                    <span class="keyframe-ts">{ts}</span>
                                                    <span class="mono" style="font-size:10px;color:var(--text-dim);">
                                                        {format!("#{idx}")}
                                                    </span>
                                                </div>
                                            </div>
                                        }
                                    })
                                    .collect_view()}
                            </div>
                        }
                        .into_any(),
                    }}
                </div>
            </div>
        }
        .into_any(),
        MediaKind::VoiceNote => view! {
            <div class="evidence-media-wrap evidence-audio-wrap">
                <audio
                    class="evidence-audio"
                    controls=true
                    preload="metadata"
                    src=content_url
                ></audio>
            </div>
        }
        .into_any(),
        MediaKind::Pdf | MediaKind::Log | MediaKind::Other => view! {
            <div class="evidence-file-card">
                <div style="display:flex;flex-direction:column;gap:2px;min-width:0;">
                    <span class="mono" style="font-size:12px;color:#f1f5f9;word-break:break-all;">
                        {filename.clone()}
                    </span>
                    <span class="mono" style="font-size:11px;color:var(--text-dim);">
                        {format!("{mime_type} · {size_text}")}
                    </span>
                </div>
                <a
                    href=open_url.clone()
                    target="_blank"
                    rel="noopener noreferrer"
                    class="btn btn-sm"
                    style="text-decoration:none;"
                >
                    "Open File ↗"
                </a>
            </div>
        }
        .into_any(),
    };

    view! {
        <div class="evidence-card">
            <div class="evidence-card-head">
                <div style="display:flex;align-items:center;gap:7px;min-width:0;flex-wrap:wrap;">
                    <span class=kind_cls>{kind_label}</span>
                    <span class="mono evidence-filename" title=filename_title>{filename}</span>
                </div>
                <div style="display:flex;align-items:center;gap:8px;flex-shrink:0;">
                    {has_file_uri.then(|| view! {
                        <span class="badge badge-emerald" title="Synced with Gemini Files API">"Files API"</span>
                    })}
                    <span class="mono" style="font-size:11px;color:var(--text-dim);">{size_text}</span>
                    <a
                        href=open_url
                        target="_blank"
                        rel="noopener noreferrer"
                        class="mono"
                        style="font-size:11px;"
                        title="Open raw attachment endpoint"
                    >
                        "Raw ↗"
                    </a>
                </div>
            </div>
            {media_body}
        </div>
    }
}

fn render_finding_card(finding: MultimodalFinding) -> impl IntoView {
    let (kind_label, kind_cls) = media_kind_meta(finding.media_kind);
    let ts_hint = finding
        .timestamp_hint
        .filter(|s| !s.trim().is_empty());
    let symbols = finding.extracted_symbols;

    view! {
        <div class="finding-card">
            <div class="finding-head">
                <div style="display:flex;align-items:center;gap:7px;flex-wrap:wrap;">
                    <span class=kind_cls>{kind_label}</span>
                    <strong class="mono" style="font-size:12px;color:#f8fafc;">
                        {finding.source_name}
                    </strong>
                </div>
                {ts_hint.map(|ts| view! {
                    <span class="badge badge-amber">{format!("@ {ts}")}</span>
                })}
            </div>

            <p class="finding-summary">{finding.summary}</p>

            {(!symbols.is_empty()).then(|| view! {
                <div class="symbol-chips">
                    {symbols
                        .into_iter()
                        .map(|sym| view! {
                            <code class="symbol-chip">{sym}</code>
                        })
                        .collect_view()}
                </div>
            })}
        </div>
    }
}

#[component]
pub fn MultimodalEvidencePanel(session: Signal<Option<TournamentSession>>) -> impl IntoView {
    let keyframes_cache = RwSignal::new(HashMap::<Uuid, KeyframeFetchState>::new());

    let attachments_memo = Memo::new(move |_| {
        session
            .get()
            .map(|s| s.intake.attachments)
            .unwrap_or_default()
    });
    let findings_memo = Memo::new(move |_| {
        session
            .get()
            .map(|s| s.intake.multimodal_findings)
            .unwrap_or_default()
    });
    let suspect_files_memo = Memo::new(move |_| {
        session
            .get()
            .map(|s| s.intake.suspect_files)
            .unwrap_or_default()
    });
    let repro_steps_memo = Memo::new(move |_| {
        session
            .get()
            .map(|s| s.intake.repro_steps)
            .unwrap_or_default()
    });

    // Automatically fetch extracted keyframes once per screen-recording attachment.
    Effect::new(move |_| {
        let video_ids: Vec<Uuid> = attachments_memo
            .get()
            .into_iter()
            .filter(|a| a.media_kind == MediaKind::ScreenRecording)
            .map(|a| a.id)
            .collect();

        for vid in video_ids {
            let already_requested = keyframes_cache.with_untracked(|map| map.contains_key(&vid));
            if !already_requested {
                keyframes_cache.update(|map| {
                    map.insert(vid, KeyframeFetchState::Loading);
                });
                spawn_local(async move {
                    let result = match api_client::fetch_attachment_keyframes(vid).await {
                        Ok(resp) => KeyframeFetchState::Ready(resp.keyframes),
                        Err(err) => KeyframeFetchState::Error(err),
                    };
                    keyframes_cache.update(|map| {
                        map.insert(vid, result);
                    });
                });
            }
        }
    });

    view! {
        <section class="panel media-evidence-panel">
            <div class="panel-header">
                <div>
                    <h2 class="panel-title">
                        <span class="model-dot dot-cyan"></span>
                        "Multimodal Evidence & Hydrogen Findings"
                        <span class="badge badge-cyan">"Stage 1 Intake"</span>
                    </h2>
                    <div class="panel-subtitle">
                        "Original screenshots, screen recordings, extracted video keyframes, and voice notes paired with Gemini 4 Hydrogen's extracted symbols and repro contract."
                    </div>
                </div>
                <div style="display:flex;align-items:center;gap:8px;flex-wrap:wrap;">
                    <span class="badge">
                        {move || format!("{} attachment(s)", attachments_memo.get().len())}
                    </span>
                    <span class="badge badge-cyan">
                        {move || format!("{} finding(s)", findings_memo.get().len())}
                    </span>
                </div>
            </div>

            <div class="panel-body">
                <div class="media-evidence-grid">
                    // Left column: Captured Visual / Video / Audio Evidence
                    <div class="evidence-col">
                        <div class="evidence-col-header">
                            <span class="field-label" style="margin:0;">"Captured Visual / Video / Audio Evidence"</span>
                        </div>

                        {move || {
                            let items = attachments_memo.get();
                            if items.is_empty() {
                                view! {
                                    <div class="empty-state" style="padding:22px 16px;">
                                        "No binary media attachments stored on this session — Hydrogen findings on the right were synthesized from the prompt and repository context."
                                    </div>
                                }
                                .into_any()
                            } else {
                                view! {
                                    <div class="evidence-stack">
                                        {items
                                            .into_iter()
                                            .map(|att| render_attachment_card(att, keyframes_cache))
                                            .collect_view()}
                                    </div>
                                }
                                .into_any()
                            }
                        }}
                    </div>

                    // Right column: Gemini 4 Hydrogen Multimodal Findings & Derived Repro Contract
                    <div class="evidence-col">
                        <div class="evidence-col-header">
                            <span class="field-label" style="margin:0;">
                                "Gemini 4 Hydrogen Multimodal Findings & Derived Repro Contract"
                            </span>
                        </div>

                        {move || {
                            let findings = findings_memo.get();
                            if findings.is_empty() {
                                view! {
                                    <div class="empty-state" style="padding:20px 16px;">
                                        "Awaiting Gemini 4 Hydrogen multimodal extraction (visual error traces, symbols, and timestamps)..."
                                    </div>
                                }
                                .into_any()
                            } else {
                                view! {
                                    <div class="finding-list">
                                        {findings
                                            .into_iter()
                                            .map(render_finding_card)
                                            .collect_view()}
                                    </div>
                                }
                                .into_any()
                            }
                        }}

                        {move || {
                            let suspects = suspect_files_memo.get();
                            (!suspects.is_empty()).then(|| view! {
                                <div class="repro-contract-block">
                                    <span class="field-label">"Suspect Files"</span>
                                    <div class="suspect-files-row">
                                        {suspects
                                            .into_iter()
                                            .map(|path| view! {
                                                <span class="suspect-file-pill mono">{path}</span>
                                            })
                                            .collect_view()}
                                    </div>
                                </div>
                            })
                        }}

                        {move || {
                            let steps = repro_steps_memo.get();
                            (!steps.is_empty()).then(|| view! {
                                <div class="repro-contract-block">
                                    <span class="field-label">"Derived Reproduction Steps"</span>
                                    <ol class="repro-steps-list">
                                        {steps
                                            .into_iter()
                                            .map(|step| view! {
                                                <li>{step}</li>
                                            })
                                            .collect_view()}
                                    </ol>
                                </div>
                            })
                        }}
                    </div>
                </div>
            </div>
        </section>
    }
}
