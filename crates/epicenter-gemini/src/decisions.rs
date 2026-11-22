use crate::types::InteractionInput;
use epicenter_common::GEMINI_DECISION_MODEL;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Typed closed-set question evaluated by `POST /v1beta/decisions` (`gemini-decision-1`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DecisionQuestion {
    /// Calibrated Yes/No Bernoulli probability (`0.01..=0.99`).
    Noul { instructions: String },
    /// Closed-set single choice over 2..=255 named options (`key -> description`).
    Choice {
        instructions: String,
        criteria: BTreeMap<String, String>,
    },
    /// Ordered rubric rating over 2..=255 levels (index `0` = lowest, `len - 1` = highest).
    Score {
        instructions: String,
        criteria: Vec<String>,
    },
}

impl DecisionQuestion {
    pub fn noul(instructions: impl Into<String>) -> Self {
        Self::Noul {
            instructions: instructions.into(),
        }
    }

    pub fn choice<I, K, V>(instructions: impl Into<String>, criteria: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        Self::Choice {
            instructions: instructions.into(),
            criteria: criteria
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        }
    }

    pub fn score<I, S>(instructions: impl Into<String>, criteria: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::Score {
            instructions: instructions.into(),
            criteria: criteria.into_iter().map(Into::into).collect(),
        }
    }
}

/// Request payload for `POST /v1beta/decisions`.
///
/// Note: `/v1beta/decisions` rejects `generation_config` / `temperature` / `stream`;
/// only `model`, `input`, and `questions` (up to 1,000 keys) are permitted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateDecisionRequest {
    pub model: String,
    pub input: InteractionInput,
    pub questions: BTreeMap<String, DecisionQuestion>,
}

impl CreateDecisionRequest {
    pub fn new(input: impl Into<InteractionInput>) -> Self {
        Self {
            model: GEMINI_DECISION_MODEL.to_string(),
            input: input.into(),
            questions: BTreeMap::new(),
        }
    }

    pub fn with_question(mut self, key: impl Into<String>, question: DecisionQuestion) -> Self {
        self.questions.insert(key.into(), question);
        self
    }

    pub fn insert_question(&mut self, key: impl Into<String>, question: DecisionQuestion) {
        self.questions.insert(key.into(), question);
    }
}

/// Individual typed answer inside `DecisionResponse.answers[key]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DecisionAnswer {
    Noul {
        noul: f64,
        confidence: f64,
    },
    Choice {
        choice: String,
        #[serde(default)]
        probabilities: BTreeMap<String, f64>,
        confidence: f64,
    },
    Score {
        score: f64,
        #[serde(default)]
        legend: BTreeMap<String, String>,
        #[serde(default)]
        probabilities: BTreeMap<String, f64>,
        confidence: f64,
    },
}

impl DecisionAnswer {
    pub const fn confidence(&self) -> f64 {
        match self {
            Self::Noul { confidence, .. }
            | Self::Choice { confidence, .. }
            | Self::Score { confidence, .. } => *confidence,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ModalityTokenCount {
    #[serde(default)]
    pub modality: String,
    #[serde(default)]
    pub tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DecisionUsage {
    #[serde(default)]
    pub total_input_tokens: u64,
    #[serde(default)]
    pub total_output_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
    #[serde(default)]
    pub input_tokens_by_modality: Vec<ModalityTokenCount>,
}

/// Response payload returned by `POST /v1beta/decisions`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct DecisionResponse {
    #[serde(default)]
    pub answers: BTreeMap<String, DecisionAnswer>,
    #[serde(default)]
    pub usage: Option<DecisionUsage>,
}

impl DecisionResponse {
    /// Returns the calibrated Bernoulli `yes` probability (`0.01..=0.99`) for a `noul` question.
    pub fn noul_prob(&self, key: &str) -> Option<f64> {
        match self.answers.get(key)? {
            DecisionAnswer::Noul { noul, .. } => Some(*noul),
            _ => None,
        }
    }

    /// Returns `(choice, confidence)` for a `choice` question.
    pub fn choice_with_confidence(&self, key: &str) -> Option<(&str, f64)> {
        match self.answers.get(key)? {
            DecisionAnswer::Choice {
                choice, confidence, ..
            } => Some((choice.as_str(), *confidence)),
            _ => None,
        }
    }

    /// Returns the selected `choice` string for a `choice` question.
    pub fn choice(&self, key: &str) -> Option<&str> {
        self.choice_with_confidence(key).map(|(c, _)| c)
    }

    /// Returns the raw probability-weighted rubric score (`0.0..=(levels - 1) as f64`).
    pub fn score(&self, key: &str) -> Option<f64> {
        match self.answers.get(key)? {
            DecisionAnswer::Score { score, .. } => Some(*score),
            _ => None,
        }
    }

    /// Maps an `N`-level `score` rubric (`0..=max_level_index`) linearly onto `0.0..=100.0`.
    pub fn score_normalized_100(&self, key: &str, num_levels: usize) -> Option<f32> {
        if num_levels < 2 {
            return None;
        }
        let raw = self.score(key)?;
        let max_idx = (num_levels - 1) as f64;
        Some(((raw / max_idx) * 100.0).clamp(0.0, 100.0) as f32)
    }

    /// Returns the model's calibrated confidence (`0.0..=1.0`) for `key`.
    pub fn confidence(&self, key: &str) -> Option<f64> {
        self.answers.get(key).map(DecisionAnswer::confidence)
    }
}
