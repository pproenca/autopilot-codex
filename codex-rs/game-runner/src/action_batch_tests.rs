use pretty_assertions::assert_eq;

use super::ActionBatch;
use super::ActionBatchError;
use super::MAX_ACTIONS_PER_TURN;

#[test]
fn batch_authorizes_exactly_eight_actions_and_resets() -> anyhow::Result<()> {
    let mut batch = ActionBatch::new();
    for used in 1..=MAX_ACTIONS_PER_TURN {
        batch.authorize()?;
        assert_eq!((batch.used(), batch.is_closed()), (used, false));
    }
    assert_eq!(batch.authorize(), Err(ActionBatchError::Exhausted));
    batch.reset();
    assert_eq!(batch, ActionBatch::new());
    Ok(())
}

#[test]
fn closed_batch_rejects_actions_until_reset() {
    let mut batch = ActionBatch::new();
    batch.close();
    assert_eq!(batch.authorize(), Err(ActionBatchError::Closed));
}
