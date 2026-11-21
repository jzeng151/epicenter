use base64::Engine;
use epicenter_common::Gemini4Model;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::GeminiError;

/// Reference to a file uploaded via Gemini's resumable Files API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRef {
    pub uri: String,
    pub mime_type: String,
    pub name: String,
}

/// Content part sent inside an interaction `input` array.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text {
        text: String,
    },
    Image {
        #[serde(skip_serializing_if = "Option::is_none")]
        uri: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        data: Option<String>,
        mime_type: String,
    },
    Video {
        #[serde(skip_serializing_if = "Option::is_none")]
        uri: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        data: Option<String>,
        mime_type: String,
    },
    Audio {
        #[serde(skip_serializing_if = "Option::is_none")]
        uri: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        data: Option<String>,
        mime_type: String,
    },
    Document {
        #[serde(skip_serializing_if = "Option::is_none")]
        uri: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        data: Option<String>,
        mime_type: String,
    },
}

impl ContentPart {
    pub fn text(s: impl Into<String>) -> Self {
        Self::Text { text: s.into() }
    }

    pub fn from_file_ref(file: &FileRef) -> Self {
        let mime = file.mime_type.to_ascii_lowercase();
        if mime.starts_with("image/") {
            Self::Image {
                uri: Some(file.uri.clone()),
                data: None,
                mime_type: file.mime_type.clone(),
            }
        } else if mime.starts_with("video/") {
            Self::Video {
                uri: Some(file.uri.clone()),
                data: None,
                mime_type: file.mime_type.clone(),
            }
        } else if mime.starts_with("audio/") {
            Self::Audio {
                uri: Some(file.uri.clone()),
                data: None,
                mime_type: file.mime_type.clone(),
            }
        } else {
            Self::Document {
                uri: Some(file.uri.clone()),
                data: None,
                mime_type: file.mime_type.clone(),
            }
        }
    }
}

/// Interaction input supporting either plain text or multimodal content parts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum InteractionInput {
    Text(String),
    Parts(Vec<ContentPart>),
}

impl From<String> for InteractionInput {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

impl From<&str> for InteractionInput {
    fn from(value: &str) -> Self {
        Self::Text(value.to_string())
    }
}

impl From<Vec<ContentPart>> for InteractionInput {
    fn from(parts: Vec<ContentPart>) -> Self {
        Self::Parts(parts)
    }
}

/// Structured output or audio modality format for `/v1beta/interactions`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ResponseFormat {
    /// Audio modality format (`{ "type": "audio" }`), used with `gemini-3.8-flash-tts`.
    Audio(AudioResponseFormat),
    /// Structured JSON Schema format (`{ "type": "text", "mime_type": "application/json", "schema": ... }`).
    JsonSchema(JsonSchemaResponseFormat),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioResponseFormat {
    #[serde(rename = "type")]
    pub format_type: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonSchemaResponseFormat {
    #[serde(rename = "type")]
    pub format_type: String,
    pub mime_type: String,
    pub schema: Value,
}

impl ResponseFormat {
    pub fn audio() -> Self {
        Self::Audio(AudioResponseFormat {
            format_type: "audio".to_string(),
        })
    }

    pub fn json_schema(schema: Value) -> Self {
        Self::JsonSchema(JsonSchemaResponseFormat {
            format_type: "text".to_string(),
            mime_type: "application/json".to_string(),
            schema,
        })
    }
}

/// Source repository mounted into a remote Managed Agent sandbox environment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentSource {
    #[serde(rename = "type")]
    pub source_type: String,
    pub source: String,
    pub target: String,
}

impl EnvironmentSource {
    pub fn repository(repo_url: impl Into<String>, target: impl Into<String>) -> Self {
        Self {
            source_type: "repository".to_string(),
            source: repo_url.into(),
            target: target.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkAllowlistDomain {
    pub domain: String,
}

/// Network egress policy for a Managed Agent remote sandbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentNetwork {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowlist: Vec<NetworkAllowlistDomain>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteEnvironmentConfig {
    #[serde(rename = "type")]
    pub env_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<EnvironmentSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network: Option<EnvironmentNetwork>,
}

/// Remote sandbox environment specification (`"remote"` shorthand or `RemoteEnvironmentConfig` object).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum InteractionEnvironment {
    Shorthand(String),
    Config(RemoteEnvironmentConfig),
}

impl InteractionEnvironment {
    /// Clean `"remote"` shorthand that provisions a fresh Ubuntu sandbox with git/python/node.
    pub fn remote() -> Self {
        Self::Shorthand("remote".to_string())
    }

    pub fn remote_repo(
        repo_url: impl Into<String>,
        _branch: Option<String>,
        _commit: Option<String>,
        allowlist: Vec<String>,
    ) -> Self {
        Self::Config(RemoteEnvironmentConfig {
            env_type: "remote".to_string(),
            environment_id: None,
            sources: vec![EnvironmentSource::repository(repo_url, "/workspace/repo")],
            network: if allowlist.is_empty() {
                None
            } else {
                Some(EnvironmentNetwork {
                    allowlist: allowlist
                        .into_iter()
                        .map(|domain| NetworkAllowlistDomain { domain })
                        .collect(),
                })
            },
        })
    }

    pub fn existing_remote(environment_id: impl Into<String>) -> Self {
        Self::Config(RemoteEnvironmentConfig {
            env_type: "remote".to_string(),
            environment_id: Some(environment_id.into()),
            sources: Vec::new(),
            network: None,
        })
    }
}

/// Optional generation tuning parameters (including multi-speaker TTS `speech_config`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct GenerationConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speech_config: Option<Value>,
}

/// Request body for `POST /v1beta/interactions`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct CreateInteractionRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<InteractionInput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_instruction: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<ResponseFormat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_mime_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<InteractionEnvironment>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_interaction_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation_config: Option<GenerationConfig>,
}

impl CreateInteractionRequest {
    pub fn for_model(model: Gemini4Model, input: impl Into<InteractionInput>) -> Self {
        Self {
            model: Some(model.as_str().to_string()),
            input: Some(input.into()),
            ..Default::default()
        }
    }

    pub fn for_model_id(model: impl Into<String>, input: impl Into<InteractionInput>) -> Self {
        Self {
            model: Some(model.into()),
            input: Some(input.into()),
            ..Default::default()
        }
    }

    pub fn for_agent(agent_id: impl Into<String>, input: impl Into<InteractionInput>) -> Self {
        Self {
            agent: Some(agent_id.into()),
            input: Some(input.into()),
            ..Default::default()
        }
    }
}

/// Lifecycle status of an interaction returned by `POST/GET /v1beta/interactions`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InteractionStatus {
    InProgress,
    Completed,
    Failed,
    Cancelled,
    RequiresAction,
    #[serde(other)]
    Unknown,
}

impl InteractionStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

/// Output content element inside `outputs[]` or `steps[].content[]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct OutputPart {
    #[serde(rename = "type", default)]
    pub part_type: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub data: Option<String>,
    #[serde(default)]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub uri: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub arguments: Option<Value>,
    #[serde(default)]
    pub result: Option<Value>,
}

/// Execution step recorded in an interaction response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct InteractionStep {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(rename = "type", default)]
    pub step_type: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub content: Vec<OutputPart>,
}

/// Structured error returned inside a failed interaction response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InteractionError {
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
}

/// Environment metadata echoed back on an interaction response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct InteractionEnvironmentInfo {
    #[serde(default)]
    pub environment_id: Option<String>,
    #[serde(default)]
    pub id: Option<String>,
}

/// Response payload from `POST /v1beta/interactions` and `GET /v1beta/interactions/{id}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InteractionResponse {
    pub id: String,
    #[serde(default = "default_status")]
    pub status: InteractionStatus,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub environment_id: Option<String>,
    #[serde(default)]
    pub environment: Option<InteractionEnvironmentInfo>,
    #[serde(default)]
    pub outputs: Vec<OutputPart>,
    #[serde(default)]
    pub steps: Vec<InteractionStep>,
    #[serde(default)]
    pub error: Option<InteractionError>,
}

fn default_status() -> InteractionStatus {
    InteractionStatus::Completed
}

impl InteractionResponse {
    /// Returns the resolved remote environment ID if present at top-level or nested under `environment`.
    pub fn resolved_environment_id(&self) -> Option<&str> {
        self.environment_id
            .as_deref()
            .or_else(|| {
                self.environment
                    .as_ref()
                    .and_then(|e| e.environment_id.as_deref().or(e.id.as_deref()))
            })
    }

    /// Concatenates all text output parts from `outputs` (preferred) or `steps[].content[]`.
    pub fn extract_text(&self) -> String {
        let mut chunks: Vec<&str> = self
            .outputs
            .iter()
            .filter_map(|p| p.text.as_deref())
            .collect();

        if chunks.is_empty() {
            for step in &self.steps {
                for part in &step.content {
                    if let Some(ref t) = part.text {
                        chunks.push(t.as_str());
                    }
                }
            }
        }

        chunks.join("\n").trim().to_string()
    }

    /// Extracts and deserializes structured JSON output (stripping optional ```json fences).
    pub fn extract_json<T: serde::de::DeserializeOwned>(&self) -> Result<T, GeminiError> {
        let raw = self.extract_text();
        let cleaned = strip_markdown_fences(&raw);
        Ok(serde_json::from_str(cleaned)?)
    }

    /// Extracts and base64-decodes WAV/audio bytes from `steps[].content[].data` or `outputs[].data`.
    pub fn extract_audio_bytes(&self) -> Result<Vec<u8>, GeminiError> {
        let candidate_b64 = self
            .steps
            .iter()
            .flat_map(|s| s.content.iter())
            .chain(self.outputs.iter())
            .find(|p| {
                p.data.is_some()
                    && (p.part_type.as_deref() == Some("audio")
                        || p.mime_type
                            .as_deref()
                            .is_some_and(|m| m.starts_with("audio/"))
                        || p.text.is_none())
            })
            .and_then(|p| p.data.as_deref());

        let Some(b64) = candidate_b64 else {
            return Err(GeminiError::MissingAudioPayload);
        };

        let compact: String = b64.chars().filter(|c| !c.is_whitespace()).collect();
        let bytes = base64::engine::general_purpose::STANDARD.decode(compact)?;
        Ok(bytes)
    }
}

fn strip_markdown_fences(s: &str) -> &str {
    let trimmed = s.trim();
    if let Some(rest) = trimmed.strip_prefix("```json") {
        if let Some(inner) = rest.strip_suffix("```") {
            return inner.trim();
        }
    }
    if let Some(rest) = trimmed.strip_prefix("```") {
        if let Some(inner) = rest.strip_suffix("```") {
            return inner.trim();
        }
    }
    trimmed
}

/// Configuration for registering a custom Managed Agent via `POST /v1beta/agents`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentConfig {
    #[serde(rename = "type")]
    pub agent_type: String,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_instruction: Option<String>,
}

/// Request body for `POST /v1beta/agents`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateAgentRequest {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub agent_config: AgentConfig,
}

impl CreateAgentRequest {
    pub fn antigravity(
        name: impl Into<String>,
        model: Gemini4Model,
        system_instruction: Option<String>,
    ) -> Self {
        let name_str = name.into();
        Self {
            display_name: Some(name_str.clone()),
            name: name_str,
            agent_config: AgentConfig {
                agent_type: "antigravity".to_string(),
                model: model.as_str().to_string(),
                system_instruction,
            },
        }
    }
}

/// Response payload from `POST /v1beta/agents`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentResource {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub agent_config: Option<AgentConfig>,
}

impl AgentResource {
    pub fn resolved_agent_id(&self) -> Option<&str> {
        self.id.as_deref().or(self.name.as_deref())
    }
}

/// Internal deserialization wrapper for Gemini Files API finalize response.
#[derive(Debug, Deserialize)]
pub(crate) struct UploadFileResponse {
    pub file: UploadedFileMetadata,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UploadedFileMetadata {
    pub name: String,
    pub uri: String,
    #[serde(rename = "mimeType", alias = "mime_type")]
    pub mime_type: String,
}
