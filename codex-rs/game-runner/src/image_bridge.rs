use std::fs::File;
use std::io;
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::ensure;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use serde_json::Value;
use serde_json::json;
use sha2::Digest;
use sha2::Sha256;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;

const MAX_JPEG_BYTES: u64 = 16 * 1024 * 1024;
const MAX_HELPER_LINE_BYTES: usize = 1024 * 1024;

/// Relays MCP over a helper socket and turns verified screenshot blobs into images.
pub async fn run_image_bridge(socket_path: &Path) -> anyhow::Result<()> {
    let stream = codex_uds::UnixStream::connect(socket_path)
        .await
        .with_context(|| format!("failed to connect to socket at {}", socket_path.display()))?;
    let spool_root = socket_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("screenshot-spool");
    let (socket_reader, mut socket_writer) = tokio::io::split(stream);

    let copy_stdin_to_socket = async {
        let mut stdin = tokio::io::stdin();
        tokio::io::copy(&mut stdin, &mut socket_writer)
            .await
            .context("failed to copy MCP requests to the game helper")?;
        if let Err(error) = socket_writer.shutdown().await
            && error.kind() != io::ErrorKind::NotConnected
        {
            return Err(error).context("failed to close the game helper request stream");
        }
        anyhow::Ok(())
    };
    let copy_socket_to_stdout = async {
        let mut reader = BufReader::new(socket_reader);
        let mut stdout = tokio::io::stdout();
        let mut line = Vec::new();
        loop {
            line.clear();
            let count = reader
                .read_until(b'\n', &mut line)
                .await
                .context("failed to read an MCP response from the game helper")?;
            if count == 0 {
                break;
            }
            ensure!(
                line.len() <= MAX_HELPER_LINE_BYTES,
                "game helper MCP response exceeded {MAX_HELPER_LINE_BYTES} bytes"
            );
            let mut response = serde_json::from_slice::<Value>(&line)
                .context("game helper returned malformed MCP JSON")?;
            adopt_spooled_image(&mut response, &spool_root)?;
            let mut encoded = serde_json::to_vec(&response).context("encode game MCP response")?;
            encoded.push(b'\n');
            stdout
                .write_all(&encoded)
                .await
                .context("write game MCP response")?;
            stdout.flush().await.context("flush game MCP response")?;
        }
        anyhow::Ok(())
    };

    tokio::try_join!(copy_stdin_to_socket, copy_socket_to_stdout)?;
    Ok(())
}

fn adopt_spooled_image(response: &mut Value, spool_root: &Path) -> anyhow::Result<()> {
    let Some(metadata) = response
        .pointer("/result/structuredContent")
        .and_then(Value::as_object)
        .cloned()
    else {
        return Ok(());
    };
    let Some(blob_id) = metadata.get("image_blob_id").and_then(Value::as_str) else {
        return Ok(());
    };
    let blob_id = blob_id.to_string();
    let parsed_id =
        uuid::Uuid::parse_str(&blob_id).context("invalid screenshot blob identifier")?;
    ensure!(
        parsed_id.hyphenated().to_string() == blob_id,
        "screenshot blob identifier is not canonical lowercase UUID"
    );
    let expected_bytes = metadata
        .get("image_bytes")
        .and_then(Value::as_u64)
        .context("screenshot metadata omitted image_bytes")?;
    ensure!(
        expected_bytes <= MAX_JPEG_BYTES,
        "screenshot metadata exceeded the image size limit"
    );
    ensure!(
        metadata.get("mime_type").and_then(Value::as_str) == Some("image/jpeg"),
        "screenshot metadata did not identify a JPEG"
    );
    let expected_sha256 = metadata
        .get("sha256")
        .and_then(Value::as_str)
        .context("screenshot metadata omitted sha256")?
        .to_string();
    ensure!(
        expected_sha256.len() == 64
            && expected_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "screenshot metadata contained an invalid sha256"
    );

    let source = spool_root.join(format!("{blob_id}.jpg"));
    let claim = spool_root.join(format!(".{blob_id}.{}.claim", uuid::Uuid::new_v4()));
    std::fs::rename(&source, &claim).context("claim screenshot blob")?;
    let claimed = ClaimedBlob(claim);
    let file_metadata = std::fs::symlink_metadata(&claimed.0).context("inspect screenshot blob")?;
    ensure!(
        file_metadata.is_file(),
        "screenshot blob is not a regular file"
    );
    ensure!(
        file_metadata.len() == expected_bytes,
        "screenshot blob size did not match its metadata"
    );
    let mut jpeg = Vec::with_capacity(expected_bytes as usize);
    File::open(&claimed.0)
        .context("open screenshot blob")?
        .take(MAX_JPEG_BYTES + 1)
        .read_to_end(&mut jpeg)
        .context("read screenshot blob")?;
    ensure!(
        jpeg.len() as u64 == expected_bytes,
        "screenshot blob changed while being read"
    );
    ensure!(
        jpeg.starts_with(&[0xff, 0xd8]),
        "screenshot blob is not a JPEG"
    );
    let actual_sha256 = format!("{:x}", Sha256::digest(&jpeg));
    ensure!(
        actual_sha256 == expected_sha256,
        "screenshot blob sha256 did not match its metadata"
    );

    let mut public_metadata = metadata;
    public_metadata.remove("image_blob_id");
    public_metadata.insert(
        "artifact_uri".to_string(),
        Value::String(format!("sha256:{actual_sha256}")),
    );
    let text = serde_json::to_string(&public_metadata).context("encode screenshot metadata")?;
    let result = response
        .get_mut("result")
        .and_then(Value::as_object_mut)
        .context("screenshot response omitted its MCP result")?;
    result.insert(
        "content".to_string(),
        json!([
            {"type": "text", "text": text},
            {
                "type": "image",
                "data": BASE64_STANDARD.encode(jpeg),
                "mimeType": "image/jpeg"
            }
        ]),
    );
    result.insert(
        "structuredContent".to_string(),
        Value::Object(public_metadata),
    );
    Ok(())
}

struct ClaimedBlob(PathBuf);

impl Drop for ClaimedBlob {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(test)]
#[path = "image_bridge_tests.rs"]
mod tests;
