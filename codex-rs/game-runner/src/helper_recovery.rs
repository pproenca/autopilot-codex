use std::path::Path;
use std::time::Duration;

use crate::HelperLauncher;
use crate::PauseReason;
use crate::RunnerDeployment;
use crate::RunnerError;

/// Supplies socket probes and bounded helper startup attempts to helper recovery.
///
/// Implementations must report readiness without mutating campaign state. A failed
/// startup future represents one complete recovery cycle and may be retried only
/// by `HelperRecovery` according to its configured budget.
pub trait HelperReadiness: Send + Sync {
    fn socket_is_ready(
        &self,
        socket_path: &Path,
    ) -> impl std::future::Future<Output = bool> + Send;

    fn ensure_serving(
        &self,
        deployment: &RunnerDeployment,
    ) -> impl std::future::Future<Output = Result<(), RunnerError>> + Send;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecoveryLimits {
    pub attempts: u8,
    pub readiness_timeout: Duration,
    pub backoffs: [Duration; 2],
}

impl RecoveryLimits {
    pub fn stage_4b2() -> Self {
        Self {
            attempts: 3,
            readiness_timeout: Duration::from_secs(15),
            backoffs: [Duration::from_secs(1), Duration::from_secs(2)],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryOutcome {
    Recovered { attempts: u8 },
    Exhausted { attempts: u8, reason: PauseReason },
}

pub struct HelperRecovery<R> {
    readiness: R,
    limits: RecoveryLimits,
}

impl<R: HelperReadiness> HelperRecovery<R> {
    pub fn new(readiness: R, limits: RecoveryLimits) -> Self {
        Self { readiness, limits }
    }

    pub async fn socket_is_ready(&self, socket_path: &Path) -> bool {
        self.readiness.socket_is_ready(socket_path).await
    }

    pub async fn recover(
        &self,
        deployment: &RunnerDeployment,
    ) -> Result<RecoveryOutcome, RunnerError> {
        let available_attempts = u8::try_from(self.limits.backoffs.len() + 1).map_err(|_| {
            RunnerError::CampaignFailed {
                message: "helper recovery backoff count overflowed".to_string(),
            }
        })?;
        if self.limits.attempts == 0 || self.limits.attempts > available_attempts {
            return Err(RunnerError::CampaignFailed {
                message: format!(
                    "helper recovery attempts must be between 1 and {available_attempts}"
                ),
            });
        }

        for attempt in 1..=self.limits.attempts {
            if matches!(
                tokio::time::timeout(
                    self.limits.readiness_timeout,
                    self.readiness.ensure_serving(deployment),
                )
                .await,
                Ok(Ok(()))
            ) {
                return Ok(RecoveryOutcome::Recovered { attempts: attempt });
            }
            if attempt < self.limits.attempts {
                tokio::time::sleep(self.limits.backoffs[usize::from(attempt - 1)]).await;
            }
        }

        Ok(RecoveryOutcome::Exhausted {
            attempts: self.limits.attempts,
            reason: PauseReason::HelperUnavailable {
                summary: format!(
                    "helper unavailable after {} recovery cycles",
                    self.limits.attempts
                ),
            },
        })
    }
}

impl HelperReadiness for HelperLauncher {
    fn socket_is_ready(
        &self,
        socket_path: &Path,
    ) -> impl std::future::Future<Output = bool> + Send {
        #[cfg(unix)]
        {
            let socket_path = socket_path.to_path_buf();
            async move { tokio::net::UnixStream::connect(socket_path).await.is_ok() }
        }

        #[cfg(not(unix))]
        {
            let _ = socket_path;
            std::future::ready(false)
        }
    }

    fn ensure_serving(
        &self,
        deployment: &RunnerDeployment,
    ) -> impl std::future::Future<Output = Result<(), RunnerError>> + Send {
        HelperLauncher::ensure_serving(self, deployment)
    }
}

#[cfg(test)]
#[path = "helper_recovery_tests.rs"]
mod tests;
