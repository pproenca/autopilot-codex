use pretty_assertions::assert_eq;

use crate::OwnerLease;

use super::OwnerLeaseError;
use super::OwnerLeaseState;

#[test]
fn generation_increment_replaces_the_complete_lease() -> anyhow::Result<()> {
    let lease = OwnerLeaseState::new("epoch-1".to_string(), 1);

    assert_eq!(
        (lease.current(), lease.increment_generation()?, lease.current()),
        (
            OwnerLease {
                epoch: "epoch-1".to_string(),
                generation: 1,
            },
            OwnerLease {
                epoch: "epoch-1".to_string(),
                generation: 2,
            },
            OwnerLease {
                epoch: "epoch-1".to_string(),
                generation: 2,
            },
        )
    );
    Ok(())
}

#[test]
fn generation_overflow_preserves_the_previous_lease() {
    let lease = OwnerLeaseState::new("epoch-1".to_string(), u64::MAX);

    assert_eq!(
        lease.increment_generation(),
        Err(OwnerLeaseError::GenerationOverflow)
    );
    assert_eq!(
        lease.current(),
        OwnerLease {
            epoch: "epoch-1".to_string(),
            generation: u64::MAX,
        }
    );
}
