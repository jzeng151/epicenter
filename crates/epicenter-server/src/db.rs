use anyhow::{Context, Result};
use chrono::Utc;
use epicenter_common::{
    AttachmentMeta, FiveSigmaEval, MediaKind, SessionListItem, TournamentSession,
};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

/// Thread-safe SQLite persistence layer for sessions, multimodal attachments,
/// synthesized WAV briefings, and 5-Sigma evaluation runs.
#[derive(Clone)]
pub struct DbStore {
    conn: Arc<Mutex<Connection>>,
    eval_seed_path: PathBuf,
}

impl DbStore {
    /// Opens (or creates) an on-disk SQLite database at `path` and initializes tables.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).with_context(|| {
                    format!("failed to create parent dir for db {}", parent.display())
                })?;
            }
        }
        let conn = Connection::open(path)
            .with_context(|| format!("failed to open sqlite db at {}", path.display()))?;
        let _ = conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;");
        Self::from_connection(conn, PathBuf::from("eval_runs/latest_five_sigma.json"))
    }

    /// Opens an in-memory SQLite database (ideal for tests).
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().context("failed to open in-memory sqlite db")?;
        Self::from_connection(conn, PathBuf::from("eval_runs/latest_five_sigma.json"))
    }

    /// Overrides the fallback disk path checked when `eval_runs` is empty.
    pub fn with_eval_seed_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.eval_seed_path = path.into();
        self
    }

    fn from_connection(conn: Connection, eval_seed_path: PathBuf) -> Result<Self> {
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                json_blob TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_sessions_created_at
                ON sessions (created_at DESC);

            CREATE TABLE IF NOT EXISTS attachments (
                id TEXT PRIMARY KEY,
                filename TEXT NOT NULL,
                mime_type TEXT NOT NULL,
                media_kind TEXT NOT NULL,
                size_bytes INTEGER NOT NULL,
                file_uri TEXT,
                bytes BLOB NOT NULL,
                created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS audio_briefings (
                session_id TEXT PRIMARY KEY,
                wav_bytes BLOB NOT NULL,
                created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS eval_runs (
                run_id TEXT PRIMARY KEY,
                created_at TEXT NOT NULL,
                json_blob TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_eval_runs_created_at
                ON eval_runs (created_at DESC);
            "#,
        )
        .context("failed to initialize sqlite schema")?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            eval_seed_path,
        })
    }

    /// Inserts or updates a full `TournamentSession` snapshot.
    pub fn upsert_session(&self, session: &TournamentSession) -> Result<()> {
        let id = session.id.to_string();
        let created_at = session.created_at.to_rfc3339();
        let updated_at = session.updated_at.to_rfc3339();
        let json_blob =
            serde_json::to_string(session).context("failed to serialize TournamentSession")?;

        let conn = self.lock_conn()?;
        conn.execute(
            r#"
            INSERT INTO sessions (id, created_at, updated_at, json_blob)
            VALUES (?1, ?2, ?3, ?4)
            ON CONFLICT(id) DO UPDATE SET
                updated_at = excluded.updated_at,
                json_blob = excluded.json_blob
            "#,
            params![id, created_at, updated_at, json_blob],
        )
        .context("failed to upsert session")?;
        Ok(())
    }

    /// Loads a `TournamentSession` by UUID.
    pub fn get_session(&self, id: Uuid) -> Result<Option<TournamentSession>> {
        let conn = self.lock_conn()?;
        let json_blob: Option<String> = conn
            .query_row(
                "SELECT json_blob FROM sessions WHERE id = ?1",
                params![id.to_string()],
                |row| row.get(0),
            )
            .optional()
            .context("failed to query session")?;

        match json_blob {
            Some(blob) => {
                let session: TournamentSession =
                    serde_json::from_str(&blob).context("failed to deserialize session")?;
                Ok(Some(session))
            }
            None => Ok(None),
        }
    }

    /// Lists all persisted sessions, newest first.
    pub fn list_sessions(&self) -> Result<Vec<SessionListItem>> {
        let conn = self.lock_conn()?;
        let mut stmt = conn
            .prepare("SELECT json_blob FROM sessions ORDER BY created_at DESC")
            .context("failed to prepare list_sessions query")?;

        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .context("failed to query sessions")?;

        let mut items = Vec::new();
        for row in rows {
            let blob = row.context("failed to read session row")?;
            let session: TournamentSession =
                serde_json::from_str(&blob).context("failed to deserialize session list row")?;
            items.push(SessionListItem {
                id: session.id,
                title: session.intake.title,
                repo_url: session.intake.repo_url,
                task_kind: session.intake.task_kind,
                stage: session.stage,
                candidate_count: session.candidates.len(),
                created_at: session.created_at,
            });
        }
        Ok(items)
    }

    /// Persists an uploaded multimodal attachment's metadata and raw bytes.
    pub fn insert_attachment(&self, meta: &AttachmentMeta, bytes: &[u8]) -> Result<()> {
        let media_kind_json =
            serde_json::to_string(&meta.media_kind).context("failed to serialize MediaKind")?;
        let created_at = Utc::now().to_rfc3339();
        let conn = self.lock_conn()?;
        conn.execute(
            r#"
            INSERT INTO attachments (
                id, filename, mime_type, media_kind, size_bytes, file_uri, bytes, created_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            ON CONFLICT(id) DO UPDATE SET
                filename = excluded.filename,
                mime_type = excluded.mime_type,
                media_kind = excluded.media_kind,
                size_bytes = excluded.size_bytes,
                file_uri = excluded.file_uri,
                bytes = excluded.bytes
            "#,
            params![
                meta.id.to_string(),
                meta.filename,
                meta.mime_type,
                media_kind_json,
                meta.size_bytes as i64,
                meta.file_uri,
                bytes,
                created_at
            ],
        )
        .context("failed to insert attachment")?;
        Ok(())
    }

    /// Loads a single attachment's metadata and raw bytes by UUID.
    pub fn get_attachment(&self, id: Uuid) -> Result<Option<(AttachmentMeta, Vec<u8>)>> {
        let conn = self.lock_conn()?;
        let record = conn
            .query_row(
                "SELECT id, filename, mime_type, media_kind, size_bytes, file_uri, bytes FROM attachments WHERE id = ?1",
                params![id.to_string()],
                |row| {
                    let id_str: String = row.get(0)?;
                    let filename: String = row.get(1)?;
                    let mime_type: String = row.get(2)?;
                    let media_kind_str: String = row.get(3)?;
                    let size_bytes: i64 = row.get(4)?;
                    let file_uri: Option<String> = row.get(5)?;
                    let bytes: Vec<u8> = row.get(6)?;
                    Ok((
                        id_str,
                        filename,
                        mime_type,
                        media_kind_str,
                        size_bytes,
                        file_uri,
                        bytes,
                    ))
                },
            )
            .optional()
            .context("failed to fetch attachment")?;

        Ok(record.map(
            |(id_str, filename, mime_type, media_kind_str, size_bytes, file_uri, bytes)| {
                let parsed_id = Uuid::parse_str(&id_str).unwrap_or(id);
                let media_kind: MediaKind =
                    serde_json::from_str(&media_kind_str).unwrap_or(MediaKind::Other);
                let meta = AttachmentMeta {
                    id: parsed_id,
                    filename,
                    mime_type,
                    media_kind,
                    size_bytes: size_bytes.max(0) as u64,
                    file_uri,
                };
                (meta, bytes)
            },
        ))
    }

    /// Loads attachment metadata and raw bytes for the requested attachment IDs (preserving input order).
    pub fn get_attachments_by_ids(&self, ids: &[Uuid]) -> Result<Vec<(AttachmentMeta, Vec<u8>)>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.lock_conn()?;
        let mut results = Vec::with_capacity(ids.len());
        let mut stmt = conn
            .prepare(
                "SELECT id, filename, mime_type, media_kind, size_bytes, file_uri, bytes FROM attachments WHERE id = ?1",
            )
            .context("failed to prepare attachment lookup")?;

        for id in ids {
            let record = stmt
                .query_row(params![id.to_string()], |row| {
                    let id_str: String = row.get(0)?;
                    let filename: String = row.get(1)?;
                    let mime_type: String = row.get(2)?;
                    let media_kind_str: String = row.get(3)?;
                    let size_bytes: i64 = row.get(4)?;
                    let file_uri: Option<String> = row.get(5)?;
                    let bytes: Vec<u8> = row.get(6)?;
                    Ok((
                        id_str,
                        filename,
                        mime_type,
                        media_kind_str,
                        size_bytes,
                        file_uri,
                        bytes,
                    ))
                })
                .optional()
                .context("failed to fetch attachment")?;

            if let Some((id_str, filename, mime_type, media_kind_str, size_bytes, file_uri, bytes)) =
                record
            {
                let parsed_id = Uuid::parse_str(&id_str).unwrap_or(*id);
                let media_kind: MediaKind =
                    serde_json::from_str(&media_kind_str).unwrap_or(MediaKind::Other);
                let meta = AttachmentMeta {
                    id: parsed_id,
                    filename,
                    mime_type,
                    media_kind,
                    size_bytes: size_bytes.max(0) as u64,
                    file_uri,
                };
                results.push((meta, bytes));
            }
        }
        Ok(results)
    }

    /// Stores synthesized WAV bytes for a session's war-room audio briefing.
    pub fn save_audio_briefing(&self, session_id: Uuid, wav_bytes: &[u8]) -> Result<()> {
        let created_at = Utc::now().to_rfc3339();
        let conn = self.lock_conn()?;
        conn.execute(
            r#"
            INSERT INTO audio_briefings (session_id, wav_bytes, created_at)
            VALUES (?1, ?2, ?3)
            ON CONFLICT(session_id) DO UPDATE SET
                wav_bytes = excluded.wav_bytes,
                created_at = excluded.created_at
            "#,
            params![session_id.to_string(), wav_bytes, created_at],
        )
        .context("failed to save audio briefing")?;
        Ok(())
    }

    /// Retrieves synthesized WAV bytes for a session if available.
    pub fn get_audio_briefing(&self, session_id: Uuid) -> Result<Option<Vec<u8>>> {
        let conn = self.lock_conn()?;
        let bytes: Option<Vec<u8>> = conn
            .query_row(
                "SELECT wav_bytes FROM audio_briefings WHERE session_id = ?1",
                params![session_id.to_string()],
                |row| row.get(0),
            )
            .optional()
            .context("failed to query audio briefing")?;
        Ok(bytes)
    }

    /// Inserts or updates a `FiveSigmaEval` run record.
    pub fn upsert_eval_run(&self, eval: &FiveSigmaEval) -> Result<()> {
        let run_id = eval.run_id.to_string();
        let created_at = eval.created_at.to_rfc3339();
        let json_blob = serde_json::to_string(eval).context("failed to serialize FiveSigmaEval")?;

        let conn = self.lock_conn()?;
        conn.execute(
            r#"
            INSERT INTO eval_runs (run_id, created_at, json_blob)
            VALUES (?1, ?2, ?3)
            ON CONFLICT(run_id) DO UPDATE SET
                created_at = excluded.created_at,
                json_blob = excluded.json_blob
            "#,
            params![run_id, created_at, json_blob],
        )
        .context("failed to upsert eval_run")?;
        Ok(())
    }

    /// Fetches a specific `FiveSigmaEval` by `run_id`.
    pub fn get_eval_run(&self, run_id: Uuid) -> Result<Option<FiveSigmaEval>> {
        let conn = self.lock_conn()?;
        let json_blob: Option<String> = conn
            .query_row(
                "SELECT json_blob FROM eval_runs WHERE run_id = ?1",
                params![run_id.to_string()],
                |row| row.get(0),
            )
            .optional()
            .context("failed to query eval_run")?;

        match json_blob {
            Some(blob) => {
                let eval: FiveSigmaEval =
                    serde_json::from_str(&blob).context("failed to deserialize FiveSigmaEval")?;
                Ok(Some(eval))
            }
            None => Ok(None),
        }
    }

    /// Returns the newest non-simulated `FiveSigmaEval` from SQLite. If no non-simulated row exists
    /// in SQLite (whether `eval_runs` is empty or contains only stale simulated rows), falls through
    /// to check `eval_seed_path` on disk and only seeds/returns it when its label is NOT simulated.
    /// Never presents dry-run, offline, seed, or simulated results as real evaluations.
    pub fn get_latest_eval_run_or_seed(&self) -> Result<Option<FiveSigmaEval>> {
        let blobs: Vec<String> = {
            let conn = self.lock_conn()?;
            let mut stmt = conn
                .prepare("SELECT json_blob FROM eval_runs ORDER BY created_at DESC")
                .context("failed to prepare eval_runs query")?;
            let rows = stmt
                .query_map([], |row| row.get::<_, String>(0))
                .context("failed to query eval_runs")?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row.context("failed to read eval_run row")?);
            }
            out
        };

        for blob in blobs {
            let eval: FiveSigmaEval =
                serde_json::from_str(&blob).context("failed to deserialize eval_run")?;
            if !is_simulated_eval_label(&eval.label) {
                return Ok(Some(eval));
            }
        }

        if self.eval_seed_path.exists() {
            let raw = std::fs::read_to_string(&self.eval_seed_path).with_context(|| {
                format!(
                    "failed to read eval seed file {}",
                    self.eval_seed_path.display()
                )
            })?;
            let eval: FiveSigmaEval = serde_json::from_str(&raw).with_context(|| {
                format!(
                    "failed to parse FiveSigmaEval from {}",
                    self.eval_seed_path.display()
                )
            })?;
            if is_simulated_eval_label(&eval.label) {
                return Ok(None);
            }
            self.upsert_eval_run(&eval)?;
            return Ok(Some(eval));
        }

        Ok(None)
    }

    fn lock_conn(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|_| anyhow::anyhow!("sqlite connection mutex poisoned"))
    }
}

/// Returns `true` when an evaluation run label indicates a dry-run, offline, seed, or simulated run
/// that must never be surfaced as real live benchmark data.
pub fn is_simulated_eval_label(label: &str) -> bool {
    let lower = label.to_ascii_lowercase();
    lower.contains("dry-run")
        || lower.contains("offline")
        || lower.contains("seed")
        || lower.contains("simulated")
}

/// Infers `MediaKind` from a MIME content-type string.
pub fn infer_media_kind(mime_type: &str) -> MediaKind {
    let normalized = mime_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();

    if normalized.starts_with("video/") {
        MediaKind::ScreenRecording
    } else if normalized.starts_with("image/") {
        MediaKind::Screenshot
    } else if normalized == "application/pdf" {
        MediaKind::Pdf
    } else if normalized.starts_with("audio/") {
        MediaKind::VoiceNote
    } else if normalized.starts_with("text/")
        || normalized == "application/json"
        || normalized == "application/x-ndjson"
    {
        MediaKind::Log
    } else {
        MediaKind::Other
    }
}
