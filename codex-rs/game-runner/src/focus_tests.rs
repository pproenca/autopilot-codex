use std::sync::Arc;
use std::sync::Mutex;

use pretty_assertions::assert_eq;
use serde_json::json;

use super::ApplicationFocus;
use super::FocusTracker;

#[derive(Clone)]
struct FakeFocus {
    events: Arc<Mutex<Vec<String>>>,
}

impl ApplicationFocus for FakeFocus {
    type Lease = String;

    async fn borrow(&self) -> anyhow::Result<Self::Lease> {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push("borrow".to_string());
        Ok("previous.app".to_string())
    }

    async fn restore(&self, lease: &Self::Lease) -> anyhow::Result<()> {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(format!("restore:{lease}"));
        Ok(())
    }
}

#[tokio::test]
async fn focus_is_borrowed_only_for_a_mutation_and_restored_on_its_response() -> anyhow::Result<()>
{
    let events = Arc::new(Mutex::new(Vec::new()));
    let mut tracker = FocusTracker::new(FakeFocus {
        events: Arc::clone(&events),
    });

    tracker
        .before_request(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": "get_app_state", "arguments": {}}
        }))
        .await?;
    tracker
        .before_request(&json!({
            "jsonrpc": "2.0",
            "id": "mutation-1",
            "method": "tools/call",
            "params": {"name": "click", "arguments": {"x": 210, "y": 645}}
        }))
        .await?;
    assert_eq!(
        *events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        vec!["borrow".to_string()]
    );

    tracker
        .after_response(&json!({"jsonrpc": "2.0", "id": 1, "result": {}}))
        .await?;
    tracker
        .after_response(&json!({
            "jsonrpc": "2.0",
            "id": "mutation-1",
            "result": {"isError": false}
        }))
        .await?;

    assert_eq!(
        *events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        vec!["borrow".to_string(), "restore:previous.app".to_string()]
    );
    Ok(())
}

#[tokio::test]
async fn overlapping_mutations_fail_closed_and_cleanup_restores_focus() -> anyhow::Result<()> {
    let events = Arc::new(Mutex::new(Vec::new()));
    let mut tracker = FocusTracker::new(FakeFocus {
        events: Arc::clone(&events),
    });
    let mutation = json!({
        "jsonrpc": "2.0",
        "id": 7,
        "method": "tools/call",
        "params": {"name": "drag", "arguments": {}}
    });

    tracker.before_request(&mutation).await?;
    let error = tracker
        .before_request(&json!({
            "jsonrpc": "2.0",
            "id": 8,
            "method": "tools/call",
            "params": {"name": "focus_click", "arguments": {}}
        }))
        .await
        .expect_err("overlapping mutation must fail");
    assert_eq!(
        error.to_string(),
        "a game mutation already owns application focus"
    );

    tracker.restore_all().await?;
    assert_eq!(
        *events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        vec!["borrow".to_string(), "restore:previous.app".to_string()]
    );
    Ok(())
}
