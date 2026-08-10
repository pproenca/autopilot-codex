use std::path::Path;
use std::time::Duration;

use anyhow::Context;
use serde_json::Value;
use serde_json::json;
use sha2::Digest;
use sha2::Sha256;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::net::unix::OwnedReadHalf;
use tokio::net::unix::OwnedWriteHalf;

const SOCKET_TIMEOUT: Duration = Duration::from_secs(5);

pub fn method(message: &Value) -> anyhow::Result<&str> {
    message["method"]
        .as_str()
        .context("MCP message has no method")
}

pub async fn next_message(
    lines: &mut tokio::io::Lines<BufReader<OwnedReadHalf>>,
) -> anyhow::Result<Value> {
    let line = tokio::time::timeout(SOCKET_TIMEOUT, lines.next_line())
        .await??
        .context("MCP client closed the socket")?;
    Ok(serde_json::from_str(&line)?)
}

pub async fn respond(
    writer: &mut OwnedWriteHalf,
    request: &Value,
    result: Value,
) -> anyhow::Result<()> {
    let mut encoded = serde_json::to_vec(&json!({
        "jsonrpc": "2.0",
        "id": request["id"],
        "result": result,
    }))?;
    encoded.push(b'\n');
    writer.write_all(&encoded).await?;
    writer.flush().await?;
    Ok(())
}

pub fn write_spooled_jpeg(
    spool_root: &Path,
    blob_id: &str,
    jpeg: &[u8],
) -> anyhow::Result<String> {
    std::fs::create_dir_all(spool_root)?;
    std::fs::write(spool_root.join(format!("{blob_id}.jpg")), jpeg)?;
    Ok(format!("{:x}", Sha256::digest(jpeg)))
}
