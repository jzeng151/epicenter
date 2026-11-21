use crate::types::{InteractionResponse, InteractionStatus, InteractionStep};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Typed streaming events yielded by `/v1beta/interactions?alt=sse`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum InteractionStreamEvent {
    InteractionStart {
        interaction_id: String,
    },
    ContentDelta {
        delta_type: String,
        text: Option<String>,
        data: Option<String>,
        mime_type: Option<String>,
    },
    StepUpdate {
        step: InteractionStep,
    },
    StatusChange {
        status: InteractionStatus,
    },
    InteractionComplete {
        interaction: Box<InteractionResponse>,
    },
    Error {
        message: String,
    },
    Raw {
        event: Option<String>,
        data: String,
    },
}

/// Incremental Server-Sent Events (SSE) parser for chunked HTTP streams.
#[derive(Debug, Default)]
pub struct SseParser {
    buffer: String,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds a chunk of UTF-8 text into the parser and drains any complete SSE events.
    pub fn push_chunk(&mut self, chunk: &str) -> Vec<InteractionStreamEvent> {
        self.buffer.push_str(&chunk.replace("\r\n", "\n"));
        let mut events = Vec::new();

        while let Some(idx) = self.buffer.find("\n\n") {
            let block = self.buffer[..idx].to_string();
            self.buffer.drain(..idx + 2);
            if let Some(evt) = parse_sse_block(&block) {
                events.push(evt);
            }
        }
        events
    }

    /// Flushes any trailing buffered frame when the stream ends.
    pub fn finish(&mut self) -> Vec<InteractionStreamEvent> {
        let remaining = self.buffer.trim().to_string();
        self.buffer.clear();
        if remaining.is_empty() {
            Vec::new()
        } else {
            parse_sse_block(&remaining).into_iter().collect()
        }
    }
}

fn parse_sse_block(block: &str) -> Option<InteractionStreamEvent> {
    let mut event_name: Option<String> = None;
    let mut data_lines: Vec<&str> = Vec::new();

    for line in block.lines() {
        let trimmed = line.trim_end();
        if trimmed.is_empty() || trimmed.starts_with(':') {
            continue;
        }
        if let Some(ev) = trimmed.strip_prefix("event:") {
            event_name = Some(ev.trim().to_string());
        } else if let Some(d) = trimmed.strip_prefix("data:") {
            data_lines.push(d.trim_start());
        }
    }

    if data_lines.is_empty() {
        return None;
    }

    let data = data_lines.join("\n");
    if data.trim() == "[DONE]" {
        return Some(InteractionStreamEvent::StatusChange {
            status: InteractionStatus::Completed,
        });
    }

    let Ok(json) = serde_json::from_str::<Value>(&data) else {
        return Some(InteractionStreamEvent::Raw {
            event: event_name,
            data,
        });
    };

    let inferred_type = event_name
        .as_deref()
        .or_else(|| json.get("event_type").and_then(Value::as_str))
        .or_else(|| json.get("type").and_then(Value::as_str))
        .unwrap_or("");

    match inferred_type {
        "interaction.start" => {
            let id = json
                .get("interaction")
                .and_then(|v| v.get("id"))
                .or_else(|| json.get("id"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            Some(InteractionStreamEvent::InteractionStart { interaction_id: id })
        }
        "content.delta" => {
            let delta = json.get("delta").unwrap_or(&json);
            let delta_type = delta
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("text")
                .to_string();
            Some(InteractionStreamEvent::ContentDelta {
                delta_type,
                text: delta.get("text").and_then(Value::as_str).map(String::from),
                data: delta.get("data").and_then(Value::as_str).map(String::from),
                mime_type: delta
                    .get("mime_type")
                    .and_then(Value::as_str)
                    .map(String::from),
            })
        }
        "step.start" | "step.delta" | "step.complete" | "step.update" => {
            let step_val = json.get("step").unwrap_or(&json);
            if let Ok(step) = serde_json::from_value::<InteractionStep>(step_val.clone()) {
                Some(InteractionStreamEvent::StepUpdate { step })
            } else {
                Some(InteractionStreamEvent::Raw {
                    event: event_name,
                    data,
                })
            }
        }
        "interaction.complete" => {
            let target = json.get("interaction").unwrap_or(&json);
            if let Ok(interaction) = serde_json::from_value::<InteractionResponse>(target.clone()) {
                Some(InteractionStreamEvent::InteractionComplete {
                    interaction: Box::new(interaction),
                })
            } else {
                Some(InteractionStreamEvent::StatusChange {
                    status: InteractionStatus::Completed,
                })
            }
        }
        "error" => {
            let message = json
                .get("error")
                .and_then(|e| e.get("message"))
                .or_else(|| json.get("message"))
                .and_then(Value::as_str)
                .unwrap_or(&data)
                .to_string();
            Some(InteractionStreamEvent::Error { message })
        }
        _ => {
            if let Some(delta) = json.get("delta") {
                return Some(InteractionStreamEvent::ContentDelta {
                    delta_type: delta
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or("text")
                        .to_string(),
                    text: delta.get("text").and_then(Value::as_str).map(String::from),
                    data: delta.get("data").and_then(Value::as_str).map(String::from),
                    mime_type: delta
                        .get("mime_type")
                        .and_then(Value::as_str)
                        .map(String::from),
                });
            }
            if let Some(status_val) = json.get("status") {
                if let Ok(status) = serde_json::from_value::<InteractionStatus>(status_val.clone())
                {
                    return Some(InteractionStreamEvent::StatusChange { status });
                }
            }
            Some(InteractionStreamEvent::Raw {
                event: event_name,
                data,
            })
        }
    }
}
