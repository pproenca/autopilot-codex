use serde::Deserialize;
use serde::Serialize;

const SUMMARY_BYTES: usize = 2 * 1024;
const ITEM_BYTES: usize = 512;
const STRATEGY_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StrategyRecord {
    pub summary: String,
    pub confirmed_mechanics: Vec<String>,
    pub failed_approaches: Vec<String>,
    pub shop_and_boss_notes: Vec<String>,
    pub next_attempt_priorities: Vec<String>,
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum StrategyValidationError {
    #[error("{field} exceeds the {max_bytes}-byte limit")]
    StringTooLarge { field: String, max_bytes: usize },
    #[error("{field} must contain between {min} and {max} items")]
    InvalidItemCount {
        field: String,
        min: usize,
        max: usize,
    },
    #[error("strategy exceeds the {max_bytes}-byte limit")]
    StrategyTooLarge { max_bytes: usize },
    #[error("failed to encode strategy")]
    Encoding,
}

impl StrategyRecord {
    pub fn validate(&self) -> Result<(), StrategyValidationError> {
        validate_string("summary", &self.summary, SUMMARY_BYTES)?;
        validate_items("confirmed_mechanics", &self.confirmed_mechanics, 0, 24)?;
        validate_items("failed_approaches", &self.failed_approaches, 0, 16)?;
        validate_items("shop_and_boss_notes", &self.shop_and_boss_notes, 0, 24)?;
        validate_items(
            "next_attempt_priorities",
            &self.next_attempt_priorities,
            1,
            8,
        )?;

        let encoded = serde_json::to_vec(self).map_err(|_| StrategyValidationError::Encoding)?;
        if encoded.len() > STRATEGY_BYTES {
            return Err(StrategyValidationError::StrategyTooLarge {
                max_bytes: STRATEGY_BYTES,
            });
        }
        Ok(())
    }
}

fn validate_items(
    field: &str,
    items: &[String],
    min: usize,
    max: usize,
) -> Result<(), StrategyValidationError> {
    if !(min..=max).contains(&items.len()) {
        return Err(StrategyValidationError::InvalidItemCount {
            field: field.to_string(),
            min,
            max,
        });
    }
    for (index, item) in items.iter().enumerate() {
        validate_string(&format!("{field}[{index}]"), item, ITEM_BYTES)?;
    }
    Ok(())
}

fn validate_string(
    field: &str,
    value: &str,
    max_bytes: usize,
) -> Result<(), StrategyValidationError> {
    if value.len() > max_bytes {
        return Err(StrategyValidationError::StringTooLarge {
            field: field.to_string(),
            max_bytes,
        });
    }
    Ok(())
}

#[cfg(test)]
#[path = "strategy_tests.rs"]
mod tests;
