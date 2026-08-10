use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;

use clap::Parser;
use codex_game_runner::CampaignReport;
use pretty_assertions::assert_eq;

use super::Args;
use super::dispatch_main;
use super::run;

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

#[test]
fn runner_entry_uses_the_codex_main_runtime() -> anyhow::Result<()> {
    let observed_thread = Arc::new(Mutex::new(None));
    let observed_thread_for_run = Arc::clone(&observed_thread);

    dispatch_main(move |_| async move {
        *observed_thread_for_run
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            std::thread::current().name().map(str::to_string);
        Ok(())
    })?;

    assert_eq!(
        *observed_thread
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        Some("codex-main".to_string())
    );
    Ok(())
}

#[test]
fn production_run_returns_a_campaign_report() {
    fn assert_campaign_report(
        future: impl Future<Output = anyhow::Result<CampaignReport>>,
    ) {
        drop(future);
    }

    assert_campaign_report(run(
        Args {
            helper_app: PathBuf::from("/signed/AutoPilotHelper.app"),
            socket: PathBuf::from("/private/game.sock"),
            target_app: "Gambonanza".to_string(),
        },
        PathBuf::from("/bin/codex-game-runner"),
    ));
}
