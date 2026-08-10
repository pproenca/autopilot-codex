use std::collections::HashMap;
use std::future::Future;
use std::time::Duration;

use anyhow::Context;
use anyhow::ensure;
use serde_json::Value;

/// Borrows application focus for one physical action and restores it afterward.
pub(crate) trait ApplicationFocus: Send + Sync {
    type Lease: Send + Sync;

    fn borrow(&self) -> impl Future<Output = anyhow::Result<Self::Lease>> + Send;

    fn restore(&self, lease: &Self::Lease) -> impl Future<Output = anyhow::Result<()>> + Send;
}

pub(crate) struct MacOsApplicationFocus {
    target_app: String,
    settle_delay: Duration,
}

impl MacOsApplicationFocus {
    pub(crate) fn new(target_app: impl Into<String>) -> Self {
        Self {
            target_app: target_app.into(),
            settle_delay: Duration::from_millis(300),
        }
    }
}

pub(crate) struct FocusLease {
    previous_bundle_id: String,
}

pub(crate) struct NoApplicationFocus;

impl ApplicationFocus for NoApplicationFocus {
    type Lease = ();

    async fn borrow(&self) -> anyhow::Result<Self::Lease> {
        Ok(())
    }

    async fn restore(&self, _lease: &Self::Lease) -> anyhow::Result<()> {
        Ok(())
    }
}

impl ApplicationFocus for MacOsApplicationFocus {
    type Lease = FocusLease;

    async fn borrow(&self) -> anyhow::Result<Self::Lease> {
        #[cfg(not(target_os = "macos"))]
        anyhow::bail!("application focus borrowing requires macOS");

        #[cfg(target_os = "macos")]
        {
            let previous_bundle_id = run_osascript(
                "tell application \"System Events\" to get bundle identifier of first application process whose frontmost is true",
                None,
            )
            .await
            .context("identify the frontmost application before game input")?;
            ensure!(
                !previous_bundle_id.is_empty(),
                "frontmost application has no bundle identifier"
            );
            run_osascript(
                "on run argv\nset targetName to item 1 of argv\ntell application \"System Events\" to set frontmost of first application process whose name is targetName to true\nend run",
                Some(&self.target_app),
            )
            .await
            .with_context(|| format!("activate {} before game input", self.target_app))?;
            tokio::time::sleep(self.settle_delay).await;
            Ok(FocusLease { previous_bundle_id })
        }
    }

    async fn restore(&self, lease: &Self::Lease) -> anyhow::Result<()> {
        #[cfg(not(target_os = "macos"))]
        anyhow::bail!("application focus restoration requires macOS");

        #[cfg(target_os = "macos")]
        {
            run_osascript(
                "on run argv\nset targetBundle to item 1 of argv\ntell application \"System Events\" to set frontmost of first application process whose bundle identifier is targetBundle to true\nend run",
                Some(&lease.previous_bundle_id),
            )
            .await
            .with_context(|| {
                format!(
                    "restore frontmost application {} after game input",
                    lease.previous_bundle_id
                )
            })?;
            Ok(())
        }
    }
}

#[cfg(target_os = "macos")]
async fn run_osascript(script: &str, argument: Option<&str>) -> anyhow::Result<String> {
    let mut command = tokio::process::Command::new("/usr/bin/osascript");
    command.arg("-e").arg(script);
    if let Some(argument) = argument {
        command.arg(argument);
    }
    let output = command.output().await.context("run osascript")?;
    ensure!(
        output.status.success(),
        "osascript failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub(crate) struct FocusTracker<F: ApplicationFocus> {
    focus: F,
    leases: HashMap<String, F::Lease>,
}

impl<F: ApplicationFocus> FocusTracker<F> {
    pub(crate) fn new(focus: F) -> Self {
        Self {
            focus,
            leases: HashMap::new(),
        }
    }

    pub(crate) async fn before_request(&mut self, request: &Value) -> anyhow::Result<()> {
        let Some(request_id) = mutation_request_id(request)? else {
            return Ok(());
        };
        ensure!(
            self.leases.is_empty(),
            "a game mutation already owns application focus"
        );
        let lease = self.focus.borrow().await?;
        self.leases.insert(request_id, lease);
        Ok(())
    }

    pub(crate) async fn after_response(&mut self, response: &Value) -> anyhow::Result<()> {
        let Some(response_id) = response_id(response)? else {
            return Ok(());
        };
        let Some(lease) = self.leases.get(&response_id) else {
            return Ok(());
        };
        self.focus.restore(lease).await?;
        self.leases.remove(&response_id);
        Ok(())
    }

    pub(crate) async fn restore_all(&mut self) -> anyhow::Result<()> {
        while let Some(request_id) = self.leases.keys().next().cloned() {
            let lease = self
                .leases
                .get(&request_id)
                .context("focus lease disappeared during cleanup")?;
            self.focus.restore(lease).await?;
            self.leases.remove(&request_id);
        }
        Ok(())
    }
}

fn mutation_request_id(request: &Value) -> anyhow::Result<Option<String>> {
    if request.get("method").and_then(Value::as_str) != Some("tools/call") {
        return Ok(None);
    }
    let Some(tool) = request.pointer("/params/name").and_then(Value::as_str) else {
        return Ok(None);
    };
    if !matches!(tool, "click" | "drag" | "focus_click") {
        return Ok(None);
    }
    let id = request
        .get("id")
        .context("game mutation request omitted its JSON-RPC id")?;
    ensure!(
        !id.is_null(),
        "game mutation request used a null JSON-RPC id"
    );
    Ok(Some(serde_json::to_string(id)?))
}

fn response_id(response: &Value) -> anyhow::Result<Option<String>> {
    if response.get("result").is_none() && response.get("error").is_none() {
        return Ok(None);
    }
    let Some(id) = response.get("id") else {
        return Ok(None);
    };
    if id.is_null() {
        return Ok(None);
    }
    Ok(Some(serde_json::to_string(id)?))
}

#[cfg(test)]
#[path = "focus_tests.rs"]
mod tests;
