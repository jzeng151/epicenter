use crate::error::GeminiError;
use crate::sse::{InteractionStreamEvent, SseParser};
use crate::types::{
    AgentResource, CreateAgentRequest, CreateInteractionRequest, FileRef, InteractionResponse,
    InteractionStatus, ResponseFormat, UploadFileResponse,
};
use epicenter_common::{
    DialogueSpeaker, Gemini4Model, WarRoomTurn, EPICENTER_NEON_AGENT_NAME, GEMINI_TTS_MODEL,
    MANAGED_AGENT_BASE,
};
use futures::stream::BoxStream;
use futures::StreamExt;
use reqwest::{Method, Response, StatusCode};
use serde_json::json;
use std::time::{Duration, Instant};
use tracing::{debug, warn};

pub const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com";

/// Configuration for `GeminiClient` retry and timeout behavior.
#[derive(Debug, Clone)]
pub struct GeminiClientConfig {
    pub base_url: String,
    pub max_retries: u32,
    pub initial_backoff: Duration,
    pub request_timeout: Duration,
}

impl Default for GeminiClientConfig {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_string(),
            max_retries: 4,
            initial_backoff: Duration::from_millis(500),
            request_timeout: Duration::from_secs(90),
        }
    }
}

/// Strongly-typed async client for Gemini `v1beta` Interactions, Managed Agents, Files, and TTS APIs.
#[derive(Debug, Clone)]
pub struct GeminiClient {
    http: reqwest::Client,
    api_key: String,
    config: GeminiClientConfig,
}

impl GeminiClient {
    pub fn new(api_key: impl Into<String>) -> Result<Self, GeminiError> {
        Self::with_config(api_key, GeminiClientConfig::default())
    }

    pub fn from_env() -> Result<Self, GeminiError> {
        let key = std::env::var("GEMINI_API_KEY")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .ok_or(GeminiError::MissingApiKey)?;
        Self::new(key)
    }

    pub fn with_config(
        api_key: impl Into<String>,
        config: GeminiClientConfig,
    ) -> Result<Self, GeminiError> {
        let key = api_key.into();
        if key.trim().is_empty() {
            return Err(GeminiError::MissingApiKey);
        }
        let http = reqwest::Client::builder()
            .timeout(config.request_timeout)
            .pool_idle_timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self {
            http,
            api_key: key,
            config,
        })
    }

    pub fn base_url(&self) -> &str {
        &self.config.base_url
    }

    /// `POST /v1beta/interactions`
    pub async fn create_interaction(
        &self,
        request: &CreateInteractionRequest,
    ) -> Result<InteractionResponse, GeminiError> {
        let url = format!("{}/v1beta/interactions", self.config.base_url);
        let resp = self
            .send_json_with_retry(Method::POST, &url, request)
            .await?;
        let parsed: InteractionResponse = resp.json().await?;
        Ok(parsed)
    }

    /// `GET /v1beta/interactions/{id}`
    pub async fn get_interaction(
        &self,
        interaction_id: &str,
    ) -> Result<InteractionResponse, GeminiError> {
        let url = format!(
            "{}/v1beta/interactions/{}",
            self.config.base_url, interaction_id
        );
        let resp = self.send_empty_with_retry(Method::GET, &url).await?;
        let parsed: InteractionResponse = resp.json().await?;
        Ok(parsed)
    }

    /// Polls `GET /v1beta/interactions/{id}` until `completed`, `failed`, or `cancelled`.
    pub async fn poll_interaction(
        &self,
        interaction_id: &str,
        poll_interval: Duration,
        timeout: Duration,
    ) -> Result<InteractionResponse, GeminiError> {
        let started = Instant::now();
        loop {
            let state = self.get_interaction(interaction_id).await?;
            match state.status {
                InteractionStatus::Completed => return Ok(state),
                InteractionStatus::Failed => {
                    let msg = state
                        .error
                        .as_ref()
                        .and_then(|e| e.message.clone())
                        .unwrap_or_else(|| "interaction reported failed status".to_string());
                    return Err(GeminiError::InteractionFailed {
                        id: state.id,
                        message: msg,
                    });
                }
                InteractionStatus::Cancelled => {
                    return Err(GeminiError::InteractionFailed {
                        id: state.id,
                        message: "interaction was cancelled".to_string(),
                    });
                }
                _ => {
                    if started.elapsed() >= timeout {
                        return Err(GeminiError::Timeout {
                            id: interaction_id.to_string(),
                            elapsed_secs: started.elapsed().as_secs(),
                        });
                    }
                    tokio::time::sleep(poll_interval).await;
                }
            }
        }
    }

    /// Launches a background interaction (`background: true`) and polls until completion.
    pub async fn run_background_interaction(
        &self,
        mut request: CreateInteractionRequest,
        poll_interval: Duration,
        timeout: Duration,
    ) -> Result<InteractionResponse, GeminiError> {
        request.background = Some(true);
        let initial = self.create_interaction(&request).await?;
        if initial.status.is_terminal() {
            if initial.status == InteractionStatus::Failed {
                let msg = initial
                    .error
                    .as_ref()
                    .and_then(|e| e.message.clone())
                    .unwrap_or_else(|| "interaction failed immediately".to_string());
                return Err(GeminiError::InteractionFailed {
                    id: initial.id,
                    message: msg,
                });
            }
            return Ok(initial);
        }
        self.poll_interaction(&initial.id, poll_interval, timeout)
            .await
    }

    /// Streams `POST /v1beta/interactions?alt=sse` with `"stream": true`, yielding typed SSE events.
    pub async fn stream_interaction(
        &self,
        mut request: CreateInteractionRequest,
    ) -> Result<BoxStream<'static, Result<InteractionStreamEvent, GeminiError>>, GeminiError> {
        request.stream = Some(true);
        let url = format!("{}/v1beta/interactions?alt=sse", self.config.base_url);
        let resp = self
            .http
            .post(&url)
            .header("x-goog-api-key", &self.api_key)
            .header("Accept", "text/event-stream")
            .json(&request)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(GeminiError::Api {
                status: status.as_u16(),
                body,
            });
        }

        let byte_stream = resp.bytes_stream();
        let stream = futures::stream::unfold(
            (byte_stream, SseParser::new(), Vec::<InteractionStreamEvent>::new(), false),
            |(mut bytes, mut parser, mut pending, mut finished)| async move {
                loop {
                    if !pending.is_empty() {
                        let next = pending.remove(0);
                        return Some((Ok(next), (bytes, parser, pending, finished)));
                    }
                    if finished {
                        return None;
                    }
                    match bytes.next().await {
                        Some(Ok(chunk)) => {
                            let text = String::from_utf8_lossy(&chunk);
                            pending.extend(parser.push_chunk(&text));
                        }
                        Some(Err(err)) => {
                            return Some((
                                Err(GeminiError::Http(err)),
                                (bytes, parser, pending, true),
                            ));
                        }
                        None => {
                            finished = true;
                            pending.extend(parser.finish());
                        }
                    }
                }
            },
        );

        Ok(Box::pin(stream))
    }

    /// `POST /v1beta/agents` to register a custom Managed Agent.
    pub async fn create_agent(
        &self,
        request: &CreateAgentRequest,
    ) -> Result<AgentResource, GeminiError> {
        let url = format!("{}/v1beta/agents", self.config.base_url);
        let resp = self
            .send_json_with_retry(Method::POST, &url, request)
            .await?;
        let resource: AgentResource = resp.json().await?;
        Ok(resource)
    }

    /// Registers/ensures a custom Managed Agent (e.g. `epicenter-neon-worker` running `gemini-4-neon`),
    /// falling back gracefully to `antigravity-preview-09-2026` if custom agent creation is unavailable.
    pub async fn ensure_managed_agent(
        &self,
        agent_name: &str,
        model: Gemini4Model,
        system_instruction: Option<String>,
    ) -> String {
        let req = CreateAgentRequest::antigravity(agent_name, model, system_instruction);
        match self.create_agent(&req).await {
            Ok(res) => res
                .resolved_agent_id()
                .map(String::from)
                .unwrap_or_else(|| agent_name.to_string()),
            Err(GeminiError::Api { status: 409, .. }) => {
                debug!(
                    agent = agent_name,
                    "custom managed agent already exists; reusing"
                );
                agent_name.to_string()
            }
            Err(err) => {
                warn!(
                    agent = agent_name,
                    error = %err,
                    fallback = MANAGED_AGENT_BASE,
                    "failed to register custom managed agent; falling back to base agent"
                );
                MANAGED_AGENT_BASE.to_string()
            }
        }
    }

    /// Convenience helper ensuring Epicenter's default Neon Managed Agent worker.
    pub async fn ensure_neon_worker_agent(&self) -> String {
        self.ensure_managed_agent(EPICENTER_NEON_AGENT_NAME, Gemini4Model::Neon, None)
            .await
    }

    /// Resumable media upload via `POST /upload/v1beta/files` (`X-Goog-Upload-Protocol: resumable`),
    /// returning a `FileRef` usable across multimodal Gemini Interactions.
    pub async fn upload_file_resumable(
        &self,
        display_name: &str,
        mime_type: &str,
        bytes: Vec<u8>,
    ) -> Result<FileRef, GeminiError> {
        let start_url = format!("{}/upload/v1beta/files", self.config.base_url);
        let content_len = bytes.len().to_string();

        let start_resp = self
            .http
            .post(&start_url)
            .header("x-goog-api-key", &self.api_key)
            .header("X-Goog-Upload-Protocol", "resumable")
            .header("X-Goog-Upload-Command", "start")
            .header("X-Goog-Upload-Header-Content-Length", &content_len)
            .header("X-Goog-Upload-Header-Content-Type", mime_type)
            .json(&json!({
                "file": {
                    "display_name": display_name
                }
            }))
            .send()
            .await?;

        let start_status = start_resp.status();
        if !start_status.is_success() {
            let body = start_resp.text().await.unwrap_or_default();
            return Err(GeminiError::Api {
                status: start_status.as_u16(),
                body,
            });
        }

        let upload_url = start_resp
            .headers()
            .get("x-goog-upload-url")
            .and_then(|v| v.to_str().ok())
            .map(String::from)
            .ok_or(GeminiError::MissingHeader("x-goog-upload-url"))?;

        let upload_resp = self
            .http
            .post(&upload_url)
            .header("Content-Length", &content_len)
            .header("X-Goog-Upload-Offset", "0")
            .header("X-Goog-Upload-Command", "upload, finalize")
            .header("Content-Type", mime_type)
            .body(bytes)
            .send()
            .await?;

        let upload_status = upload_resp.status();
        if !upload_status.is_success() {
            let body = upload_resp.text().await.unwrap_or_default();
            return Err(GeminiError::Api {
                status: upload_status.as_u16(),
                body,
            });
        }

        let parsed: UploadFileResponse = upload_resp.json().await?;
        Ok(FileRef {
            uri: parsed.file.uri,
            mime_type: parsed.file.mime_type,
            name: parsed.file.name,
        })
    }

    /// Synthesizes an executive audio briefing WAV via `gemini-3.8-flash-tts` on `/v1beta/interactions`
    /// with `response_format: { "type": "audio" }`, decoding base64 WAV bytes from `steps[].content[].data`.
    pub async fn synthesize_briefing_wav(
        &self,
        script_text: &str,
        system_instruction: Option<String>,
    ) -> Result<Vec<u8>, GeminiError> {
        let mut req = CreateInteractionRequest::for_model_id(GEMINI_TTS_MODEL, script_text);
        req.response_format = Some(ResponseFormat::audio());
        req.system_instruction = system_instruction;

        let resp = self.create_interaction(&req).await?;
        resp.extract_audio_bytes()
    }

    /// Synthesizes a 2-speaker war-room dialogue WAV (`Architect` vs. `Auditor`) via `gemini-3.8-flash-tts`,
    /// automatically falling back to single-prompt narration if multi-speaker annotations are rejected.
    pub async fn synthesize_war_room_wav(
        &self,
        turns: &[WarRoomTurn],
    ) -> Result<Vec<u8>, GeminiError> {
        let content_parts: Vec<serde_json::Value> = turns
            .iter()
            .map(|t| {
                json!({
                    "type": "text",
                    "text": t.text,
                    "annotations": [{
                        "type": "speech_metadata",
                        "speaker": t.speaker.label(),
                        "style": t.speaker.delivery_style()
                    }]
                })
            })
            .collect();

        let payload = json!({
            "model": GEMINI_TTS_MODEL,
            "input": [{
                "type": "user_input",
                "content": content_parts
            }],
            "response_format": { "type": "audio" },
            "generation_config": {
                "speech_config": {
                    "speakers": [
                        {
                            "speaker": DialogueSpeaker::Architect.label(),
                            "voice": DialogueSpeaker::Architect.prebuilt_voice()
                        },
                        {
                            "speaker": DialogueSpeaker::Auditor.label(),
                            "voice": DialogueSpeaker::Auditor.prebuilt_voice()
                        }
                    ]
                }
            }
        });

        let url = format!("{}/v1beta/interactions", self.config.base_url);
        match self.send_json_with_retry(Method::POST, &url, &payload).await {
            Ok(resp) => {
                let parsed: InteractionResponse = resp.json().await?;
                parsed.extract_audio_bytes()
            }
            Err(err) => {
                warn!(error = %err, "multi-speaker TTS failed; falling back to combined script TTS");
                let fallback_script = turns
                    .iter()
                    .map(|t| format!("{}: {}", t.speaker.label(), t.text))
                    .collect::<Vec<_>>()
                    .join("\n");
                self.synthesize_briefing_wav(&fallback_script, None).await
            }
        }
    }

    async fn send_json_with_retry<T: serde::Serialize>(
        &self,
        method: Method,
        url: &str,
        payload: &T,
    ) -> Result<Response, GeminiError> {
        let mut attempt = 0u32;
        loop {
            let result = self
                .http
                .request(method.clone(), url)
                .header("x-goog-api-key", &self.api_key)
                .json(payload)
                .send()
                .await;

            match self.evaluate_retry(result, attempt).await? {
                RetryOutcome::Ok(resp) => return Ok(resp),
                RetryOutcome::Retry => {
                    attempt += 1;
                }
            }
        }
    }

    async fn send_empty_with_retry(
        &self,
        method: Method,
        url: &str,
    ) -> Result<Response, GeminiError> {
        let mut attempt = 0u32;
        loop {
            let result = self
                .http
                .request(method.clone(), url)
                .header("x-goog-api-key", &self.api_key)
                .send()
                .await;

            match self.evaluate_retry(result, attempt).await? {
                RetryOutcome::Ok(resp) => return Ok(resp),
                RetryOutcome::Retry => {
                    attempt += 1;
                }
            }
        }
    }

    async fn evaluate_retry(
        &self,
        result: Result<Response, reqwest::Error>,
        attempt: u32,
    ) -> Result<RetryOutcome, GeminiError> {
        match result {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    return Ok(RetryOutcome::Ok(resp));
                }
                if is_retryable_status(status) && attempt < self.config.max_retries {
                    let backoff = self.backoff_duration(attempt);
                    warn!(
                        status = status.as_u16(),
                        attempt,
                        backoff_ms = backoff.as_millis() as u64,
                        "transient Gemini API status; retrying"
                    );
                    tokio::time::sleep(backoff).await;
                    return Ok(RetryOutcome::Retry);
                }
                let body = resp.text().await.unwrap_or_default();
                Err(GeminiError::Api {
                    status: status.as_u16(),
                    body,
                })
            }
            Err(err) => {
                // Retry fast connect/transport errors up to `max_retries`, while capping full
                // request timeouts to a single retry so slow calls never stall a stage.
                let retry_allowed = if err.is_timeout() {
                    attempt == 0 && self.config.max_retries > 0
                } else {
                    (err.is_connect() || err.is_request()) && attempt < self.config.max_retries
                };
                if retry_allowed {
                    let backoff = self.backoff_duration(attempt);
                    warn!(
                        error = %err,
                        attempt,
                        backoff_ms = backoff.as_millis() as u64,
                        "transient Gemini transport error; retrying"
                    );
                    tokio::time::sleep(backoff).await;
                    return Ok(RetryOutcome::Retry);
                }
                Err(GeminiError::Http(err))
            }
        }
    }

    fn backoff_duration(&self, attempt: u32) -> Duration {
        let mult = 1u32.checked_shl(attempt.min(6)).unwrap_or(64);
        self.config.initial_backoff * mult
    }
}

enum RetryOutcome {
    Ok(Response),
    Retry,
}

fn is_retryable_status(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::TOO_MANY_REQUESTS
            | StatusCode::INTERNAL_SERVER_ERROR
            | StatusCode::BAD_GATEWAY
            | StatusCode::SERVICE_UNAVAILABLE
            | StatusCode::GATEWAY_TIMEOUT
    )
}
