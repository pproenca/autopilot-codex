use serde::Deserialize;
use serde::Serialize;

use crate::decision::ObservationEvidence;
use crate::strategy::StrategyRecord;
use crate::strategy::StrategyValidationError;

const STRING_BYTES: usize = 2 * 1024;
const OUTCOME_BYTES: usize = 24 * 1024;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum OutcomeDraft {
    Loss {
        observation_reference: String,
        visible_evidence_summary: String,
        lesson: String,
        strategy: StrategyRecord,
    },
    Win {
        observation_reference: String,
        visible_evidence_summary: String,
        lesson: String,
    },
    TerminalBlock {
        observation_reference: String,
        visible_evidence_summary: String,
        lesson: String,
    },
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeKind {
    Loss,
    Win,
    TerminalBlock,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ReportedOutcome {
    pub observation: ObservationEvidence,
    pub draft: OutcomeDraft,
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum OutcomeValidationError {
    #[error("{field} exceeds the {max_bytes}-byte limit")]
    StringTooLarge { field: String, max_bytes: usize },
    #[error("outcome exceeds the {max_bytes}-byte limit")]
    OutcomeTooLarge { max_bytes: usize },
    #[error(transparent)]
    Strategy(#[from] StrategyValidationError),
    #[error("failed to encode outcome")]
    Encoding,
}

impl OutcomeDraft {
    pub fn kind(&self) -> OutcomeKind {
        match self {
            Self::Loss { .. } => OutcomeKind::Loss,
            Self::Win { .. } => OutcomeKind::Win,
            Self::TerminalBlock { .. } => OutcomeKind::TerminalBlock,
        }
    }

    pub fn observation_reference(&self) -> &str {
        match self {
            Self::Loss {
                observation_reference,
                ..
            }
            | Self::Win {
                observation_reference,
                ..
            }
            | Self::TerminalBlock {
                observation_reference,
                ..
            } => observation_reference,
        }
    }

    pub fn validate(&self) -> Result<(), OutcomeValidationError> {
        let (observation_reference, visible_evidence_summary, lesson) = match self {
            Self::Loss {
                observation_reference,
                visible_evidence_summary,
                lesson,
                strategy,
            } => {
                strategy.validate()?;
                (observation_reference, visible_evidence_summary, lesson)
            }
            | Self::Win {
                observation_reference,
                visible_evidence_summary,
                lesson,
            }
            | Self::TerminalBlock {
                observation_reference,
                visible_evidence_summary,
                lesson,
            } => (observation_reference, visible_evidence_summary, lesson),
        };
        for (field, value) in [
            ("observation_reference", observation_reference),
            ("visible_evidence_summary", visible_evidence_summary),
            ("lesson", lesson),
        ] {
            validate_string(field, value)?;
        }
        let encoded = serde_json::to_vec(self).map_err(|_| OutcomeValidationError::Encoding)?;
        if encoded.len() > OUTCOME_BYTES {
            return Err(OutcomeValidationError::OutcomeTooLarge {
                max_bytes: OUTCOME_BYTES,
            });
        }
        Ok(())
    }
}

fn validate_string(field: &str, value: &str) -> Result<(), OutcomeValidationError> {
    if value.len() > STRING_BYTES {
        return Err(OutcomeValidationError::StringTooLarge {
            field: field.to_string(),
            max_bytes: STRING_BYTES,
        });
    }
    Ok(())
}

#[cfg(test)]
#[path = "outcome_tests.rs"]
mod tests;
