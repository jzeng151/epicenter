use crate::db::{infer_media_kind, is_simulated_eval_label, DbStore};
use crate::github::{
    apply_hunks, apply_unified_diff_to_map, extract_media_urls, is_test_file,
    parse_github_pr_url, parse_github_repo_url, parse_unified_diff, FetchedPrContext,
    GitHubClient, PrSlug, RepoSlug,
};
use crate::live_judge::{
    download_pr_media_attachments, evaluate_fetched_pr, strip_editable_pth_files,
    verify_candidate_on_host_with_cache,
};
use crate::media::{base64_encode, TempDirGuard};
use crate::routes::{
    build_router, create_real_bug_preset, create_session, export_candidate_bundle,
    get_attachment_bytes, get_attachment_keyframes, get_eval_by_id, get_latest_eval, get_session,
    get_session_audio, health_handler, list_sessions, session_events_sse, upload_attachment,
};
use crate::state::AppState;
use axum::{
    body::Body,
    extract::{FromRequest, Multipart, Path, State},
    http::{header, Request, StatusCode},
    response::IntoResponse,
    Json,
};
use chrono::{Duration as ChronoDuration, Utc};
use epicenter_common::{
    ArmOutcome, AttachmentMeta, CreateSessionRequest, EvalArm, EvalIssueCase, EvalScorecard,
    CandidateResult, CandidateStatus, EvalSplit, FiveSigmaEval, MediaKind, TaskKind, TournamentStage,
};
use epicenter_engine::TournamentEngine;
use futures::StreamExt;
use std::collections::BTreeMap;
use std::time::Duration;
use uuid::Uuid;

fn test_state() -> AppState {
    let db = DbStore::open_in_memory().expect("in-memory sqlite");
    let engine = TournamentEngine::new(None);
    let github = GitHubClient::new(None);
    AppState::new(db, engine, github)
}

#[tokio::test]
async fn health_endpoint_returns_server_status() {
    let state = test_state();
    let Json(resp) = health_handler(State(state)).await;
    assert_eq!(resp.status, "ok");
    assert_eq!(resp.version, env!("CARGO_PKG_VERSION"));
    assert!(!resp.has_gemini_key);
    assert!(!resp.has_github_token);

    let state_with_gh = AppState::new(
        DbStore::open_in_memory().unwrap(),
        TournamentEngine::new(None),
        GitHubClient::new(Some("ghp_test_token".to_string())),
    );
    let Json(resp2) = health_handler(State(state_with_gh.clone())).await;
    assert!(resp2.has_github_token);

    // Ensure router builds cleanly.
    let _router = build_router(state_with_gh);
}

#[tokio::test]
async fn sqlite_roundtrips_sessions_attachments_audio_and_eval_runs() {
    let db = DbStore::open_in_memory().unwrap();

    // Sessions (verify newest-first ordering).
    let req1 = CreateSessionRequest {
        repo_url: "https://github.com/acme/alpha".to_string(),
        commit_sha: None,
        git_ref: Some("main".to_string()),
        task_kind: Some(TaskKind::Bug),
        title: Some("Older session".to_string()),
        problem_statement: "Old bug".to_string(),
        attachment_ids: Vec::new(),
        attachments: Vec::new(),
    };
    let mut s1 = TournamentEngine::initialize_session(&req1);
    s1.created_at = Utc::now() - ChronoDuration::minutes(10);
    s1.updated_at = s1.created_at;
    db.upsert_session(&s1).unwrap();

    let req2 = CreateSessionRequest {
        repo_url: "https://github.com/acme/beta".to_string(),
        commit_sha: Some("deadbeef".to_string()),
        git_ref: None,
        task_kind: Some(TaskKind::Feature),
        title: Some("Newer session".to_string()),
        problem_statement: "Fast cache".to_string(),
        attachment_ids: Vec::new(),
        attachments: Vec::new(),
    };
    let s2 = TournamentEngine::initialize_session(&req2);
    db.upsert_session(&s2).unwrap();

    let fetched = db.get_session(s2.id).unwrap().expect("session present");
    assert_eq!(fetched.id, s2.id);
    assert_eq!(fetched.intake.title, "Newer session");

    let list = db.list_sessions().unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0].id, s2.id, "newest session must appear first");
    assert_eq!(list[1].id, s1.id);

    // Attachments roundtrip.
    let att_id = Uuid::new_v4();
    let meta = AttachmentMeta {
        id: att_id,
        filename: "crash.png".to_string(),
        mime_type: "image/png".to_string(),
        media_kind: infer_media_kind("image/png"),
        size_bytes: 5,
        file_uri: None,
    };
    db.insert_attachment(&meta, b"PNG42").unwrap();
    let (single_meta, single_bytes) = db
        .get_attachment(att_id)
        .unwrap()
        .expect("single attachment present");
    assert_eq!(single_meta.id, att_id);
    assert_eq!(single_meta.filename, "crash.png");
    assert_eq!(single_bytes, b"PNG42");
    assert!(db.get_attachment(Uuid::new_v4()).unwrap().is_none());

    let loaded = db.get_attachments_by_ids(&[att_id]).unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].0.filename, "crash.png");
    assert_eq!(loaded[0].0.media_kind, MediaKind::Screenshot);
    assert_eq!(loaded[0].1, b"PNG42");

    // Audio briefing WAV roundtrip.
    let wav = b"RIFF\x24\x00\x00\x00WAVEfmt ";
    assert!(db.get_audio_briefing(s2.id).unwrap().is_none());
    db.save_audio_briefing(s2.id, wav).unwrap();
    assert_eq!(db.get_audio_briefing(s2.id).unwrap().as_deref(), Some(wav.as_slice()));

    // Eval run roundtrip.
    let eval = sample_eval_run();
    db.upsert_eval_run(&eval).unwrap();
    let loaded_eval = db.get_eval_run(eval.run_id).unwrap().expect("eval stored");
    assert_eq!(loaded_eval.run_id, eval.run_id);
    assert_eq!(loaded_eval.label, eval.label);
}

#[tokio::test]
async fn eval_latest_seeds_from_disk_when_table_empty() {
    let eval = sample_eval_run();
    let seed_path = std::env::temp_dir().join(format!("epicenter_eval_seed_{}.json", Uuid::new_v4()));
    std::fs::write(&seed_path, serde_json::to_string_pretty(&eval).unwrap()).unwrap();

    let db = DbStore::open_in_memory()
        .unwrap()
        .with_eval_seed_path(&seed_path);
    let state = AppState::new(db, TournamentEngine::new(None), GitHubClient::new(None));

    let Json(latest) = get_latest_eval(State(state.clone())).await.unwrap();
    let seeded = latest.expect("should auto-seed from disk JSON");
    assert_eq!(seeded.run_id, eval.run_id);

    // Delete disk file and verify it now reads from SQLite via GET /api/eval/{run_id}.
    let _ = std::fs::remove_file(&seed_path);
    let Json(by_id) = get_eval_by_id(State(state), Path(eval.run_id))
        .await
        .unwrap();
    assert_eq!(by_id.run_id, eval.run_id);
}

#[tokio::test]
async fn multipart_upload_infers_media_kind_and_persists_bytes() {
    let state = test_state();
    let boundary = "----EpicenterTestBoundary7MA4YWxkTrZu0gW";
    let payload = format!(
        "--{b}\r\n\
         Content-Disposition: form-data; name=\"file\"; filename=\"repro.mp4\"\r\n\
         Content-Type: video/mp4\r\n\r\n\
         FAKE_MP4_STREAM_BYTES\r\n\
         --{b}--\r\n",
        b = boundary
    );

    let req = Request::builder()
        .method("POST")
        .uri("/api/attachments")
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(payload))
        .unwrap();

    let multipart = Multipart::from_request(req, &state).await.unwrap();
    let Json(meta) = upload_attachment(State(state.clone()), multipart)
        .await
        .unwrap();

    assert_eq!(meta.filename, "repro.mp4");
    assert_eq!(meta.mime_type, "video/mp4");
    assert_eq!(meta.media_kind, MediaKind::ScreenRecording);
    assert_eq!(meta.size_bytes, b"FAKE_MP4_STREAM_BYTES".len() as u64);

    let stored = state.db.get_attachments_by_ids(&[meta.id]).unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].1, b"FAKE_MP4_STREAM_BYTES");

    assert_eq!(infer_media_kind("application/pdf"), MediaKind::Pdf);
    assert_eq!(infer_media_kind("audio/wav"), MediaKind::VoiceNote);
    assert_eq!(infer_media_kind("text/plain; charset=utf-8"), MediaKind::Log);
}

#[tokio::test]
async fn create_session_completes_in_background_and_exports_candidate_bundle() {
    let state = test_state();

    // Seed an uploaded attachment first so POST /api/sessions loads it by attachment_ids.
    let att_id = Uuid::new_v4();
    let att_meta = AttachmentMeta {
        id: att_id,
        filename: "trace.log".to_string(),
        mime_type: "text/plain".to_string(),
        media_kind: MediaKind::Log,
        size_bytes: 19,
        file_uri: None,
    };
    state
        .db
        .insert_attachment(&att_meta, b"KeyError: cache_key")
        .unwrap();

    let req = CreateSessionRequest {
        repo_url: "https://github.com/encode/httpx".to_string(),
        commit_sha: Some("abc1234".to_string()),
        git_ref: Some("main".to_string()),
        task_kind: Some(TaskKind::Bug),
        title: Some("Fix connection pool race".to_string()),
        problem_statement: "Concurrent redirects leak sockets under burst traffic.".to_string(),
        attachment_ids: vec![att_id],
        attachments: Vec::new(),
    };

    let Json(created) = create_session(State(state.clone()), Json(req))
        .await
        .unwrap();
    let session_id = created.session.id;
    assert_eq!(created.session.intake.attachments.len(), 1);
    assert_eq!(created.session.intake.attachments[0].id, att_id);

    // Wait for background offline tournament execution to reach Completed.
    let mut final_session = None;
    for _ in 0..80 {
        let Json(snap) = get_session(State(state.clone()), Path(session_id))
            .await
            .unwrap();
        if snap.stage == TournamentStage::Completed {
            final_session = Some(snap);
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    let mut completed = final_session.expect("background tournament should complete");
    assert_eq!(completed.stage, TournamentStage::Completed);

    // Ensure export endpoint is exercised both with the Phase 2 engine stub and full engine.
    let strategy_id = if let Some(first) = completed.candidates.first() {
        first.strategy_id.clone()
    } else {
        let mut cand = CandidateResult::pending("s1-root-cause");
        cand.status = CandidateStatus::Audited;
        cand.repro_passed = true;
        cand.regression_suite_passed = true;
        cand.git_diff = "diff --git a/src/pool.py b/src/pool.py\n--- a/src/pool.py\n+++ b/src/pool.py\n@@ -1 +1 @@\n-old()\n+new()\n".to_string();
        completed.candidates.push(cand);
        state.db.upsert_session(&completed).unwrap();
        "s1-root-cause".to_string()
    };

    let Json(bundle) = export_candidate_bundle(
        State(state.clone()),
        Path((session_id, strategy_id.clone())),
    )
    .await
    .unwrap();

    assert_eq!(bundle.session_id, session_id);
    assert_eq!(bundle.strategy_id, strategy_id);
    assert!(!bundle.audit_markdown.is_empty());
    assert!(bundle.git_apply_command.contains("git apply"));

    let Json(sessions) = list_sessions(State(state)).await.unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].stage, TournamentStage::Completed);
}

#[tokio::test]
async fn session_audio_endpoint_serves_404_when_missing_and_wav_when_stored() {
    let state = test_state();
    let session_id = Uuid::new_v4();

    // Missing -> 404.
    let missing_resp = get_session_audio(State(state.clone()), Path(session_id))
        .await
        .err()
        .expect("should return 404")
        .into_response();
    assert_eq!(missing_resp.status(), StatusCode::NOT_FOUND);

    // Stored -> 200 audio/wav.
    let wav_bytes = b"RIFF\x24\x00\x00\x00WAVEfmt \x10\x00\x00\x00".to_vec();
    state
        .db
        .save_audio_briefing(session_id, &wav_bytes)
        .unwrap();

    let ok_resp = get_session_audio(State(state), Path(session_id))
        .await
        .expect("wav should be served");
    assert_eq!(ok_resp.status(), StatusCode::OK);
    assert_eq!(
        ok_resp.headers().get(header::CONTENT_TYPE).unwrap(),
        "audio/wav"
    );
    let body_bytes = axum::body::to_bytes(ok_resp.into_body(), 1024)
        .await
        .unwrap();
    assert_eq!(body_bytes.as_ref(), wav_bytes.as_slice());
}

#[tokio::test]
async fn sse_endpoint_emits_initial_stage_sync_event() {
    let state = test_state();
    let req = CreateSessionRequest {
        repo_url: "https://github.com/acme/demo".to_string(),
        commit_sha: None,
        git_ref: None,
        task_kind: Some(TaskKind::Bug),
        title: Some("SSE Sync Test".to_string()),
        problem_statement: "Verify reconnect stage sync".to_string(),
        attachment_ids: Vec::new(),
        attachments: Vec::new(),
    };
    let session = TournamentEngine::initialize_session(&req);
    state.db.upsert_session(&session).unwrap();

    let sse = session_events_sse(State(state), Path(session.id))
        .await
        .unwrap();
    let resp = sse.into_response();
    assert_eq!(resp.status(), StatusCode::OK);

    let mut body_stream = resp.into_body().into_data_stream();
    let first_chunk = tokio::time::timeout(Duration::from_secs(2), body_stream.next())
        .await
        .expect("SSE initial frame timed out")
        .expect("stream yielded chunk")
        .expect("chunk bytes ok");
    let frame = String::from_utf8_lossy(&first_chunk);
    assert!(
        frame.contains("stage_changed"),
        "expected initial stage_changed frame, got: {frame}"
    );
    assert!(frame.contains(&session.id.to_string()));
}

#[test]
fn unified_diff_application_handles_modifications_fuzzy_offsets_new_and_deleted_files() {
    let original = "fn alpha() -> i32 {\n    let x = 1;\n    x + 1\n}\n\nfn beta() -> i32 {\n    42\n}\n";
    let diff = "\
diff --git a/src/math.rs b/src/math.rs
--- a/src/math.rs
+++ b/src/math.rs
@@ -1,4 +1,4 @@
 fn alpha() -> i32 {
-    let x = 1;
+    let x = 10;
     x + 1
 }
diff --git a/src/added.rs b/src/added.rs
new file mode 100644
--- /dev/null
+++ b/src/added.rs
@@ -0,0 +1,2 @@
+pub fn hello() -> &'static str {
+    \"world\"
+}
diff --git a/src/obsolete.rs b/src/obsolete.rs
deleted file mode 100644
--- a/src/obsolete.rs
+++ /dev/null
@@ -1,1 +0,0 @@
-pub fn gone() {}
";

    let mut base_files = BTreeMap::new();
    base_files.insert("src/math.rs".to_string(), original.to_string());
    base_files.insert("src/obsolete.rs".to_string(), "pub fn gone() {}\n".to_string());

    let updated = apply_unified_diff_to_map(&base_files, diff).unwrap();
    assert_eq!(
        updated.get("src/math.rs").unwrap().as_deref(),
        Some("fn alpha() -> i32 {\n    let x = 10;\n    x + 1\n}\n\nfn beta() -> i32 {\n    42\n}\n")
    );
    assert_eq!(
        updated.get("src/added.rs").unwrap().as_deref(),
        Some("pub fn hello() -> &'static str {\n    \"world\"\n}\n")
    );
    assert_eq!(updated.get("src/obsolete.rs").unwrap(), &None);

    // Also test fuzzy offset matching when line numbers shifted by +3 lines.
    let shifted_original = format!("// header 1\n// header 2\n// header 3\n{original}");
    let patches = parse_unified_diff(diff).unwrap();
    let patched_shifted = apply_hunks(&shifted_original, &patches[0].hunks).unwrap();
    assert!(patched_shifted.contains("let x = 10;"));
    assert!(patched_shifted.starts_with("// header 1\n"));
}

#[test]
fn github_url_parsers_and_media_extractor_handle_edge_cases() {
    assert_eq!(
        parse_github_repo_url("https://github.com/jzeng151/epicenter.git").unwrap(),
        RepoSlug {
            owner: "jzeng151".to_string(),
            repo: "epicenter".to_string(),
        }
    );
    assert_eq!(
        parse_github_repo_url("git@github.com:encode/httpx.git").unwrap(),
        RepoSlug {
            owner: "encode".to_string(),
            repo: "httpx".to_string(),
        }
    );
    assert_eq!(
        parse_github_repo_url("colinhacks/zod").unwrap(),
        RepoSlug {
            owner: "colinhacks".to_string(),
            repo: "zod".to_string(),
        }
    );

    assert_eq!(
        parse_github_pr_url("https://github.com/encode/httpx/pull/3042/files#diff-123").unwrap(),
        PrSlug {
            owner: "encode".to_string(),
            repo: "httpx".to_string(),
            number: 3042,
        }
    );
    assert!(parse_github_pr_url("https://github.com/encode/httpx/issues/12").is_err());

    assert!(is_test_file("tests/test_client.py"));
    assert!(is_test_file("src/utils/parser.spec.ts"));
    assert!(!is_test_file("src/utils/parser.ts"));

    let md = "Repro screenshot: ![err](https://user-images.githubusercontent.com/1/shot.png) \
              and clip <img src=\"https://github.com/user-attachments/assets/abc-123\" /> \
              plus docs https://example.com/readme.html";
    let urls = extract_media_urls(md);
    assert_eq!(urls.len(), 2);
    assert_eq!(
        urls[0],
        "https://user-images.githubusercontent.com/1/shot.png"
    );
    assert_eq!(
        urls[1],
        "https://github.com/user-attachments/assets/abc-123"
    );
}

#[tokio::test]
async fn live_judge_evaluates_fetched_pr_across_three_arms() {
    let state = test_state();
    let (repo_guard, parent_commit) = create_temp_python_git_repo();
    let mut base_source_files = BTreeMap::new();
    base_source_files.insert(
        "src/calc.py".to_string(),
        std::fs::read_to_string(repo_guard.path().join("src/calc.py")).unwrap(),
    );

    let ctx = FetchedPrContext {
        slug: PrSlug {
            owner: "acme".to_string(),
            repo: "calc".to_string(),
            number: 99,
        },
        repo_url: repo_guard.path().to_string_lossy().to_string(),
        title: "Handle empty envvar sequences cleanly".to_string(),
        body: "See screenshot ![bug](http://127.0.0.1:1/bug.png)".to_string(),
        parent_commit,
        ground_truth_test_patch: temp_repo_ground_truth_test_patch(),
        modified_source_files: vec!["src/calc.py".to_string()],
        base_source_files,
        media_urls: vec!["http://127.0.0.1:1/bug.png".to_string()],
    };

    let eval = evaluate_fetched_pr(&state, ctx).await.unwrap();
    assert_eq!(eval.cases.len(), 1);
    assert_eq!(eval.cases[0].split, EvalSplit::LiveJudge);
    assert!(eval.cases[0].has_multimodal_media);
    assert_eq!(eval.outcomes.len(), 3);
    assert_eq!(eval.scorecard.arm_metrics.len(), 3);
    assert!(eval
        .outcomes
        .iter()
        .any(|o| o.arm == EvalArm::SingleModelCall));
    assert!(eval
        .outcomes
        .iter()
        .any(|o| o.arm == EvalArm::SingleManagedAgent));
    assert!(eval
        .outcomes
        .iter()
        .any(|o| o.arm == EvalArm::EpicenterTournament));
    // Offline stubs produce synthetic diffs that do NOT match this local repo, so honest host
    // verification must NEVER rubber-stamp them as resolved_ground_truth == true.
    assert!(eval.outcomes.iter().all(|o| !o.resolved_ground_truth));
}

#[tokio::test]
async fn attachment_bytes_and_keyframes_endpoints_serve_media_and_handle_non_video() {
    let state = test_state();
    let png_payload = b"\x89PNG\r\n\x1a\nFAKE_PNG_BYTES_12345".to_vec();
    let meta = AttachmentMeta {
        id: Uuid::new_v4(),
        filename: "error-screen.png".to_string(),
        mime_type: "image/png".to_string(),
        media_kind: MediaKind::Screenshot,
        size_bytes: png_payload.len() as u64,
        file_uri: None,
    };
    state.db.insert_attachment(&meta, &png_payload).unwrap();

    // Missing attachment -> 404 for both bytes and keyframes.
    let missing_id = Uuid::new_v4();
    let missing_resp = get_attachment_bytes(State(state.clone()), Path(missing_id))
        .await
        .expect_err("missing attachment should return 404")
        .into_response();
    assert_eq!(missing_resp.status(), StatusCode::NOT_FOUND);

    let missing_kf = get_attachment_keyframes(State(state.clone()), Path(missing_id))
        .await
        .expect_err("missing attachment keyframes should return 404")
        .into_response();
    assert_eq!(missing_kf.status(), StatusCode::NOT_FOUND);

    // GET /api/attachments/{id} serves exact bytes + exact Content-Type + Cache-Control.
    let resp = get_attachment_bytes(State(state.clone()), Path(meta.id))
        .await
        .expect("stored attachment should be served");
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers().get(header::CONTENT_TYPE).unwrap(),
        "image/png"
    );
    assert_eq!(
        resp.headers().get(header::CACHE_CONTROL).unwrap(),
        "public, max-age=3600"
    );
    let served_bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    assert_eq!(served_bytes.as_ref(), png_payload.as_slice());

    // GET /api/attachments/{id}/keyframes returns empty keyframes cleanly for non-video attachments.
    let Json(kf_resp) = get_attachment_keyframes(State(state.clone()), Path(meta.id))
        .await
        .expect("non-video keyframes should succeed with empty list");
    assert_eq!(kf_resp.attachment_id, meta.id);
    assert!(kf_resp.keyframes.is_empty());

    // Also verify corrupt video bytes never return 500 (gracefully empty keyframes).
    let bad_vid = AttachmentMeta {
        id: Uuid::new_v4(),
        filename: "corrupt.mp4".to_string(),
        mime_type: "video/mp4".to_string(),
        media_kind: MediaKind::ScreenRecording,
        size_bytes: 12,
        file_uri: None,
    };
    state.db.insert_attachment(&bad_vid, b"not_a_realmp4").unwrap();
    let Json(corrupt_kf) = get_attachment_keyframes(State(state.clone()), Path(bad_vid.id))
        .await
        .expect("corrupt video should gracefully return empty keyframes instead of 500");
    assert_eq!(corrupt_kf.attachment_id, bad_vid.id);
    assert!(corrupt_kf.keyframes.is_empty());

    // If ffmpeg is available on the host, also test real video keyframe extraction end-to-end.
    let ffmpeg_bin = if std::path::Path::new("/opt/homebrew/bin/ffmpeg").exists() {
        "/opt/homebrew/bin/ffmpeg"
    } else {
        "ffmpeg"
    };
    if let Ok(vid_dir) = TempDirGuard::new("test-mp4") {
        let mp4_path = vid_dir.path().join("sample.mp4");
        let gen = std::process::Command::new(ffmpeg_bin)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "color=c=0x161b22:s=320x180:d=2.2",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(&mp4_path)
            .status();
        if gen.is_ok_and(|s| s.success()) {
            let mp4_bytes = std::fs::read(&mp4_path).unwrap();
            let vid_meta = AttachmentMeta {
                id: Uuid::new_v4(),
                filename: "demo.mp4".to_string(),
                mime_type: "video/mp4".to_string(),
                media_kind: MediaKind::ScreenRecording,
                size_bytes: mp4_bytes.len() as u64,
                file_uri: None,
            };
            state.db.insert_attachment(&vid_meta, &mp4_bytes).unwrap();
            let Json(real_kf) = get_attachment_keyframes(State(state), Path(vid_meta.id))
                .await
                .unwrap();
            assert_eq!(real_kf.attachment_id, vid_meta.id);
            assert!(
                !real_kf.keyframes.is_empty() && real_kf.keyframes.len() <= 4,
                "expected 1..=4 keyframes from real MP4, got {}",
                real_kf.keyframes.len()
            );
            assert_eq!(real_kf.keyframes[0].index, 0);
            assert!(real_kf.keyframes[0]
                .data_url
                .starts_with("data:image/png;base64,iVBORw0KGgo"));
        }
    }
}

#[tokio::test]
async fn real_bug_preset_returns_pallets_click_3884_and_serves_valid_png() {
    let state = test_state();
    let Json(preset) = create_real_bug_preset(State(state.clone()))
        .await
        .expect("preset creation should succeed");

    assert_eq!(preset.repo_url, "https://github.com/pallets/click");
    assert_eq!(
        preset.commit_sha,
        "bb695aaecdef95832d167fc1d508921745aa824e"
    );
    assert!(preset.git_ref.is_none());
    assert_eq!(preset.pr_url, "https://github.com/pallets/click/pull/3884");
    assert_eq!(preset.task_kind, TaskKind::Bug);
    assert!(preset.title.contains("pallets/click#3884"));
    assert!(preset.problem_statement.contains("show_envvar"));
    assert_eq!(
        preset.attachment.filename,
        "click-3884-envvar-terminal-bug.png"
    );
    assert_eq!(preset.attachment.mime_type, "image/png");
    assert_eq!(preset.attachment.media_kind, MediaKind::Screenshot);
    assert!(preset.attachment.size_bytes > 100);

    // Verify stored PNG is retrievable via GET /api/attachments/{id} and has valid PNG signature/chunks.
    let resp = get_attachment_bytes(State(state), Path(preset.attachment.id))
        .await
        .expect("preset PNG should be retrievable");
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers().get(header::CONTENT_TYPE).unwrap(),
        "image/png"
    );
    let png_bytes = axum::body::to_bytes(resp.into_body(), 10 * 1024 * 1024)
        .await
        .unwrap();
    assert_eq!(png_bytes.len() as u64, preset.attachment.size_bytes);
    assert!(
        png_bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "preset attachment must start with valid PNG magic signature"
    );
    assert_eq!(&png_bytes[12..16], b"IHDR");
    assert!(
        png_bytes.windows(4).any(|w| w == b"IDAT"),
        "PNG must contain IDAT chunk"
    );
    assert!(
        png_bytes.windows(4).any(|w| w == b"IEND"),
        "PNG must contain IEND chunk"
    );

    // If ffmpeg is present, verify it decodes our pure-Rust PNG with zero CRC/zlib errors.
    let ffmpeg_bin = if std::path::Path::new("/opt/homebrew/bin/ffmpeg").exists() {
        "/opt/homebrew/bin/ffmpeg"
    } else {
        "ffmpeg"
    };
    if let Ok(check_dir) = TempDirGuard::new("png-decode-check") {
        let png_path = check_dir.path().join("preset.png");
        std::fs::write(&png_path, &png_bytes).unwrap();
        if let Ok(out) = std::process::Command::new(ffmpeg_bin)
            .args(["-v", "error", "-i"])
            .arg(&png_path)
            .args(["-f", "null", "-"])
            .output()
        {
            assert!(
                out.status.success(),
                "ffmpeg PNG decode failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            assert!(
                out.stderr.is_empty(),
                "ffmpeg reported PNG decode warnings/errors: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
}

#[tokio::test]
async fn router_serves_preset_attachment_keyframes_and_media_downloader_fetches_it() {
    let state = test_state();
    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    // POST /api/presets/real-bug
    let preset_resp = client
        .post(format!("{base}/api/presets/real-bug"))
        .send()
        .await
        .unwrap();
    assert_eq!(preset_resp.status(), reqwest::StatusCode::OK);
    let preset_json: serde_json::Value = preset_resp.json().await.unwrap();
    assert_eq!(preset_json["repo_url"], "https://github.com/pallets/click");
    assert_eq!(
        preset_json["commit_sha"],
        "bb695aaecdef95832d167fc1d508921745aa824e"
    );
    assert!(preset_json["git_ref"].is_null());
    assert_eq!(preset_json["task_kind"], "bug");
    let att_id = preset_json["attachment"]["id"].as_str().unwrap();

    // GET /api/attachments/{id}
    let media_url = format!("{base}/api/attachments/{att_id}");
    let media_resp = client.get(&media_url).send().await.unwrap();
    assert_eq!(media_resp.status(), reqwest::StatusCode::OK);
    assert_eq!(
        media_resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .unwrap(),
        "image/png"
    );
    let raw_bytes = media_resp.bytes().await.unwrap();
    assert!(raw_bytes.starts_with(b"\x89PNG\r\n\x1a\n"));

    // GET /api/attachments/{id}/keyframes
    let kf_resp = client
        .get(format!("{base}/api/attachments/{att_id}/keyframes"))
        .send()
        .await
        .unwrap();
    assert_eq!(kf_resp.status(), reqwest::StatusCode::OK);
    let kf_json: serde_json::Value = kf_resp.json().await.unwrap();
    assert_eq!(kf_json["attachment_id"], att_id);
    assert_eq!(kf_json["keyframes"].as_array().unwrap().len(), 0);

    // download_pr_media_attachments fetches real HTTP media for Arm 3.
    let downloaded = download_pr_media_attachments(&[media_url]).await;
    assert_eq!(downloaded.len(), 1);
    assert_eq!(downloaded[0].meta.mime_type, "image/png");
    assert_eq!(downloaded[0].meta.media_kind, MediaKind::Screenshot);
    assert_eq!(downloaded[0].bytes.len(), raw_bytes.len());

    server.abort();
}

#[tokio::test]
async fn eval_latest_refuses_simulated_seed_and_returns_only_live_runs() {
    assert!(is_simulated_eval_label("offline-dry-run-baseline-seed"));
    assert!(is_simulated_eval_label("Offline Run #1"));
    assert!(is_simulated_eval_label("Seeded 5-Sigma Run"));
    assert!(is_simulated_eval_label("SIMULATED benchmark"));
    assert!(!is_simulated_eval_label("Live Judge: pallets/click#3884"));
    assert!(!is_simulated_eval_label("Live 5-Sigma Eval v1"));

    // 1. Disk seed with "offline-dry-run-baseline-seed" MUST be refused (returns None, not seeded).
    let sim_eval = sample_eval_run_with_label("offline-dry-run-baseline-seed");
    let seed_path =
        std::env::temp_dir().join(format!("epicenter_sim_seed_{}.json", Uuid::new_v4()));
    std::fs::write(&seed_path, serde_json::to_string_pretty(&sim_eval).unwrap()).unwrap();

    let db = DbStore::open_in_memory()
        .unwrap()
        .with_eval_seed_path(&seed_path);
    assert!(
        db.get_latest_eval_run_or_seed().unwrap().is_none(),
        "must refuse to auto-seed or return offline-dry-run-baseline-seed"
    );
    assert!(
        db.get_eval_run(sim_eval.run_id).unwrap().is_none(),
        "simulated disk file must not be persisted into SQLite"
    );
    let _ = std::fs::remove_file(&seed_path);

    // 2. If SQLite only has simulated rows, get_latest_eval_run_or_seed() MUST return None.
    let mut sim_db_row = sample_eval_run_with_label("Simulated Run");
    sim_db_row.created_at = Utc::now();
    db.upsert_eval_run(&sim_db_row).unwrap();
    assert!(db.get_latest_eval_run_or_seed().unwrap().is_none());

    // 3. When a real live eval run exists alongside a newer simulated row, return the real live run!
    let mut live_run = sample_eval_run_with_label("Live Judge: pallets/click#3884");
    live_run.created_at = Utc::now() - ChronoDuration::minutes(5);
    db.upsert_eval_run(&live_run).unwrap();

    let latest = db
        .get_latest_eval_run_or_seed()
        .unwrap()
        .expect("must return real live eval run");
    assert_eq!(latest.run_id, live_run.run_id);
    assert_eq!(latest.label, "Live Judge: pallets/click#3884");
}

#[tokio::test]
async fn host_ground_truth_grader_verifies_pass_fail_conflict_and_tampering() {
    let (repo_guard, parent_commit) = create_temp_python_git_repo();
    let cache_guard = TempDirGuard::new("judge-cache-test").unwrap();
    let repo_url = repo_guard.path().to_string_lossy().to_string();
    let gt_patch = temp_repo_ground_truth_test_patch();

    // Case 1: Passing fix (fixes empty/sequence formatting AND keeps existing regression suite green).
    let passing_diff = "\
diff --git a/src/calc.py b/src/calc.py
--- a/src/calc.py
+++ b/src/calc.py
@@ -1,4 +1,9 @@
 def format_envvar(envvar):
     if envvar is None:
         return ''
+    if isinstance(envvar, (list, tuple)):
+        parts = [f\"'{v}'\" for v in envvar if v]
+        return f\"(env var: {', '.join(parts)})\" if parts else ''
+    if not envvar:
+        return ''
     return f\"(env var: '{envvar}')\"
";
    let pass_res = verify_candidate_on_host_with_cache(
        cache_guard.path(),
        &repo_url,
        &parent_commit,
        passing_diff,
        &gt_patch,
    )
    .await;
    assert!(pass_res.patch_applied, "passing diff must apply cleanly: {pass_res:?}");
    assert!(pass_res.ground_truth_patch_applied);
    assert!(
        pass_res.resolved_ground_truth,
        "passing fix must pass ground-truth tests: {pass_res:?}"
    );
    assert!(
        pass_res.regression_free,
        "passing fix must keep module regression suite green: {pass_res:?}"
    );
    assert!(!pass_res.test_tampering);
    assert!(pass_res.failure_category.is_none());
    let blast = pass_res
        .blast_radius
        .as_ref()
        .expect("real checked-out blast radius should be computed");
    assert!(
        !blast.ring0_mutated.is_empty(),
        "ring0 should include mutated format_envvar symbol"
    );
    assert!(
        !blast.ring1_direct_callers.is_empty(),
        "ring1 should discover direct caller render_error_hint from src/service.py"
    );

    // Case 2: Failing fix (applies cleanly, doesn't break existing tests, but fails hidden ground-truth test).
    let failing_diff = "\
diff --git a/src/calc.py b/src/calc.py
--- a/src/calc.py
+++ b/src/calc.py
@@ -1,4 +1,6 @@
 def format_envvar(envvar):
     if envvar is None:
         return ''
+    if envvar == '':
+        return ''
     return f\"(env var: '{envvar}')\"
";
    let fail_res = verify_candidate_on_host_with_cache(
        cache_guard.path(),
        &repo_url,
        &parent_commit,
        failing_diff,
        &gt_patch,
    )
    .await;
    assert!(fail_res.patch_applied);
    assert!(fail_res.ground_truth_patch_applied);
    assert!(
        !fail_res.resolved_ground_truth,
        "incomplete fix must fail ground-truth tests"
    );
    assert!(
        fail_res.regression_free,
        "existing regression tests should still pass when reverted gt patch isolates new test"
    );
    assert_eq!(
        fail_res.failure_category.as_deref(),
        Some("ground_truth_tests_failed")
    );

    // Case 3: Regression-introducing fix (passes new edge cases but breaks existing single-string behavior).
    let regressing_diff = "\
diff --git a/src/calc.py b/src/calc.py
--- a/src/calc.py
+++ b/src/calc.py
@@ -1,4 +1,9 @@
 def format_envvar(envvar):
     if envvar is None:
         return ''
+    if isinstance(envvar, (list, tuple)):
+        parts = [f\"'{v}'\" for v in envvar if v]
+        return f\"(env var: {', '.join(parts)})\" if parts else ''
+    if not envvar:
+        return ''
-    return f\"(env var: '{envvar}')\"
+    return f\"(env var: '{envvar.upper()}')\"
";
    let reg_res = verify_candidate_on_host_with_cache(
        cache_guard.path(),
        &repo_url,
        &parent_commit,
        regressing_diff,
        &gt_patch,
    )
    .await;
    assert!(reg_res.patch_applied);
    assert!(reg_res.resolved_ground_truth);
    assert!(
        !reg_res.regression_free,
        "must detect regression in existing tests/test_service.py"
    );
    assert_eq!(
        reg_res.failure_category.as_deref(),
        Some("regression_suite_failed")
    );

    // Case 4: Patch apply conflict.
    let conflict_diff = "\
diff --git a/src/calc.py b/src/calc.py
--- a/src/calc.py
+++ b/src/calc.py
@@ -1,3 +1,3 @@
-def nonexistent_function():
+def still_nonexistent():
     return 999
";
    let conflict_res = verify_candidate_on_host_with_cache(
        cache_guard.path(),
        &repo_url,
        &parent_commit,
        conflict_diff,
        &gt_patch,
    )
    .await;
    assert!(!conflict_res.patch_applied);
    assert!(!conflict_res.resolved_ground_truth);
    assert!(!conflict_res.regression_free);
    assert_eq!(
        conflict_res.failure_category.as_deref(),
        Some("patch_apply_conflict")
    );

    // Case 5: Test tampering violation (candidate tries to modify locked ground-truth test file).
    let tampering_diff = "\
diff --git a/src/calc.py b/src/calc.py
--- a/src/calc.py
+++ b/src/calc.py
@@ -1,4 +1,4 @@
 def format_envvar(envvar):
     if envvar is None:
         return ''
-    return f\"(env var: '{envvar}')\"
+    return ''
diff --git a/tests/test_calc.py b/tests/test_calc.py
--- a/tests/test_calc.py
+++ b/tests/test_calc.py
@@ -1,4 +1,4 @@
 from calc import format_envvar
 
 def test_single_envvar():
-    assert format_envvar('API_KEY') == \"(env var: 'API_KEY')\"
+    assert True
";
    let tamper_res = verify_candidate_on_host_with_cache(
        cache_guard.path(),
        &repo_url,
        &parent_commit,
        tampering_diff,
        &gt_patch,
    )
    .await;
    assert!(tamper_res.test_tampering);
    assert!(!tamper_res.patch_applied);
    assert!(!tamper_res.resolved_ground_truth);
    assert!(!tamper_res.regression_free);
    assert_eq!(
        tamper_res.failure_category.as_deref(),
        Some("test_tampering_violation")
    );
}

#[test]
fn media_helpers_base64_and_pth_stripping_work_cleanly() {
    assert_eq!(base64_encode(b""), "");
    assert_eq!(base64_encode(b"f"), "Zg==");
    assert_eq!(base64_encode(b"fo"), "Zm8=");
    assert_eq!(base64_encode(b"foo"), "Zm9v");
    assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    assert_eq!(base64_encode(b"\x89PNG\r\n\x1a\n"), "iVBORw0KGgo=");

    let fake_venv = TempDirGuard::new("pth-strip-test").unwrap();
    let sp = fake_venv.path().join("lib/python3.12/site-packages");
    std::fs::create_dir_all(&sp).unwrap();
    std::fs::write(sp.join("_virtualenv.pth"), "import _virtualenv\n").unwrap();
    std::fs::write(sp.join("__editable__.click-8.3.0.pth"), "/tmp/epicenter-cache/repos/pallets__click/src\n").unwrap();
    std::fs::write(sp.join("__editable___click_8_3_0_finder.py"), "def install(): pass\n").unwrap();
    std::fs::write(sp.join("click.pth"), "/tmp/epicenter-wt-123/src\n").unwrap();
    std::fs::write(sp.join("pkg.egg-link"), "/tmp/epicenter-cache/repos/pallets__click\n").unwrap();

    strip_editable_pth_files(fake_venv.path());

    assert!(sp.join("_virtualenv.pth").exists(), "standard _virtualenv.pth must be preserved");
    assert!(!sp.join("__editable__.click-8.3.0.pth").exists());
    assert!(!sp.join("__editable___click_8_3_0_finder.py").exists());
    assert!(!sp.join("click.pth").exists());
    assert!(!sp.join("pkg.egg-link").exists());
}

fn create_temp_python_git_repo() -> (TempDirGuard, String) {
    let dir = TempDirGuard::new("local-py-repo").unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("tests")).unwrap();

    std::fs::write(
        root.join("src/calc.py"),
        "def format_envvar(envvar):\n    if envvar is None:\n        return ''\n    return f\"(env var: '{envvar}')\"\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/service.py"),
        "from calc import format_envvar\n\ndef render_error_hint(name, envvar):\n    hint = format_envvar(envvar)\n    return f\"{name} {hint}\".strip()\n",
    )
    .unwrap();
    std::fs::write(
        root.join("tests/test_calc.py"),
        "from calc import format_envvar\n\ndef test_single_envvar():\n    assert format_envvar('API_KEY') == \"(env var: 'API_KEY')\"\n",
    )
    .unwrap();
    std::fs::write(
        root.join("tests/test_service.py"),
        "from service import render_error_hint\n\ndef test_render_error_hint_preserves_case():\n    assert render_error_hint('--token', 'my_token') == \"--token (env var: 'my_token')\"\n",
    )
    .unwrap();

    let run = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .expect("git command failed");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };

    run(&["init"]);
    run(&["config", "user.email", "test@epicenter.local"]);
    run(&["config", "user.name", "Epicenter Test"]);
    run(&["add", "."]);
    run(&["commit", "-m", "initial parent commit"]);
    let sha = run(&["rev-parse", "HEAD"]);
    (dir, sha)
}

fn temp_repo_ground_truth_test_patch() -> String {
    "\
diff --git a/tests/test_calc.py b/tests/test_calc.py
--- a/tests/test_calc.py
+++ b/tests/test_calc.py
@@ -2,3 +2,11 @@ from calc import format_envvar
 
 def test_single_envvar():
     assert format_envvar('API_KEY') == \"(env var: 'API_KEY')\"
+
+def test_empty_envvar_omitted():
+    assert format_envvar('') == ''
+    assert format_envvar([]) == ''
+
+def test_sequence_envvar_formatted():
+    assert format_envvar(['WEATHER_UNITS', 'UNITS']) == \"(env var: 'WEATHER_UNITS', 'UNITS')\"
"
    .to_string()
}

fn sample_eval_run() -> FiveSigmaEval {
    sample_eval_run_with_label("Live 5-Sigma Eval Run")
}

fn sample_eval_run_with_label(label: &str) -> FiveSigmaEval {
    let cases = vec![EvalIssueCase {
        id: "httpx-1".to_string(),
        repo_url: "https://github.com/encode/httpx".to_string(),
        issue_number: 1,
        pr_number: Some(2),
        title: "Sample PR".to_string(),
        issue_body: "Fix timeout handling".to_string(),
        parent_commit: "deadbeef".to_string(),
        ground_truth_test_patch: String::new(),
        has_multimodal_media: true,
        media_urls: vec!["https://example.com/demo.gif".to_string()],
        split: EvalSplit::LockedUnseen,
    }];
    let outcomes = vec![ArmOutcome {
        case_id: "httpx-1".to_string(),
        arm: EvalArm::EpicenterTournament,
        resolved_ground_truth: true,
        regression_free: true,
        speedup_pct: Some(24.5),
        blast_risk_score: Some(12.0),
        root_cause_depth_score: Some(88.0),
        dark_zone_touches: Some(0),
        duration_ms: 1200,
        notes: Some("Passed all ground-truth tests".to_string()),
        failure_category: None,
    }];
    let scorecard = EvalScorecard::from_outcomes(&cases, &outcomes);
    FiveSigmaEval {
        run_id: Uuid::new_v4(),
        label: label.to_string(),
        cases,
        outcomes,
        scorecard,
        calibration_findings: Vec::new(),
        remaining_failure_modes: Vec::new(),
        created_at: Utc::now(),
    }
}
