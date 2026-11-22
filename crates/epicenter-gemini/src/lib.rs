pub mod client;
pub mod decisions;
pub mod error;
pub mod sse;
pub mod types;

pub use client::{GeminiClient, GeminiClientConfig, DEFAULT_BASE_URL};
pub use decisions::*;
pub use error::GeminiError;
pub use sse::{InteractionStreamEvent, SseParser};
pub use types::*;

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use epicenter_common::{Gemini4Model, GEMINI_TTS_MODEL, MANAGED_AGENT_BASE};
    use serde_json::{json, Value};

    #[test]
    fn serializes_model_interaction_with_json_schema_and_multimodal_parts() {
        let file = FileRef {
            uri: "https://generativelanguage.googleapis.com/v1beta/files/abc123".into(),
            mime_type: "video/mp4".into(),
            name: "files/abc123".into(),
        };
        let mut req = CreateInteractionRequest::for_model(
            Gemini4Model::Hydrogen,
            vec![
                ContentPart::text("Extract repro steps from this screen recording."),
                ContentPart::from_file_ref(&file),
            ],
        );
        req.response_format = Some(ResponseFormat::json_schema(json!({
            "type": "object",
            "properties": {
                "title": { "type": "string" }
            },
            "required": ["title"]
        })));
        req.previous_interaction_id = Some("int_prev_001".into());

        let v: Value = serde_json::to_value(&req).unwrap();
        assert_eq!(v["model"], "gemini-4-hydrogen");
        assert!(v.get("agent").is_none());
        assert_eq!(v["previous_interaction_id"], "int_prev_001");
        assert_eq!(v["input"][0]["type"], "text");
        assert_eq!(v["input"][1]["type"], "video");
        assert_eq!(v["input"][1]["mime_type"], "video/mp4");
        assert_eq!(v["response_format"]["type"], "text");
        assert_eq!(v["response_format"]["mime_type"], "application/json");
        assert_eq!(v["response_format"]["schema"]["type"], "object");
    }

    #[test]
    fn serializes_managed_agent_interaction_with_remote_environment() {
        let mut req = CreateInteractionRequest::for_agent(
            MANAGED_AGENT_BASE,
            "Implement strategy S1 and verify pytest passes.",
        );
        req.environment = Some(InteractionEnvironment::remote_repo(
            "https://github.com/jzeng151/epicenter.git",
            Some("main".into()),
            Some("deadbeef".into()),
            vec!["pypi.org".into(), "files.pythonhosted.org".into()],
        ));
        req.background = Some(true);

        let v: Value = serde_json::to_value(&req).unwrap();
        assert_eq!(v["agent"], "antigravity-preview-09-2026");
        assert!(v.get("model").is_none());
        assert_eq!(v["background"], true);
        assert_eq!(v["environment"]["type"], "remote");
        assert_eq!(v["environment"]["sources"][0]["type"], "repository");
        assert_eq!(
            v["environment"]["sources"][0]["source"],
            "https://github.com/jzeng151/epicenter.git"
        );
        assert_eq!(v["environment"]["sources"][0]["target"], "/workspace/repo");
        assert_eq!(
            v["environment"]["network"]["allowlist"][0]["domain"],
            "pypi.org"
        );
    }

    #[test]
    fn serializes_custom_agent_registration_payload() {
        let req = CreateAgentRequest::antigravity(
            "epicenter-neon-worker",
            Gemini4Model::Neon,
            Some("You are a disciplined software engineer.".into()),
        );
        let v: Value = serde_json::to_value(&req).unwrap();
        assert_eq!(v["name"], "epicenter-neon-worker");
        assert_eq!(v["agent_config"]["type"], "antigravity");
        assert_eq!(v["agent_config"]["model"], "gemini-4-neon");
    }

    #[test]
    fn serializes_tts_request_and_decodes_base64_wav_from_steps() {
        let mut req = CreateInteractionRequest::for_model_id(
            GEMINI_TTS_MODEL,
            "Strategy B cuts p95 latency by 48 percent with zero dark-zone callers.",
        );
        req.response_format = Some(ResponseFormat::audio());
        let v: Value = serde_json::to_value(&req).unwrap();
        assert_eq!(v["model"], "gemini-3.8-flash-tts");
        assert_eq!(v["response_format"], json!({ "type": "audio" }));

        let fake_wav = b"RIFF\x24\x00\x00\x00WAVEfmt ";
        let encoded = base64::engine::general_purpose::STANDARD.encode(fake_wav);
        let response_json = json!({
            "id": "int_tts_42",
            "status": "completed",
            "model": "gemini-3.8-flash-tts",
            "steps": [
                {
                    "id": "step_1",
                    "type": "model_turn",
                    "content": [
                        {
                            "type": "audio",
                            "mime_type": "audio/wav",
                            "data": encoded
                        }
                    ]
                }
            ]
        });
        let resp: InteractionResponse = serde_json::from_value(response_json).unwrap();
        let decoded = resp.extract_audio_bytes().unwrap();
        assert_eq!(decoded, fake_wav);
    }

    #[test]
    fn parses_structured_json_response_and_environment_id() {
        let resp_json = json!({
            "id": "int_plan_99",
            "status": "completed",
            "environment": { "environment_id": "env_xyz_1" },
            "outputs": [
                {
                    "type": "text",
                    "text": "```json\n{\"k\": 3, \"winner\": \"s2\"}\n```"
                }
            ]
        });
        let resp: InteractionResponse = serde_json::from_value(resp_json).unwrap();
        assert_eq!(resp.resolved_environment_id(), Some("env_xyz_1"));
        let val: Value = resp.extract_json().unwrap();
        assert_eq!(val["k"], 3);
        assert_eq!(val["winner"], "s2");
    }

    #[test]
    fn sse_parser_handles_fragmented_chunks_and_events() {
        let mut parser = SseParser::new();
        let part1 = "event: interaction.start\ndata: {\"interaction\":{\"id\":\"int_1\"}}\n\nevent: content.delta\ndata: {\"delta\":{\"type\":\"text\",\"te";
        let part2 = "xt\":\"Hello \"}}\n\nevent: content.delta\ndata: {\"delta\":{\"type\":\"text\",\"text\":\"world\"}}\n\nevent: interaction.complete\ndata: {\"interaction\":{\"id\":\"int_1\",\"status\":\"completed\",\"outputs\":[{\"type\":\"text\",\"text\":\"Hello world\"}]}}\n\n";

        let ev1 = parser.push_chunk(part1);
        assert_eq!(ev1.len(), 1);
        assert_eq!(
            ev1[0],
            InteractionStreamEvent::InteractionStart {
                interaction_id: "int_1".into()
            }
        );

        let ev2 = parser.push_chunk(part2);
        assert_eq!(ev2.len(), 3);
        assert!(matches!(
            &ev2[0],
            InteractionStreamEvent::ContentDelta { text: Some(t), .. } if t == "Hello "
        ));
        assert!(matches!(
            &ev2[1],
            InteractionStreamEvent::ContentDelta { text: Some(t), .. } if t == "world"
        ));
        assert!(matches!(
            &ev2[2],
            InteractionStreamEvent::InteractionComplete { interaction } if interaction.id == "int_1"
        ));
    }

    #[test]
    fn serializes_decisions_request_and_parses_calibrated_answers() {
        let req = CreateDecisionRequest::new(vec![ContentPart::text(
            "Fix crash when header is empty in parser.py",
        )])
        .with_question(
            "is_bug",
            DecisionQuestion::noul("Is this a software bug report?"),
        )
        .with_question(
            "kind",
            DecisionQuestion::choice(
                "Classify the engineering task.",
                [("bug", "Fixes broken behavior"), ("feature", "Adds capability")],
            ),
        )
        .with_question(
            "quality",
            DecisionQuestion::score(
                "Rate root-cause depth.",
                ["Symptom bandaid", "Partial guard", "Full root-cause fix"],
            ),
        );

        let val: Value = serde_json::to_value(&req).unwrap();
        assert_eq!(val["model"], "gemini-decision-1");
        assert!(val.get("generation_config").is_none());
        assert_eq!(val["questions"]["is_bug"]["type"], "noul");
        assert_eq!(val["questions"]["kind"]["type"], "choice");
        assert_eq!(val["questions"]["quality"]["type"], "score");

        let raw_resp = json!({
            "answers": {
                "is_bug": { "type": "noul", "noul": 0.94, "confidence": 0.94 },
                "kind": {
                    "type": "choice",
                    "choice": "bug",
                    "probabilities": { "bug": 0.99, "feature": 0.01 },
                    "confidence": 0.99
                },
                "quality": {
                    "type": "score",
                    "score": 1.80,
                    "legend": { "0": "Symptom bandaid", "1": "Partial guard", "2": "Full root-cause fix" },
                    "probabilities": { "0": 0.02, "1": 0.16, "2": 0.82 },
                    "confidence": 0.82
                }
            },
            "usage": {
                "total_tokens": 215,
                "total_input_tokens": 215,
                "total_output_tokens": 0,
                "input_tokens_by_modality": [{ "modality": "text", "tokens": 215 }]
            }
        });

        let parsed: DecisionResponse = serde_json::from_value(raw_resp).unwrap();
        assert_eq!(parsed.noul_prob("is_bug"), Some(0.94));
        assert_eq!(parsed.choice("kind"), Some("bug"));
        let norm = parsed.score_normalized_100("quality", 3).unwrap();
        assert!((norm - 90.0).abs() < 0.01);
        assert_eq!(parsed.confidence("quality"), Some(0.82));
        assert_eq!(parsed.usage.unwrap().total_output_tokens, 0);
    }
}
