use pretty_assertions::assert_eq;

use super::continuation_prompt;
use super::initial_prompt;
use super::new_attempt_prompt;

#[test]
fn initial_prompt_requires_pixel_planning_batches_losses_and_victory() {
    assert_eq!(
        initial_prompt("Gambonanza"),
        concat!(
            "Play the currently visible Gambonanza game until a full-game victory is visibly verified. ",
            "Use only fresh full-frame pixels from mcp__game__get_app_state. Before every click, drag, ",
            "or focus-click, call game_runner.record_plan with two to four candidates and one exact action. ",
            "After every dispatched mutation, capture fresh pixels before planning again. A turn permits at ",
            "most eight mutations; finish the turn when the batch is exhausted and the runner will continue. ",
            "Call game_runner.report_outcome only for a visible loss, full-game win, or terminal infrastructure ",
            "block. A loss must include a complete replacement strategy and ends only the attempt. Use visible ",
            "restart controls on the next attempt. Difficulty, an unchanged screen, or a failed strategy is not ",
            "a stopping condition. Never retry an indeterminate mutation without observing first."
        )
    );
}

#[test]
fn continuation_prompts_are_short_and_attempt_specific() {
    assert_eq!(
        continuation_prompt(2),
        concat!(
            "Continue attempt 2 of the same campaign. Capture fresh full-frame pixels before planning. ",
            "Keep using one recorded plan per mutation and fresh pixels after every mutation. Report only ",
            "a visible loss, full-game win, or terminal infrastructure block."
        )
    );
    assert_eq!(
        new_attempt_prompt(3),
        concat!(
            "Start attempt 3 of the same campaign. Use the visible restart controls, guided by the replacement ",
            "strategy you just recorded. Capture fresh full-frame pixels before planning, and keep one recorded ",
            "plan plus one fresh post-mutation observation for every action."
        )
    );
}
