use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use crate::OwnerLease;

pub struct OwnerLeaseState {
    epoch: String,
    generation: AtomicU64,
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum OwnerLeaseError {
    #[error("campaign owner generation overflowed")]
    GenerationOverflow,
}

impl OwnerLeaseState {
    pub fn new(epoch: String, generation: u64) -> Self {
        Self {
            epoch,
            generation: AtomicU64::new(generation),
        }
    }

    pub fn current(&self) -> OwnerLease {
        OwnerLease {
            epoch: self.epoch.clone(),
            generation: self.generation.load(Ordering::Acquire),
        }
    }

    pub fn increment_generation(&self) -> Result<OwnerLease, OwnerLeaseError> {
        let previous = self
            .generation
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |generation| {
                generation.checked_add(1)
            })
            .map_err(|_| OwnerLeaseError::GenerationOverflow)?;
        Ok(OwnerLease {
            epoch: self.epoch.clone(),
            generation: previous + 1,
        })
    }
}

#[cfg(test)]
#[path = "owner_lease_tests.rs"]
mod tests;
