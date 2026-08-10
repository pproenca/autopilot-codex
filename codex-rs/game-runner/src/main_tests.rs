use std::path::PathBuf;

use clap::Parser;
use pretty_assertions::assert_eq;

use super::Args;

#[test]
fn parses_only_deployment_facts() {
    assert_eq!(
        Args::try_parse_from([
            "codex-game-runner",
            "--helper-app",
            "/signed/AutoPilotHelper.app",
            "--socket",
            "/private/game.sock",
            "--target-app",
            "Gambonanza",
        ])
        .expect("valid deployment arguments"),
        Args {
            helper_app: PathBuf::from("/signed/AutoPilotHelper.app"),
            socket: PathBuf::from("/private/game.sock"),
            target_app: "Gambonanza".to_string(),
        }
    );
}
