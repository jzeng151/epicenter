use thiserror::Error;

#[derive(Debug, Error)]
pub enum GeminiError {
    #[error("missing GEMINI_API_KEY configuration")]
    MissingApiKey,

    #[error("HTTP transport error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("Gemini API error ({status}): {body}")]
    Api { status: u16, body: String },

    #[error("JSON serialization/deserialization error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("base64 decode error: {0}")]
    Base64(#[from] base64::DecodeError),

    #[error("missing expected header '{0}' in response")]
    MissingHeader(&'static str),

    #[error("interaction '{id}' failed: {message}")]
    InteractionFailed { id: String, message: String },

    #[error("interaction '{id}' timed out after {elapsed_secs}s")]
    Timeout { id: String, elapsed_secs: u64 },

    #[error("no audio bytes returned in TTS interaction response")]
    MissingAudioPayload,
}
