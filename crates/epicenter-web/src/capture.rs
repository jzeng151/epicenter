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
                                        "Captured and uploaded '{name}'."
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
            error_msg.set(Some("Repository URL is required (e.g. https://github.com/pallets/click).".to_string()));
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
        status_note.set(Some("Starting comparison session...".to_string()));

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
        <section id="intake-panel" class="panel" aria-labelledby="intake-heading">
            <div class="panel-header">
                <div>
                    <h2 id="intake-heading" class="panel-title">
                        <span class="model-dot dot-cyan" aria-hidden="true"></span>
                        "Multimodal Intake"
                    </h2>
                    <div class="panel-subtitle">
                        "Upload screenshots, screen recordings, voice notes or directly type your issue"
                    </div>
                </div>
            </div>

            <form class="panel-body" on:submit=on_submit novalidate=true>
                // Clean jargon-free overview of the comparison pipeline
                <div class="pipeline-flow-strip" aria-label="Comparison workflow">
                    <span class="pipeline-flow-step active">
                        <span class="model-dot dot-cyan" aria-hidden="true"></span>
                        <strong>"1. Intake"</strong>
                        "Extract clues from media & notes"
                    </span>
                    <span class="pipeline-flow-arrow" aria-hidden="true">"→"</span>
                    <span class="pipeline-flow-step">
                        <span class="model-dot dot-red" aria-hidden="true"></span>
                        <strong>"2. Repro Test"</strong>
                        "Verify a failing test before editing"
                    </span>
                    <span class="pipeline-flow-arrow" aria-hidden="true">"→"</span>
                    <span class="pipeline-flow-step">
                        <span class="model-dot dot-emerald" aria-hidden="true"></span>
                        <strong>"3. Implement"</strong>
                        "Build multiple fixes in isolation"
                    </span>
                    <span class="pipeline-flow-arrow" aria-hidden="true">"→"</span>
                    <span class="pipeline-flow-step">
                        <span class="model-dot dot-violet" aria-hidden="true"></span>
                        <strong>"4. Benchmark & Audit"</strong>
                        "Measure speed & code impact"
                    </span>
                    <span class="pipeline-flow-arrow" aria-hidden="true">"→"</span>
                    <span class="pipeline-flow-step">
                        <span class="model-dot dot-amber" aria-hidden="true"></span>
                        <strong>"5. Summary"</strong>
                        "Audio briefing, diff & export"
                    </span>
                </div>

                <div class="intake-grid">
                    // Primary Left Column: Multimodal Media Capture + Inline Previews
                    <div class="form-stack intake-media-col">
                        <div style="display:flex;align-items:center;justify-content:space-between;gap:8px;flex-wrap:wrap;">
                            <span class="field-label" style="margin:0;color:#7dd3fc;">
                                "1. Capture or Upload Visual / Video / Audio Evidence"
                            </span>
                            <span class="badge badge-cyan" role="status" aria-live="polite">
                                {move || format!("{} attached", attachments.get().len())}
                            </span>
                        </div>

                        <div
                            class=move || if is_dragging.get() { "dropzone dropzone-primary dragging" } else { "dropzone dropzone-primary" }
                            on:dragover=on_drag_over
                            on:dragleave=on_drag_leave
                            on:drop=on_drop
                        >
                            <div class="dropzone-title">
                                "Upload Screenshots, Screen Recordings, Voice Notes, or PDFs"
                            </div>
                            <div class="dropzone-hint">
                                ".png · .jpg · .mp4 · .webm · .gif · .wav · .mp3 · .pdf — drag & drop or browse"
                            </div>

                            <input
                                node_ref=file_input_ref
                                id="intake-media-upload"
                                type="file"
                                multiple=true
                                tabindex="-1"
                                aria-label="Upload screenshots, screen recordings, voice notes, or PDFs"
                                accept=".mp4,.webm,.gif,.png,.jpg,.jpeg,.pdf,.wav,.mp3,video/*,image/*,audio/*,application/pdf"
                                style="display:none;"
                                on:change=on_file_input_change
                            />

                            <div class="capture-controls">
                                <button
                                    type="button"
                                    class="btn btn-primary btn-sm"
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
                                            <span class="rec-dot" aria-hidden="true"></span>
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
                                                "Record Screen"
                                            </button>
                                            <button
                                                type="button"
                                                class="btn btn-sm"
                                                on:click=move |_| start_capture(RecordingMode::Voice)
                                            >
                                                "Record Voice Note"
                                            </button>
                                        </>
                                    }.into_any(),
                                }}
                            </div>

                            {move || recording_mode.get().map(|mode| view! {
                                <div role="status" aria-live="polite" style="margin-top:10px;display:inline-flex;align-items:center;gap:7px;font-size:12px;color:#fca5a5;">
                                    <span class="rec-dot" aria-hidden="true"></span>
                                    <span>{mode.label()}</span>
                                </div>
                            })}

                            {move || (uploading_count.get() > 0).then(|| view! {
                                <div role="status" aria-live="polite" style="margin-top:8px;font-size:11px;color:#7dd3fc;font-family:var(--font-mono);">
                                    {move || format!("Uploading {} file(s)...", uploading_count.get())}
                                </div>
                            })}
                        </div>

                        // Inline media preview gallery right below the dropzone
                        {move || {
                            let items = attachments.get();
                            if items.is_empty() {
                                view! {
                                    <div class="intake-preview-empty">
                                        "No media attached yet — upload a screenshot, screen recording, voice note, or PDF above."
                                    </div>
                                }.into_any()
                            } else {
                                view! {
                                    <div class="intake-preview-grid">
                                        {items.into_iter().map(|att| {
                                            let att_id = att.id;
                                            let size_text = format_bytes(att.size_bytes);
                                            let kind_str = media_kind_label(att.media_kind);
                                            let filename = att.filename.clone();
                                            let filename_title = filename.clone();
                                            let remove_aria = format!("Remove attachment {}", filename);
                                            let mime = att.mime_type.clone();
                                            let url = api_client::attachment_content_url(att_id);
                                            let open_url = url.clone();

                                            let preview_body = match att.media_kind {
                                                MediaKind::Screenshot => view! {
                                                    <a
                                                        href=open_url
                                                        target="_blank"
                                                        rel="noopener noreferrer"
                                                        aria-label=format!("Open full screenshot {} (opens in a new tab)", filename)
                                                    >
                                                        <img
                                                            class="intake-preview-img"
                                                            src=url
                                                            alt=filename.clone()
                                                            loading="lazy"
                                                        />
                                                    </a>
                                                }.into_any(),
                                                MediaKind::ScreenRecording => view! {
                                                    <video
                                                        class="intake-preview-video"
                                                        controls=true
                                                        muted=true
                                                        preload="metadata"
                                                        src=url
                                                        aria-label=format!("Screen recording preview: {}", filename)
                                                    ></video>
                                                }.into_any(),
                                                MediaKind::VoiceNote => view! {
                                                    <audio
                                                        class="intake-preview-audio"
                                                        controls=true
                                                        preload="metadata"
                                                        src=url
                                                        aria-label=format!("Voice note preview: {}", filename)
                                                    ></audio>
                                                }.into_any(),
                                                MediaKind::Pdf | MediaKind::Log | MediaKind::Other => view! {
                                                    <div class="intake-preview-file">
                                                        <span class="mono" style="font-size:11px;color:var(--text-muted);">{mime}</span>
                                                        <a
                                                            href=open_url
                                                            target="_blank"
                                                            rel="noopener noreferrer"
                                                            class="mono"
                                                            style="font-size:11.5px;"
                                                        >
                                                            "Open File ↗"
                                                            <span class="sr-only">" (opens in a new tab)"</span>
                                                        </a>
                                                    </div>
                                                }.into_any(),
                                            };

                                            view! {
                                                <div class="intake-preview-card">
                                                    <div class="intake-preview-head">
                                                        <div style="display:flex;align-items:center;gap:6px;min-width:0;">
                                                            <span class="attachment-kind">{kind_str}</span>
                                                            <span class="mono intake-preview-name" title=filename_title>{filename}</span>
                                                        </div>
                                                        <div style="display:flex;align-items:center;gap:6px;flex-shrink:0;">
                                                            <span style="color:var(--text-dim);font-size:11px;" class="mono">{size_text}</span>
                                                            <button
                                                                type="button"
                                                                class="attachment-remove"
                                                                aria-label=remove_aria
                                                                title="Remove attachment"
                                                                on:click=move |_| {
                                                                    attachments.update(|list| list.retain(|x| x.id != att_id));
                                                                }
                                                            >
                                                                <span aria-hidden="true">"×"</span>
                                                            </button>
                                                        </div>
                                                    </div>
                                                    {preview_body}
                                                </div>
                                            }
                                        }).collect_view()}
                                    </div>
                                }.into_any()
                            }
                        }}
                    </div>

                    // Right Column: Target Repository & Issue Context
                    <div class="form-stack">
                        <span class="field-label" style="margin:0;color:#cbd5e1;">
                            "2. Target Repository & Issue Context"
                        </span>

                        <div class="form-row-3">
                            <div>
                                <label class="field-label" for="intake-repo-url">"GitHub Repository URL"</label>
                                <input
                                    id="intake-repo-url"
                                    type="url"
                                    class="input mono"
                                    placeholder="https://github.com/owner/repo"
                                    aria-required="true"
                                    aria-invalid=move || (error_msg.get().is_some() && repo_url.get().trim().is_empty()).then_some("true")
                                    prop:value=move || repo_url.get()
                                    on:input=move |ev| repo_url.set(event_target_value(&ev))
                                />
                            </div>
                            <div>
                                <label class="field-label" for="intake-commit-sha">"Commit SHA (Optional)"</label>
                                <input
                                    id="intake-commit-sha"
                                    type="text"
                                    class="input mono"
                                    placeholder="HEAD"
                                    prop:value=move || commit_sha.get()
                                    on:input=move |ev| commit_sha.set(event_target_value(&ev))
                                />
                            </div>
                            <div>
                                <label class="field-label" for="intake-git-ref">"Branch / Ref (Optional)"</label>
                                <input
                                    id="intake-git-ref"
                                    type="text"
                                    class="input mono"
                                    placeholder="main"
                                    prop:value=move || git_ref.get()
                                    on:input=move |ev| git_ref.set(event_target_value(&ev))
                                />
                            </div>
                        </div>

                        <div class="form-row-2">
                            <div>
                                <span id="intake-task-type-label" class="field-label">"Task Type"</span>
                                <div class="task-kind-toggle" role="group" aria-labelledby="intake-task-type-label">
                                    <button
                                        type="button"
                                        class=move || if task_kind.get() == TaskKind::Bug { "kind-btn active" } else { "kind-btn" }
                                        aria-pressed=move || (task_kind.get() == TaskKind::Bug).to_string()
                                        on:click=move |_| task_kind.set(TaskKind::Bug)
                                    >
                                        "Bug Fix"
                                    </button>
                                    <button
                                        type="button"
                                        class=move || if task_kind.get() == TaskKind::Feature { "kind-btn active" } else { "kind-btn" }
                                        aria-pressed=move || (task_kind.get() == TaskKind::Feature).to_string()
                                        on:click=move |_| task_kind.set(TaskKind::Feature)
                                    >
                                        "Feature / Optimization"
                                    </button>
                                </div>
                            </div>
                            <div>
                                <label class="field-label" for="intake-session-title">"Session Title (Optional)"</label>
                                <input
                                    id="intake-session-title"
                                    type="text"
                                    class="input"
                                    placeholder="Short headline for this comparison"
                                    prop:value=move || title.get()
                                    on:input=move |ev| title.set(event_target_value(&ev))
                                />
                            </div>
                        </div>

                        <div>
                            <label class="field-label" for="intake-problem-statement">"Problem Statement / Notes"</label>
                            <textarea
                                id="intake-problem-statement"
                                class="textarea"
                                placeholder="Describe the bug, expected vs. actual behavior, performance target, or paste error logs..."
                                aria-required="true"
                                aria-invalid=move || (error_msg.get().is_some() && problem_statement.get().trim().is_empty()).then_some("true")
                                prop:value=move || problem_statement.get()
                                on:input=move |ev| problem_statement.set(event_target_value(&ev))
                            ></textarea>
                        </div>

                        // Error / status feedback
                        {move || error_msg.get().map(|msg| view! {
                            <div class="alert-banner err" role="alert">{msg}</div>
                        })}
                        {move || status_note.get().map(|msg| view! {
                            <div class="alert-banner ok" role="status" aria-live="polite">{msg}</div>
                        })}

                        <div style="display:flex;justify-content:flex-end;align-items:center;gap:10px;margin-top:auto;padding-top:4px;">
                            <button
                                type="submit"
                                class="btn btn-primary"
                                disabled=move || submitting.get() || (uploading_count.get() != 0) || recording_mode.get().is_some()
                            >
                                {move || if submitting.get() {
                                    "Starting Comparison..."
                                } else {
                                    "Launch Comparison"
                                }}
                            </button>
                        </div>
                    </div>
                </div>
            </form>
        </section>
    }
}
