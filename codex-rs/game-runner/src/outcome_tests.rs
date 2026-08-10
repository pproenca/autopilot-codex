use pretty_assertions::assert_eq;
use serde_json::json;

use super::OutcomeDraft;
use super::OutcomeKind;
use super::OutcomeValidationError;
use crate::StrategyRecord;

fn strategy() -> StrategyRecord {
    StrategyRecord {
        summary: "Develop economy before committing to the boss".to_string(),
        confirmed_mechanics: vec!["Shop purchases use drag-to-buy".to_string()],
        failed_approaches: vec!["Early all-in left no boss answer".to_string()],
        shop_and_boss_notes: vec!["Preserve one reroll for the boss shop".to_string()],
        next_attempt_priorities: vec!["Buy mobility".to_string()],
    }
}

#[test]
fn loss_requires_and_preserves_a_complete_replacement_strategy() -> anyhow::Result<()> {
    let draft = OutcomeDraft::Loss {
        observation_reference: "sha256:loss".to_string(),
        visible_evidence_summary: "The defeat screen is visible".to_string(),
        lesson: "The build lacked boss mobility".to_string(),
        strategy: strategy(),
    };
    draft.validate()?;
    assert_eq!(draft.kind(), OutcomeKind::Loss);
    assert_eq!(draft.observation_reference(), "sha256:loss");
    assert_eq!(
        serde_json::to_value(&draft)?,
        json!({
            "outcome": "loss",
            "observation_reference": "sha256:loss",
            "visible_evidence_summary": "The defeat screen is visible",
            "lesson": "The build lacked boss mobility",
            "strategy": strategy(),
        })
    );
    Ok(())
}

#[test]
fn outcomes_are_exhaustive_and_bounded() {
    let win = OutcomeDraft::Win {
        observation_reference: "sha256:win".to_string(),
        visible_evidence_summary: "x".repeat(2049),
        lesson: "won".to_string(),
    };
    assert!(matches!(
        win.validate(),
        Err(OutcomeValidationError::StringTooLarge { .. })
    ));

    let terminal_block = OutcomeDraft::TerminalBlock {
        observation_reference: "sha256:block".to_string(),
        visible_evidence_summary: "The game cannot continue".to_string(),
        lesson: "The save is irrecoverably blocked".to_string(),
    };
    assert_eq!(terminal_block.kind(), OutcomeKind::TerminalBlock);
}
