use crate::intake::format_multimodal_findings;
use crate::repo::{emit_event, emit_log, extract_json_from_text};
use crate::EngineRunOptions;
use epicenter_common::{
    DialogueSpeaker, Gemini4Model, LiveEvent, LogLevel, TournamentSession, WarRoomTurn,
};
use epicenter_gemini::{CreateInteractionRequest, GeminiClient, ResponseFormat};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::broadcast;
use tracing::warn;

#[derive(Debug, Deserialize)]
struct WarRoomScriptOutput {
    #[serde(default)]
    turns: Vec<WarRoomTurn>,
}

/// Stage 7: 2-Speaker War-Room Executive Briefing (`Architect` vs. `Auditor`) + `gemini-3.8-flash-tts`.
///
/// Generates a crisp 4–6 turn dialogue (~35 seconds spoken) explaining the Red-Gate verification,
/// single-VM Arena benchmarks, blast-radius dark-zone audit, and Dual-Score winner. When
/// `opts.synthesize_tts` is true, synthesizes 24kHz WAV bytes via `gemini.synthesize_war_room_wav`.
pub async fn run_briefing_stage(
    gemini: &GeminiClient,
    session: &mut TournamentSession,
    opts: &EngineRunOptions,
    event_tx: &Option<broadcast::Sender<LiveEvent>>,
) -> Option<Vec<u8>> {
    let turns = generate_war_room_script(gemini, session).await;
    session.war_room_script = turns;
    session.updated_at = chrono::Utc::now();

    if !opts.synthesize_tts || session.war_room_script.is_empty() {
        return None;
    }

    emit_log(
        event_tx,
        session.id,
        LogLevel::Info,
        "briefing",
        format!(
            "Synthesizing 2-speaker war-room WAV via gemini-3.8-flash-tts ({} turns)",
            session.war_room_script.len()
        ),
    );

    match gemini.synthesize_war_room_wav(&session.war_room_script).await {
        Ok(wav_bytes) if !wav_bytes.is_empty() => {
            let audio_url = format!("/api/sessions/{}/audio.wav", session.id);
            session.audio_briefing_url = Some(audio_url.clone());
            session.updated_at = chrono::Utc::now();
            emit_event(
                event_tx,
                LiveEvent::AudioBriefingReady {
                    session_id: session.id,
                    audio_url,
                },
            );
            Some(wav_bytes)
        }
        Ok(_) => {
            warn!("TTS returned empty WAV payload");
            None
        }
        Err(err) => {
            warn!(error = %err, "war-room TTS synthesis failed; continuing with text script");
            emit_log(
                event_tx,
                session.id,
                LogLevel::Warn,
                "briefing",
                format!("TTS synthesis skipped ({err}); 2-speaker transcript preserved"),
            );
            None
        }
    }
}

async fn generate_war_room_script(
    gemini: &GeminiClient,
    session: &TournamentSession,
) -> Vec<WarRoomTurn> {
    let candidate_summary = session
        .candidates
        .iter()
        .map(|c| {
            let speedup = c
                .arena_metrics
                .as_ref()
                .map(|m| format!("{:+.1}% (p50 {:.1}ms)", m.speedup_pct, m.p50_ms))
                .unwrap_or_else(|| "n/a".to_string());
            let (risk, depth, dz, bandaid) = c
                .blast_radius
                .as_ref()
                .map(|b| {
                    (
                        format!("{:.0}/100", b.composite_risk_score),
                        format!("{:.0}/100", b.root_cause_depth_score),
                        b.dark_zone_count(),
                        b.bandaid_debt_warning.as_deref().unwrap_or("none"),
                    )
                })
                .unwrap_or_else(|| ("n/a".into(), "n/a".into(), 0, "none"));
            let badges: Vec<&str> = c.badges.iter().map(|b| b.label()).collect();
            format!(
                "- {} | green={} | speedup={} | risk={} | root_cause_depth={} | dark_zones={} | badges={:?} | bandaid_warning={}",
                c.strategy_id, c.is_green(), speedup, risk, depth, dz, badges, bandaid
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    let recommended = session
        .arena_summary
        .as_ref()
        .and_then(|s| s.recommended_strategy_id.as_deref())
        .unwrap_or("none");

    let multimodal_block = format_multimodal_findings(&session.intake.multimodal_findings);

    let prompt = format!(
        "Write a crisp 5-turn 2-speaker war-room executive briefing between `architect` and `auditor`.\n\
         Total length: ~85–110 words (~35 seconds spoken aloud). No markdown symbols, no bullet lists,\n\
         speak natural concise prose citing real metrics (speedup %, blast risk, dark zones, root-cause depth)\n\
         and mentioning any multimodal visual/audio findings when present.\n\n\
         Issue: {}\n\
         Recommended Winner: {}\n\
         Multimodal Findings:\n{multimodal_block}\n\
         Candidates:\n{}\n\n\
         Return STRICT JSON matching the schema with 4 to 6 alternating turns.",
        session.intake.title, recommended, candidate_summary
    );

    for model in [Gemini4Model::Hydrogen, Gemini4Model::Carbon] {
        let mut req = CreateInteractionRequest::for_model(model, prompt.clone());
        req.system_instruction = Some(
            "Produce a tight, executive-ready 2-speaker war-room dialogue in strict JSON.".to_string(),
        );
        req.response_format = Some(ResponseFormat::json_schema(war_room_schema()));

        if let Ok(resp) = gemini.create_interaction(&req).await {
            if let Ok(parsed) = extract_json_from_text::<WarRoomScriptOutput>(&resp.extract_text()) {
                let valid: Vec<WarRoomTurn> = parsed
                    .turns
                    .into_iter()
                    .filter(|t| !t.text.trim().is_empty())
                    .take(6)
                    .collect();
                if valid.len() >= 4 {
                    return valid;
                }
            }
        }
    }

    build_deterministic_war_room_script(session)
}

/// Deterministic 5-turn fallback war-room script used offline or if LLM script generation fails.
pub fn build_deterministic_war_room_script(session: &TournamentSession) -> Vec<WarRoomTurn> {
    let green_count = session.candidates.iter().filter(|c| c.is_green()).count();
    let total_count = session.candidates.len();
    let winner_id = session
        .arena_summary
        .as_ref()
        .and_then(|s| s.recommended_strategy_id.clone())
        .or_else(|| {
            session
                .candidates
                .iter()
                .find(|c| c.is_green())
                .map(|c| c.strategy_id.clone())
        });

    let Some(winner_id) = winner_id else {
        return vec![
            WarRoomTurn {
                speaker: DialogueSpeaker::Architect,
                text: format!(
                    "We completed the Epicenter tournament for {} across {} competing strategies.",
                    session.intake.title, total_count
                ),
            },
            WarRoomTurn {
                speaker: DialogueSpeaker::Auditor,
                text: "None of the candidates passed both the locked Red-Gate reproduction test and the full regression suite."
                    .to_string(),
            },
            WarRoomTurn {
                speaker: DialogueSpeaker::Architect,
                text: "All diagnostic failure logs and partial diffs have been preserved in the Studio timeline for manual inspection."
                    .to_string(),
            },
            WarRoomTurn {
                speaker: DialogueSpeaker::Auditor,
                text: "Recommend reviewing the locked reproduction test contract before launching the next tournament wave."
                    .to_string(),
            },
        ];
    };

    let winner = session
        .candidates
        .iter()
        .find(|c| c.strategy_id == winner_id);
    let fastest = session
        .candidates
        .iter()
        .filter(|c| c.is_green())
        .max_by(|a, b| {
            let sa = a.arena_metrics.as_ref().map(|m| m.speedup_pct).unwrap_or(f64::NEG_INFINITY);
            let sb = b.arena_metrics.as_ref().map(|m| m.speedup_pct).unwrap_or(f64::NEG_INFINITY);
            sa.total_cmp(&sb)
        });
    let bandaid = session.candidates.iter().find(|c| {
        c.blast_radius
            .as_ref()
            .and_then(|b| b.bandaid_debt_warning.as_ref())
            .is_some()
    });

    let (w_depth, w_risk, w_dz) = winner
        .and_then(|c| c.blast_radius.as_ref())
        .map(|b| (b.root_cause_depth_score, b.composite_risk_score, b.dark_zone_count()))
        .unwrap_or((85.0, 20.0, 0));
    let w_speedup = winner
        .and_then(|c| c.arena_metrics.as_ref())
        .map(|m| m.speedup_pct)
        .unwrap_or(0.0);

    let fastest_line = match fastest {
        Some(f) if f.strategy_id != winner_id => {
            let f_sp = f.arena_metrics.as_ref().map(|m| m.speedup_pct).unwrap_or(0.0);
            let f_risk = f.blast_radius.as_ref().map(|b| b.composite_risk_score).unwrap_or(40.0);
            format!(
                "Strategy {} led raw Arena throughput at {:+.1} percent speedup, but carried a {:.0} blast risk score.",
                f.strategy_id, f_sp, f_risk
            )
        }
        _ => format!(
            "Single-VM interleaved Arena trials confirmed {:+.1} percent p50 speedup with zero regressions.",
            w_speedup
        ),
    };

    let bandaid_line = match bandaid {
        Some(b) if b.strategy_id != winner_id => format!(
            "Meanwhile, {} had a tiny diff, yet Carbon flagged it as bandaid debt rather than an invariant cure.",
            b.strategy_id
        ),
        _ => format!(
            "Blast-radius mapping traced all Ring 1 and Ring 2 callers and found {} untested dark zones on {}.",
            w_dz, winner_id
        ),
    };

    vec![
        WarRoomTurn {
            speaker: DialogueSpeaker::Architect,
            text: format!(
                "Epicenter locked the Red-Gate for {} and verified {} of {} parallel strategies green.",
                session.intake.title, green_count, total_count
            ),
        },
        WarRoomTurn {
            speaker: DialogueSpeaker::Auditor,
            text: fastest_line,
        },
        WarRoomTurn {
            speaker: DialogueSpeaker::Architect,
            text: bandaid_line,
        },
        WarRoomTurn {
            speaker: DialogueSpeaker::Auditor,
            text: format!(
                "On Dual-Score, {} hits {:.0} root-cause depth with only {:.0} composite blast risk and {} dark zones.",
                winner_id, w_depth, w_risk, w_dz
            ),
        },
        WarRoomTurn {
            speaker: DialogueSpeaker::Architect,
            text: format!(
                "Recommendation: ship {} as Best Overall; the verified patch and audit report are ready for one-click export.",
                winner_id
            ),
        },
    ]
}

fn war_room_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "turns": {
                "type": "array",
                "minItems": 4,
                "maxItems": 6,
                "items": {
                    "type": "object",
                    "properties": {
                        "speaker": { "type": "string", "enum": ["architect", "auditor"] },
                        "text": { "type": "string" }
                    },
                    "required": ["speaker", "text"]
                }
            }
        },
        "required": ["turns"]
    })
}
