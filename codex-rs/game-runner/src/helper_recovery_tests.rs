use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use pretty_assertions::assert_eq;

use super::HelperReadiness;
use super::HelperRecovery;
use super::RecoveryLimits;
use super::RecoveryOutcome;
use crate::PauseReason;
use crate::RunnerDeployment;
use crate::RunnerError;

struct FakeReadiness {
    results: Mutex<VecDeque<bool>>,
    attempts: Arc<Mutex<Vec<tokio::time::Instant>>>,
}

impl FakeReadiness {
    fn new(
        results: impl IntoIterator<Item = bool>,
    ) -> (Self, Arc<Mutex<Vec<tokio::time::Instant>>>) {
        let attempts = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                results: Mutex::new(results.into_iter().collect()),
                attempts: Arc::clone(&attempts),
            },
            attempts,
        )
    }
}

impl HelperReadiness for FakeReadiness {
    fn socket_is_ready(
        &self,
        _socket_path: &Path,
    ) -> impl std::future::Future<Output = bool> + Send {
        std::future::ready(false)
    }

    fn ensure_serving(
        &self,
        _deployment: &RunnerDeployment,
    ) -> impl std::future::Future<Output = Result<(), RunnerError>> + Send {
        self.attempts
            .lock()
            .expect("attempt log mutex poisoned")
            .push(tokio::time::Instant::now());
        let result = self
            .results
            .lock()
            .expect("result queue mutex poisoned")
            .pop_front()
            .unwrap_or(false);
        std::future::ready(if result {
            Ok(())
        } else {
            Err(RunnerError::CampaignFailed {
                message: "helper unavailable".to_string(),
            })
        })
    }
}

#[test]
fn stage_4b2_limits_are_exact() {
    assert_eq!(
        RecoveryLimits::stage_4b2(),
        RecoveryLimits {
            attempts: 3,
            readiness_timeout: Duration::from_secs(15),
            backoffs: [Duration::from_secs(1), Duration::from_secs(2)],
        }
    );
}

#[tokio::test(start_paused = true)]
async fn recovery_succeeds_on_each_cycle_with_exact_backoffs() -> anyhow::Result<()> {
    for (results, expected_attempts, expected_offsets) in [
        (vec![true], 1, vec![Duration::ZERO]),
        (
            vec![false, true],
            2,
            vec![Duration::ZERO, Duration::from_secs(1)],
        ),
        (
            vec![false, false, true],
            3,
            vec![
                Duration::ZERO,
                Duration::from_secs(1),
                Duration::from_secs(3),
            ],
        ),
    ] {
        let start = tokio::time::Instant::now();
        let (readiness, attempts) = FakeReadiness::new(results);
        let outcome = HelperRecovery::new(readiness, RecoveryLimits::stage_4b2())
            .recover(&deployment())
            .await?;

        assert_eq!(
            outcome,
            RecoveryOutcome::Recovered {
                attempts: expected_attempts,
            }
        );
        assert_eq!(
            attempts
                .lock()
                .expect("attempt log mutex poisoned")
                .iter()
                .map(|attempt| attempt.duration_since(start))
                .collect::<Vec<_>>(),
            expected_offsets
        );
    }
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn recovery_exhausts_after_three_cycles_without_a_fourth() -> anyhow::Result<()> {
    let start = tokio::time::Instant::now();
    let (readiness, attempts) = FakeReadiness::new([false, false, false, true]);
    let outcome = HelperRecovery::new(readiness, RecoveryLimits::stage_4b2())
        .recover(&deployment())
        .await?;

    assert_eq!(
        outcome,
        RecoveryOutcome::Exhausted {
            attempts: 3,
            reason: PauseReason::HelperUnavailable {
                summary: "helper unavailable after 3 recovery cycles".to_string(),
            },
        }
    );
    assert_eq!(
        attempts
            .lock()
            .expect("attempt log mutex poisoned")
            .iter()
            .map(|attempt| attempt.duration_since(start))
            .collect::<Vec<_>>(),
        vec![
            Duration::ZERO,
            Duration::from_secs(1),
            Duration::from_secs(3),
        ]
    );
    Ok(())
}

fn deployment() -> RunnerDeployment {
    RunnerDeployment {
        helper_app: "/Applications/GameHelper.app".into(),
        socket_path: "/tmp/game-helper.sock".into(),
        target_app: "Difficult Game".to_string(),
        codex_home: "/tmp/codex-home".into(),
    }
}
