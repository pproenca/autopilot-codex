pub const MAX_ACTIONS_PER_TURN: u8 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ActionBatch {
    used: u8,
    closed: bool,
}

#[derive(Debug, Clone, Copy, thiserror::Error, PartialEq, Eq)]
pub(crate) enum ActionBatchError {
    #[error(
        "the eight-action turn batch is exhausted; verify the latest action and finish this turn"
    )]
    Exhausted,
    #[error("the action batch is closed by a reported campaign outcome")]
    Closed,
}

impl ActionBatch {
    pub(crate) fn new() -> Self {
        Self {
            used: 0,
            closed: false,
        }
    }

    pub(crate) fn authorize(&mut self) -> Result<(), ActionBatchError> {
        if self.closed {
            return Err(ActionBatchError::Closed);
        }
        if self.used >= MAX_ACTIONS_PER_TURN {
            return Err(ActionBatchError::Exhausted);
        }
        self.used = self
            .used
            .checked_add(1)
            .ok_or(ActionBatchError::Exhausted)?;
        Ok(())
    }

    pub(crate) fn reset(&mut self) {
        *self = Self::new();
    }

    pub(crate) fn close(&mut self) {
        self.closed = true;
    }

    pub(crate) fn used(self) -> u8 {
        self.used
    }

    pub(crate) fn is_closed(self) -> bool {
        self.closed
    }
}

#[cfg(test)]
#[path = "action_batch_tests.rs"]
mod tests;
