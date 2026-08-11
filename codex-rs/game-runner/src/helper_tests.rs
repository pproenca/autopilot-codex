use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use pretty_assertions::assert_eq;

use super::HelperLauncher;
use super::LaunchRequest;
use super::ReadinessLimits;
use crate::RunnerDeployment;
use crate::RunnerError;

#[test]
fn helper_launch_uses_signed_app_and_serve_socket() {
    let launcher = HelperLauncher::new(ReadinessLimits {
        timeout: Duration::from_secs(15),
        poll_interval: Duration::from_millis(100),
    });

    assert_eq!(
        launcher.launch_request(&deployment()),
        LaunchRequest {
            program: PathBuf::from("/usr/bin/open"),
            args: vec![
                "-n".into(),
                "-g".into(),
                "-j".into(),
                "/signed/AutoPilotHelper.app".into(),
                "--args".into(),
                "--serve".into(),
                "/private/game.sock".into(),
            ],
        }
    );
}

#[cfg(unix)]
#[tokio::test]
async fn missing_socket_reaches_bounded_timeout() {
    let temp = tempfile::tempdir().expect("create temporary socket parent");
    let missing_socket = temp.path().join("missing.sock");
    let launcher = HelperLauncher::new(ReadinessLimits {
        timeout: Duration::from_millis(40),
        poll_interval: Duration::from_millis(5),
    });

    let error = launcher
        .wait_for_socket(&missing_socket)
        .await
        .expect_err("missing socket should time out");

    assert!(matches!(
        error,
        RunnerError::SocketReadinessTimeout { path } if path == missing_socket
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn listening_socket_is_ready() {
    let temp = tempfile::tempdir().expect("create temporary socket parent");
    let socket = temp.path().join("helper.sock");
    let _listener = tokio::net::UnixListener::bind(&socket).expect("bind helper socket");
    let launcher = HelperLauncher::new(ReadinessLimits {
        timeout: Duration::from_secs(1),
        poll_interval: Duration::from_millis(5),
    });

    launcher
        .wait_for_socket(&socket)
        .await
        .expect("listening socket should be ready");
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn existing_helper_is_reused_without_validating_bundle() {
    let temp = tempfile::tempdir().expect("create temporary socket parent");
    let socket = temp.path().join("helper.sock");
    let _listener = tokio::net::UnixListener::bind(&socket).expect("bind helper socket");
    let launcher = HelperLauncher::new(ReadinessLimits {
        timeout: Duration::from_secs(1),
        poll_interval: Duration::from_millis(5),
    });
    let deployment = RunnerDeployment {
        helper_app: temp.path().join("missing.app"),
        socket_path: socket,
        target_app: "Gambonanza".to_string(),
        codex_home: temp.path().join("codex-home"),
    };

    launcher
        .ensure_serving(&deployment)
        .await
        .expect("existing helper should be reused");
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn invalid_helper_bundle_is_rejected_before_launch() {
    let launcher = HelperLauncher::new(ReadinessLimits {
        timeout: Duration::from_millis(40),
        poll_interval: Duration::from_millis(5),
    });

    let error = launcher
        .ensure_serving(&deployment())
        .await
        .expect_err("missing app bundle should fail validation");

    assert!(matches!(
        error,
        RunnerError::InvalidHelperApp { path }
            if path.as_path() == Path::new("/signed/AutoPilotHelper.app")
    ));
}

#[cfg(not(target_os = "macos"))]
#[tokio::test]
async fn live_launch_is_rejected_on_unsupported_platforms() {
    let launcher = HelperLauncher::new(ReadinessLimits {
        timeout: Duration::from_millis(40),
        poll_interval: Duration::from_millis(5),
    });

    let error = launcher
        .ensure_serving(&deployment())
        .await
        .expect_err("non-macOS launch should fail before validation");

    assert!(matches!(error, RunnerError::UnsupportedPlatform));
}

fn deployment() -> RunnerDeployment {
    RunnerDeployment {
        helper_app: PathBuf::from("/signed/AutoPilotHelper.app"),
        socket_path: PathBuf::from("/private/game.sock"),
        target_app: "Gambonanza".to_string(),
        codex_home: Path::new("/private/codex-home").to_path_buf(),
    }
}
