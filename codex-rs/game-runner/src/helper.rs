use std::ffi::OsString;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use crate::RunnerDeployment;
use crate::RunnerError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadinessLimits {
    pub timeout: Duration,
    pub poll_interval: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchRequest {
    pub program: PathBuf,
    pub args: Vec<OsString>,
}

pub struct HelperLauncher {
    limits: ReadinessLimits,
}

impl HelperLauncher {
    pub fn new(limits: ReadinessLimits) -> Self {
        Self { limits }
    }

    pub fn launch_request(&self, deployment: &RunnerDeployment) -> LaunchRequest {
        LaunchRequest {
            program: PathBuf::from("/usr/bin/open"),
            args: vec![
                "-n".into(),
                "-g".into(),
                "-j".into(),
                deployment.helper_app.as_os_str().to_owned(),
                "--args".into(),
                "--serve".into(),
                deployment.socket_path.as_os_str().to_owned(),
            ],
        }
    }

    pub async fn ensure_serving(
        &self,
        deployment: &RunnerDeployment,
    ) -> Result<(), RunnerError> {
        #[cfg(not(target_os = "macos"))]
        {
            let _ = deployment;
            return Err(RunnerError::UnsupportedPlatform);
        }

        #[cfg(target_os = "macos")]
        {
            use std::os::unix::fs::PermissionsExt;

            let helper_metadata = std::fs::metadata(&deployment.helper_app).map_err(|_| {
                RunnerError::InvalidHelperApp {
                    path: deployment.helper_app.clone(),
                }
            })?;
            if !helper_metadata.is_dir()
                || deployment.helper_app.extension().and_then(|ext| ext.to_str()) != Some("app")
            {
                return Err(RunnerError::InvalidHelperApp {
                    path: deployment.helper_app.clone(),
                });
            }

            if let Some(parent) = deployment.socket_path.parent()
                && !parent.as_os_str().is_empty()
            {
                std::fs::create_dir_all(parent)
                    .map_err(|source| RunnerError::LaunchServices { source })?;
                std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
                    .map_err(|source| RunnerError::LaunchServices { source })?;
            }

            let request = self.launch_request(deployment);
            let status = tokio::process::Command::new(&request.program)
                .args(&request.args)
                .status()
                .await
                .map_err(|source| RunnerError::LaunchServices { source })?;
            if !status.success() {
                return Err(RunnerError::LaunchServicesExit {
                    status: status.to_string(),
                });
            }

            self.wait_for_socket(&deployment.socket_path).await
        }
    }

    #[cfg(unix)]
    pub(crate) async fn wait_for_socket(&self, socket_path: &Path) -> Result<(), RunnerError> {
        let deadline = tokio::time::Instant::now() + self.limits.timeout;
        loop {
            if tokio::net::UnixStream::connect(socket_path).await.is_ok() {
                return Ok(());
            }

            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Err(RunnerError::SocketReadinessTimeout {
                    path: socket_path.to_path_buf(),
                });
            }
            tokio::time::sleep(self.limits.poll_interval.min(deadline - now)).await;
        }
    }
}

#[cfg(test)]
#[path = "helper_tests.rs"]
mod tests;
