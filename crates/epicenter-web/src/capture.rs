use crate::api_client;
use epicenter_common::{AttachmentMeta, CreateSessionRequest, MediaKind, TaskKind, TournamentSession};
use leptos::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen::{closure::Closure, JsCast, JsValue};
use wasm_bindgen_futures::{spawn_local, JsFuture};
use web_sys::{
    Blob, BlobEvent, BlobPropertyBag, DisplayMediaStreamConstraints, DragEvent, HtmlInputElement,
    MediaRecorder, MediaRecorderOptions, MediaStream, MediaStreamConstraints,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecordingMode {
    Screen,
    Voice,
}

impl RecordingMode {
    const fn filename(self) -> &'static str {
        match self {
            Self::Screen => "screen-bug.webm",
            Self::Voice => "voice-note.webm",
        }
    }

    const fn mime_type(self) -> &'static str {
        match self {
            Self::Screen => "video/webm",
            Self::Voice => "audio/webm",
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Screen => "Recording Screen Bug...",
            Self::Voice => "Recording Voice Brief...",
        }
    }
}

struct ActiveRecorder {
    recorder: MediaRecorder,
    stream: MediaStream,
    _on_data: Closure<dyn FnMut(BlobEvent)>,
    _on_stop: Closure<dyn FnMut()>,
}

impl ActiveRecorder {
    fn stop_tracks(&self) {
        let tracks = self.stream.get_tracks();
        for idx in 0..tracks.length() {
            let track = tracks.get(idx);
            if let Ok(stop_fn) = js_sys::Reflect::get(&track, &JsValue::from_str("stop")) {
                if let Ok(func) = stop_fn.dyn_into::<js_sys::Function>() {
                    let _ = func.call0(&track);
                }
            }
        }
    }

    fn request_stop(&self) {
        let _ = self.recorder.stop();
        self.stop_tracks();
    }
}

impl Drop for ActiveRecorder {
    fn drop(&mut self) {
        self.recorder.set_ondataavailable(None);
        self.recorder.set_onstop(None);
        self.stop_tracks();
    }
}

fn build_typed_file(
    chunks: &[Blob],
    filename: &str,
    mime_type: &str,
) -> Result<web_sys::File, String> {
    let chunk_array = js_sys::Array::new();
    for chunk in chunks {
        chunk_array.push(chunk);
    }

    let blob_opts = BlobPropertyBag::new();
    blob_opts.set_type(mime_type);
    let blob = Blob::new_with_blob_sequence_and_options(&chunk_array, &blob_opts)
        .map_err(|e| format!("Failed to assemble recorded Blob: {e:?}"))?;

    if let Some(window) = web_sys::window() {
        if let Ok(ctor_val) = js_sys::Reflect::get(&window, &JsValue::from_str("File")) {
            if let Ok(ctor) = ctor_val.dyn_into::<js_sys::Function>() {
                let parts = js_sys::Array::of1(&blob);
                let opts = js_sys::Object::new();
                let _ = js_sys::Reflect::set(
                    &opts,
                    &JsValue::from_str("type"),
                    &JsValue::from_str(mime_type),
                );
                let args = js_sys::Array::of3(&parts, &JsValue::from_str(filename), &opts);
                if let Ok(file_val) = js_sys::Reflect::construct(&ctor, &args) {
                    if let Ok(file) = file_val.dyn_into::<web_sys::File>() {
                        return Ok(file);
                    }
                }
            }
        }
    }

    let parts = js_sys::Array::of1(&blob);
    web_sys::File::new_with_blob_sequence(&parts, filename)
        .map_err(|e| format!("Failed to construct File '{filename}': {e:?}"))
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

fn media_kind_label(kind: MediaKind) -> &'static str {
    match kind {
        MediaKind::ScreenRecording => "VIDEO",
        MediaKind::Screenshot => "IMAGE",
        MediaKind::Pdf => "PDF",
        MediaKind::VoiceNote => "VOICE",
        MediaKind::Log => "LOG",
        MediaKind::Other => "FILE",
    }
}

#[component]
pub fn MultimodalIntakeDeck(
    on_session_created: Callback<TournamentSession>,
) -> impl IntoView {
    let repo_url = RwSignal::new(String::new());
    let commit_sha = RwSignal::new(String::new());
    let git_ref = RwSignal::new(String::new());
    let task_kind = RwSignal::new(TaskKind::Bug);
    let title = RwSignal::new(String::new());
    let problem_statement = RwSignal::new(String::new());

    let attachments = RwSignal::new(Vec::<AttachmentMeta>::new());
    let is_dragging = RwSignal::new(false);
    let uploading_count = RwSignal::new(0usize);
    let recording_mode = RwSignal::new(Option::<RecordingMode>::None);
    let active_recorder = StoredValue::new_local(Option::<ActiveRecorder>::None);

    let submitting = RwSignal::new(false);
    let error_msg = RwSignal::new(Option::<String>::None);
    let status_note = RwSignal::new(Option::<String>::None);

    let file_input_ref = NodeRef::<leptos::html::Input>::new();

    on_cleanup(move || {
        active_recorder.update_value(|slot| {
            let _ = slot.take();
        });
    });

    let upload_files = move |files: Vec<web_sys::File>| {
        if files.is_empty() {
            return;
        }
        error_msg.set(None);
        status_note.set(None);
        for file in files {
            uploading_count.update(|n| *n += 1);
            spawn_local(async move {
                match api_client::upload_attachment(&file).await {
                    Ok(meta) => {
                        let name = meta.filename.clone();
                        attachments.update(|list| list.push(meta));
                        status_note.set(Some(format!("Uploaded attachment '{name}'")));
                    }
                    Err(err) => {
                        error_msg.set(Some(err));
                    }
                }
                uploading_count.update(|n| *n = n.saturating_sub(1));
            });
        }
    };

    let start_capture = move |mode: RecordingMode| {
        if recording_mode.get_untracked().is_some() {
            return;
        }
        error_msg.set(None);
        status_note.set(None);

        spawn_local(async move {
            let Some(window) = web_sys::window() else {
                error_msg.set(Some("Browser window unavailable".to_string()));
                return;
            };
            let media_devices = match window.navigator().media_devices() {
                Ok(md) => md,
                Err(e) => {
                    error_msg.set(Some(format!(
                        "MediaDevices API unavailable (requires HTTPS or localhost): {e:?}"
                    )));
                    return;
                }
            };

            let stream_promise = match mode {
                RecordingMode::Screen => {
                    let constraints = DisplayMediaStreamConstraints::new();
                    constraints.set_video(&JsValue::TRUE);
                    constraints.set_audio(&JsValue::TRUE);
                    media_devices.get_display_media_with_constraints(&constraints)
                }
                RecordingMode::Voice => {
                    let constraints = MediaStreamConstraints::new();
                    constraints.set_audio(&JsValue::TRUE);
                    constraints.set_video(&JsValue::FALSE);
                    media_devices.get_user_media_with_constraints(&constraints)
                }
            };

            let promise = match stream_promise {
                Ok(p) => p,
                Err(e) => {
                    error_msg.set(Some(format!("Failed to request capture stream: {e:?}")));
                    return;
                }
            };

            let stream_js = match JsFuture::from(promise).await {
                Ok(val) => val,
                Err(e) => {
                    error_msg.set(Some(format!("Capture permission denied or cancelled: {e:?}")));
                    return;
                }
            };

            let stream = match stream_js.dyn_into::<MediaStream>() {
                Ok(s) => s,
                Err(_) => {
                    error_msg.set(Some("Browser returned invalid MediaStream".to_string()));
                    return;
                }
            };

            let preferred_mime = mode.mime_type();
            let recorder_res = if MediaRecorder::is_type_supported(preferred_mime) {
                let opts = MediaRecorderOptions::new();
                opts.set_mime_type(preferred_mime);
                MediaRecorder::new_with_media_stream_and_media_recorder_options(&stream, &opts)
            } else {
                MediaRecorder::new_with_media_stream(&stream)
            };

            let recorder = match recorder_res {
                Ok(r) => r,
                Err(e) => {
                    error_msg.set(Some(format!("MediaRecorder initialization failed: {e:?}")));
                    return;
                }
            };

            let chunks: Rc<RefCell<Vec<Blob>>> = Rc::new(RefCell::new(Vec::new()));
            let chunks_for_data = Rc::clone(&chunks);
            let on_data = Closure::<dyn FnMut(BlobEvent)>::new(move |ev: BlobEvent| {
                if let Some(blob) = ev.data() {
                    if blob.size() > 0.0 {
                        chunks_for_data.borrow_mut().push(blob);
                    }
                }
            });

            let chunks_for_stop = Rc::clone(&chunks);
            let on_stop = Closure::<dyn FnMut()>::new(move || {
                recording_mode.set(None);
                let captured = std::mem::take(&mut *chunks_for_stop.borrow_mut());
                active_recorder.update_value(|slot| {
                    let _ = slot.take();
                });

                if captured.is_empty() {
                    status_note.set(Some("Recording stopped with no audio/video data.".to_string()));
                    return;
                }

                match build_typed_file(&captured, mode.filename(), mode.mime_type()) {
                    Ok(file) => {
                        uploading_count.update(|n| *n += 1);
                        status_note.set(Some(format!("Uploading {}...", mode.filename())));
                        spawn_local(async move {
                            match api_client::upload_attachment(&file).await {
                                Ok(meta) => {
                                    let name = meta.filename.clone();
                                    attachments.update(|list| list.push(meta));
                                    status_note.set(Some(format!(
                                        "Captured and uploaded '{name}' for Hydrogen intake."
                                    )));
                                }
                                Err(err) => {
                                    error_msg.set(Some(format!(
                                        "Recorded media upload failed: {err}"
                                    )));
                                }
                            }
                            uploading_count.update(|n| *n = n.saturating_sub(1));
                        });
                    }
                    Err(err) => {
                        error_msg.set(Some(err));
                    }
                }
            });

            recorder.set_ondataavailable(Some(on_data.as_ref().unchecked_ref()));
            recorder.set_onstop(Some(on_stop.as_ref().unchecked_ref()));

            if let Err(e) = recorder.start_with_time_slice(500) {
                error_msg.set(Some(format!("Failed to start MediaRecorder: {e:?}")));
                return;
            }

            active_recorder.set_value(Some(ActiveRecorder {
                recorder,
                stream,
                _on_data: on_data,
                _on_stop: on_stop,
            }));
            recording_mode.set(Some(mode));
        });
    };

    let stop_capture = move |_| {
        active_recorder.with_value(|slot| {
            if let Some(rec) = slot.as_ref() {
                rec.request_stop();
            }
        });
    };

    let load_github_preset = move |_| {
        repo_url.set("https://github.com/encode/httpx".to_string());
        commit_sha.set(String::new());
        git_ref.set("master".to_string());
        task_kind.set(TaskKind::Bug);
        title.set("Preserve query parameter bracket encoding & auth header stripping on cross-origin redirects".to_string());
        problem_statement.set(
            "When following a 302 redirect across hosts with httpx.Client, query strings containing square brackets or percent-encoded separators are double-normalized and non-standard casing in sensitive headers can leak across redirects. Reproduce with a failing pytest test, compare surgical vs. root-cause URL/redirect refactors in parallel sandboxes, benchmark redirect throughput, and audit caller blast radius."
                .to_string(),
        );
        error_msg.set(None);
        status_note.set(Some(
            "Loaded real GitHub preset (encode/httpx). Ready to launch tournament!".to_string(),
        ));
    };

    let on_drag_over = move |ev: DragEvent| {
        ev.prevent_default();
        is_dragging.set(true);
    };

    let on_drag_leave = move |ev: DragEvent| {
        ev.prevent_default();
        is_dragging.set(false);
    };

    let on_drop = move |ev: DragEvent| {
        ev.prevent_default();
        is_dragging.set(false);
        let Some(dt) = ev.data_transfer() else {
            return;
        };
        let Some(file_list) = dt.files() else {
            return;
        };
        let mut dropped = Vec::new();
        for i in 0..file_list.length() {
            if let Some(file) = file_list.get(i) {
                dropped.push(file);
            }
        }
        upload_files(dropped);
    };

    let on_file_input_change = move |ev: web_sys::Event| {
        let Some(target) = ev.target() else {
            return;
        };
        let Ok(input) = target.dyn_into::<HtmlInputElement>() else {
            return;
        };
        let Some(file_list) = input.files() else {
            return;
        };
        let mut selected = Vec::new();
        for i in 0..file_list.length() {
            if let Some(file) = file_list.get(i) {
                selected.push(file);
            }
        }
        input.set_value("");
        upload_files(selected);
    };

    let on_submit = move |ev: web_sys::SubmitEvent| {
        ev.prevent_default();
        let repo = repo_url.get_untracked().trim().to_string();
        let problem = problem_statement.get_untracked().trim().to_string();

        if repo.is_empty() {
            error_msg.set(Some("Repository URL is required (e.g. https://github.com/encode/httpx).".to_string()));
            return;
        }
        if problem.is_empty() {
            error_msg.set(Some("Problem statement or bug notes are required.".to_string()));
            return;
        }

        let sha = commit_sha.get_untracked().trim().to_string();
        let gref = git_ref.get_untracked().trim().to_string();
        let title_str = title.get_untracked().trim().to_string();
        let current_attachments = attachments.get_untracked();
        let attachment_ids = current_attachments.iter().map(|a| a.id).collect();

        let req = CreateSessionRequest {
            repo_url: repo,
            commit_sha: (!sha.is_empty()).then_some(sha),
            git_ref: (!gref.is_empty()).then_some(gref),
            task_kind: Some(task_kind.get_untracked()),
            title: (!title_str.is_empty()).then_some(title_str),
            problem_statement: problem,
            attachment_ids,
            attachments: current_attachments,
        };

        submitting.set(true);
        error_msg.set(None);
        status_note.set(Some("Initializing Gemini 4 tournament session...".to_string()));

        spawn_local(async move {
            match api_client::create_session(&req).await {
                Ok(resp) => {
                    submitting.set(false);
                    status_note.set(None);
                    on_session_created.run(resp.session);
                }
                Err(err) => {
                    submitting.set(false);
                    status_note.set(None);
                    error_msg.set(Some(err));
                }
            }
        });
    };

    view! {
        <section class="panel">
            <div class="panel-header">
                <div>
                    <h2 class="panel-title">
                        <span class="model-dot dot-cyan"></span>
                        "Multimodal Intake Deck"
                        <span class="badge badge-cyan">"Gemini 4 Hydrogen"</span>
                    </h2>
                    <div class="panel-subtitle">
                        "Beyond a text box — drop screen recordings, stacktrace screenshots, PDFs, or record live from your browser."
                    </div>
                </div>
                <div style="display:flex;gap:8px;align-items:center;flex-wrap:wrap;">
                    <button
                        type="button"
                        class="btn btn-amber btn-sm"
                        on:click=load_github_preset
                    >
                        "Load Real GitHub Bug Preset"
                    </button>
                </div>
            </div>

            <form class="panel-body" on:submit=on_submit>
                <div class="intake-grid">
                    // Left column: Repository & Problem Statement
                    <div class="form-stack">
                        <div class="form-row-3">
                            <div>
                                <label class="field-label">"Target GitHub Repository URL"</label>
                                <input
                                    type="url"
                                    class="input mono"
                                    placeholder="https://github.com/encode/httpx"
                                    prop:value=move || repo_url.get()
                                    on:input=move |ev| repo_url.set(event_target_value(&ev))
                                />
                            </div>
                            <div>
                                <label class="field-label">"Git Ref (Optional)"</label>
                                <input
                                    type="text"
                                    class="input mono"
                                    placeholder="main"
                                    prop:value=move || git_ref.get()
                                    on:input=move |ev| git_ref.set(event_target_value(&ev))
                                />
                            </div>
                            <div>
                                <label class="field-label">"Commit SHA (Optional)"</label>
                                <input
                                    type="text"
                                    class="input mono"
                                    placeholder="HEAD"
                                    prop:value=move || commit_sha.get()
                                    on:input=move |ev| commit_sha.set(event_target_value(&ev))
                                />
                            </div>
                        </div>

                        <div class="form-row-2">
                            <div>
                                <label class="field-label">"Task Kind"</label>
                                <div class="task-kind-toggle">
                                    <button
                                        type="button"
                                        class=move || if task_kind.get() == TaskKind::Bug { "kind-btn active" } else { "kind-btn" }
                                        on:click=move |_| task_kind.set(TaskKind::Bug)
                                    >
                                        "Bug Fix (Failing Red-Gate)"
                                    </button>
                                    <button
                                        type="button"
                                        class=move || if task_kind.get() == TaskKind::Feature { "kind-btn active" } else { "kind-btn" }
                                        on:click=move |_| task_kind.set(TaskKind::Feature)
                                    >
                                        "Feature / Optimization"
                                    </button>
                                </div>
                            </div>
                            <div>
                                <label class="field-label">"Session Title (Optional)"</label>
                                <input
                                    type="text"
                                    class="input"
                                    placeholder="Short headline for tournament brief"
                                    prop:value=move || title.get()
                                    on:input=move |ev| title.set(event_target_value(&ev))
                                />
                            </div>
                        </div>

                        <div>
                            <label class="field-label">"Problem Statement / Failure Notes"</label>
                            <textarea
                                class="textarea"
                                placeholder="Describe the bug, expected vs. actual behavior, performance target, or paste crash logs..."
                                prop:value=move || problem_statement.get()
                                on:input=move |ev| problem_statement.set(event_target_value(&ev))
                            ></textarea>
                        </div>
                    </div>

                    // Right column: Multimodal Dropzone + Live web-sys Capture
                    <div class="form-stack">
                        <div
                            class=move || if is_dragging.get() { "dropzone dragging" } else { "dropzone" }
                            on:dragover=on_drag_over
                            on:dragleave=on_drag_leave
                            on:drop=on_drop
                        >
                            <div class="dropzone-title">
                                "Drop Multimodal Evidence Here"
                            </div>
                            <div class="dropzone-hint">
                                ".mp4 · .webm · .gif · .png · .jpg · .pdf · .wav · .mp3  — uploaded directly to Hydrogen intake"
                            </div>

                            <input
                                node_ref=file_input_ref
                                type="file"
                                multiple=true
                                accept=".mp4,.webm,.gif,.png,.jpg,.jpeg,.pdf,.wav,.mp3,video/*,image/*,audio/*,application/pdf"
                                style="display:none;"
                                on:change=on_file_input_change
                            />

                            <div class="capture-controls">
                                <button
                                    type="button"
                                    class="btn btn-sm"
                                    on:click=move |_| {
                                        if let Some(input) = file_input_ref.get() {
                                            input.click();
                                        }
                                    }
                                >
                                    "Browse Files"
                                </button>

                                {move || match recording_mode.get() {
                                    Some(mode) => view! {
                                        <button
                                            type="button"
                                            class="btn btn-danger btn-sm"
                                            on:click=stop_capture
                                        >
                                            <span class="rec-dot"></span>
                                            {format!("Stop {} & Upload", match mode {
                                                RecordingMode::Screen => "Screen",
                                                RecordingMode::Voice => "Voice",
                                            })}
                                        </button>
                                    }.into_any(),
                                    None => view! {
                                        <>
                                            <button
                                                type="button"
                                                class="btn btn-sm"
                                                on:click=move |_| start_capture(RecordingMode::Screen)
                                            >
                                                "Record Screen Bug"
                                            </button>
                                            <button
                                                type="button"
                                                class="btn btn-sm"
                                                on:click=move |_| start_capture(RecordingMode::Voice)
                                            >
                                                "Record Voice Brief"
                                            </button>
                                        </>
                                    }.into_any(),
                                }}
                            </div>

                            {move || recording_mode.get().map(|mode| view! {
                                <div style="margin-top:10px;display:inline-flex;align-items:center;gap:7px;font-size:12px;color:#fca5a5;">
                                    <span class="rec-dot"></span>
                                    <span>{mode.label()}</span>
                                </div>
                            })}

                            {move || (uploading_count.get() > 0).then(|| view! {
                                <div style="margin-top:8px;font-size:11px;color:#7dd3fc;font-family:var(--font-mono);">
                                    {move || format!("Uploading {} file(s) to /api/attachments...", uploading_count.get())}
                                </div>
                            })}
                        </div>

                        // Uploaded attachment chips
                        {move || {
                            let items = attachments.get();
                            if items.is_empty() {
                                view! {
                                    <div style="font-size:11.5px;color:var(--text-dim);text-align:center;">
                                        "No attachments queued yet — text-only or multimodal sessions both work."
                                    </div>
                                }.into_any()
                            } else {
                                view! {
                                    <div class="attachment-list">
                                        {items.into_iter().map(|att| {
                                            let att_id = att.id;
                                            let size_text = format_bytes(att.size_bytes);
                                            let kind_str = media_kind_label(att.media_kind);
                                            view! {
                                                <span class="attachment-badge">
                                                    <span class="attachment-kind">{kind_str}</span>
                                                    <span class="mono" style="font-size:11.5px;">{att.filename}</span>
                                                    <span style="color:var(--text-dim);font-size:11px;">{size_text}</span>
                                                    <button
                                                        type="button"
                                                        class="attachment-remove"
                                                        title="Remove attachment"
                                                        on:click=move |_| {
                                                            attachments.update(|list| list.retain(|x| x.id != att_id));
                                                        }
                                                    >
                                                        "×"
                                                    </button>
                                                </span>
                                            }
                                        }).collect_view()}
                                    </div>
                                }.into_any()
                            }
                        }}

                        // Error / status feedback
                        {move || error_msg.get().map(|msg| view! {
                            <div class="alert-banner err">{msg}</div>
                        })}
                        {move || status_note.get().map(|msg| view! {
                            <div class="alert-banner ok">{msg}</div>
                        })}

                        <div style="display:flex;justify-content:flex-end;align-items:center;gap:10px;margin-top:auto;padding-top:4px;">
                            <button
                                type="submit"
                                class="btn btn-primary"
                                disabled=move || submitting.get() || (uploading_count.get() != 0) || recording_mode.get().is_some()
                            >
                                {move || if submitting.get() {
                                    "Launching Gemini 4 Tournament..."
                                } else {
                                    "Launch Epicenter Tournament"
                                }}
                            </button>
                        </div>
                    </div>
                </div>
            </form>
        </section>
    }
}
