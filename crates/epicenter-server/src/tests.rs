use crate::db::{infer_media_kind, DbStore};
use crate::github::{
    apply_hunks, apply_unified_diff_to_map, extract_media_urls, is_test_file,
    parse_github_pr_url, parse_github_repo_url, parse_unified_diff, FetchedPrContext,
    GitHubClient, PrSlug, RepoSlug,
};
use crate::live_judge::evaluate_fetched_pr;
use crate::routes::{
    build_router, create_session, export_candidate_bundle, get_eval_by_id, get_latest_eval,
    get_session, get_session_audio, health_handler, list_sessions, session_events_sse,
    upload_attachment,
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
    let mut base_source_files = BTreeMap::new();
    base_source_files.insert(
        "src/router.py".to_string(),
        "def dispatch(path):\n    return handle(path)\n\ndef handle(path):\n    return path.strip('/')\n".to_string(),
    );

    let ctx = FetchedPrContext {
        slug: PrSlug {
            owner: "acme".to_string(),
            repo: "router".to_string(),
            number: 99,
        },
        repo_url: "https://github.com/acme/router".to_string(),
        title: "Handle trailing slashes safely".to_string(),
        body: "See screenshot ![bug](https://example.com/bug.png)".to_string(),
        parent_commit: "0123456789abcdef".to_string(),
        ground_truth_test_patch: "diff --git a/tests/test_router.py b/tests/test_router.py\n--- a/tests/test_router.py\n+++ b/tests/test_router.py\n@@ -1 +1 @@\n-assert True\n+assert handle('/') == ''\n".to_string(),
        modified_source_files: vec!["src/router.py".to_string()],
        base_source_files,
        media_urls: vec!["https://example.com/bug.png".to_string()],
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
}

fn sample_eval_run() -> FiveSigmaEval {
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
        label: "Seeded 5-Sigma Run".to_string(),
        cases,
        outcomes,
        scorecard,
        calibration_findings: Vec::new(),
        remaining_failure_modes: Vec::new(),
        created_at: Utc::now(),
    }
}
