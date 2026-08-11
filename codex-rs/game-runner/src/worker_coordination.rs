use codex_core_api::McpToolCallEndEvent;

use crate::GAME_SERVER_NAME;

const MAX_FAILURE_SUMMARY_BYTES: usize = 2 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkerDirective {
    Continue,
    PauseForRecovery,
}

pub(crate) struct GameToolFailureSignal {
    pub(crate) tool: String,
    pub(crate) summary: String,
    pub(crate) response: tokio::sync::oneshot::Sender<WorkerDirective>,
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub(crate) enum GameToolFailureError {
    #[error("unknown game tool {tool}")]
    UnknownTool { tool: String },
}

pub(crate) fn game_tool_failure_signal(
    event: &McpToolCallEndEvent,
) -> Result<
    Option<(
        GameToolFailureSignal,
        tokio::sync::oneshot::Receiver<WorkerDirective>,
    )>,
    GameToolFailureError,
> {
    if event.invocation.server != GAME_SERVER_NAME || event.is_success() {
        return Ok(None);
    }
    let tool = event.invocation.tool.clone();
    if !matches!(
        tool.as_str(),
        "get_app_state" | "wait" | "click" | "drag" | "focus_click" | "zoom"
    ) {
        return Err(GameToolFailureError::UnknownTool { tool });
    }
    let summary = match &event.result {
        Err(summary) if summary.is_empty() => "game tool call failed".to_string(),
        Err(summary) => bounded_summary(summary),
        Ok(_) => "game tool returned an error result".to_string(),
    };
    let (response, receiver) = tokio::sync::oneshot::channel();
    Ok(Some((
        GameToolFailureSignal {
            tool,
            summary,
            response,
        },
        receiver,
    )))
}

fn bounded_summary(summary: &str) -> String {
    if summary.len() <= MAX_FAILURE_SUMMARY_BYTES {
        return summary.to_string();
    }
    let mut boundary = MAX_FAILURE_SUMMARY_BYTES;
    while !summary.is_char_boundary(boundary) {
        boundary -= 1;
    }
    summary[..boundary].to_string()
}

#[cfg(test)]
#[path = "worker_coordination_tests.rs"]
mod tests;
