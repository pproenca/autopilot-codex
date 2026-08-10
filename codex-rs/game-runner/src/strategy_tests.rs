use pretty_assertions::assert_eq;

use super::StrategyRecord;
use super::StrategyValidationError;

fn strategy() -> StrategyRecord {
    StrategyRecord {
        summary: "Develop economy before committing to the boss".to_string(),
        confirmed_mechanics: vec!["Shop purchases use drag-to-buy".to_string()],
        failed_approaches: vec!["Early all-in left no boss answer".to_string()],
        shop_and_boss_notes: vec!["Preserve one reroll for the boss shop".to_string()],
        next_attempt_priorities: vec![
            "Buy mobility".to_string(),
            "Keep a defensive superpower".to_string(),
        ],
    }
}

#[test]
fn bounded_strategy_round_trips_as_one_typed_value() -> anyhow::Result<()> {
    let expected = strategy();
    expected.validate()?;
    assert_eq!(
        serde_json::from_slice::<StrategyRecord>(&serde_json::to_vec(&expected)?)?,
        expected
    );
    Ok(())
}

#[test]
fn strategy_rejects_field_collection_and_aggregate_overflow() {
    let mut oversized_field = strategy();
    oversized_field.confirmed_mechanics[0] = "x".repeat(513);
    assert_eq!(
        oversized_field.validate(),
        Err(StrategyValidationError::StringTooLarge {
            field: "confirmed_mechanics[0]".to_string(),
            max_bytes: 512,
        })
    );

    let mut missing_priority = strategy();
    missing_priority.next_attempt_priorities.clear();
    assert!(matches!(
        missing_priority.validate(),
        Err(StrategyValidationError::InvalidItemCount { .. })
    ));

    let aggregate = StrategyRecord {
        summary: "x".repeat(2 * 1024),
        confirmed_mechanics: vec!["x".repeat(512); 24],
        failed_approaches: vec!["x".repeat(512); 16],
        shop_and_boss_notes: vec!["x".repeat(512); 24],
        next_attempt_priorities: vec!["x".repeat(512); 8],
    };
    assert_eq!(
        aggregate.validate(),
        Err(StrategyValidationError::StrategyTooLarge {
            max_bytes: 16 * 1024,
        })
    );
}
