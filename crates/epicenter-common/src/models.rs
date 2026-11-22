use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use thiserror::Error;

/// Gemini 3.8 Flash TTS model used for executive audio briefings.
pub const GEMINI_TTS_MODEL: &str = "gemini-3.8-flash-tts";

/// Gemini Decision 1 model powering the zero-output-token `/v1beta/decisions` endpoint.
pub const GEMINI_DECISION_MODEL: &str = "gemini-decision-1";

/// Base Managed Agent identifier on Gemini Interactions API.
pub const MANAGED_AGENT_BASE: &str = "antigravity-preview-09-2026";

/// Default custom Managed Agent name registered by Epicenter.
pub const EPICENTER_NEON_AGENT_NAME: &str = "epicenter-neon-worker";

#[derive(Debug, Error)]
#[error("unknown Gemini 4 model identifier: '{0}'")]
pub struct ParseModelError(pub String);

/// Gemini 4 model family tiers used across the Epicenter pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Gemini4Model {
    #[serde(rename = "gemini-4-hydrogen")]
    Hydrogen,
    #[serde(rename = "gemini-4-neon")]
    Neon,
    #[serde(rename = "gemini-4-carbon")]
    Carbon,
    #[serde(rename = "gemini-4-argon")]
    Argon,
}

impl Gemini4Model {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Hydrogen => "gemini-4-hydrogen",
            Self::Neon => "gemini-4-neon",
            Self::Carbon => "gemini-4-carbon",
            Self::Argon => "gemini-4-argon",
        }
    }

    pub const fn display_name(&self) -> &'static str {
        match self {
            Self::Hydrogen => "Gemini 4 Hydrogen",
            Self::Neon => "Gemini 4 Neon",
            Self::Carbon => "Gemini 4 Carbon",
            Self::Argon => "Gemini 4 Argon",
        }
    }

    pub const fn all() -> &'static [Gemini4Model] {
        &[Self::Hydrogen, Self::Neon, Self::Carbon, Self::Argon]
    }
}

impl fmt::Display for Gemini4Model {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Gemini4Model {
    type Err = ParseModelError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "gemini-4-hydrogen" | "hydrogen" => Ok(Self::Hydrogen),
            "gemini-4-neon" | "neon" => Ok(Self::Neon),
            "gemini-4-carbon" | "carbon" => Ok(Self::Carbon),
            "gemini-4-argon" | "argon" => Ok(Self::Argon),
            other => Err(ParseModelError(other.to_string())),
        }
    }
}
