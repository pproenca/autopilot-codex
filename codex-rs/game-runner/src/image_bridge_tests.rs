use pretty_assertions::assert_eq;
use serde_json::json;

use super::adopt_spooled_image;

#[test]
fn adopts_verified_screenshot_as_mcp_image_content() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let blob_id = "00000000-0000-4000-8000-000000000001";
    let jpeg = [0xff, 0xd8, 0xff, 0xd9];
    std::fs::write(temp.path().join(format!("{blob_id}.jpg")), jpeg)?;
    let metadata = json!({
        "app": "Gambonanza",
        "image_blob_id": blob_id,
        "image_bytes": jpeg.len(),
        "mime_type": "image/jpeg",
        "sha256": "32461d5bd1773012acef0ba15636752949bd7c2ce50f9172159d9f56cf0dd9af",
        "width": 2,
        "height": 2,
    });
    let mut response = json!({
        "jsonrpc": "2.0",
        "id": 7,
        "result": {
            "content": [{"type": "text", "text": serde_json::to_string(&metadata)?}],
            "structuredContent": metadata,
            "isError": false,
        },
    });

    adopt_spooled_image(&mut response, temp.path())?;

    assert_eq!(
        response,
        json!({
            "jsonrpc": "2.0",
            "id": 7,
            "result": {
                "content": [
                    {
                        "type": "text",
                        "text": "{\"app\":\"Gambonanza\",\"artifact_uri\":\"sha256:32461d5bd1773012acef0ba15636752949bd7c2ce50f9172159d9f56cf0dd9af\",\"height\":2,\"image_bytes\":4,\"mime_type\":\"image/jpeg\",\"sha256\":\"32461d5bd1773012acef0ba15636752949bd7c2ce50f9172159d9f56cf0dd9af\",\"width\":2}"
                    },
                    {
                        "type": "image",
                        "data": "/9j/2Q==",
                        "mimeType": "image/jpeg"
                    }
                ],
                "structuredContent": {
                    "app": "Gambonanza",
                    "artifact_uri": "sha256:32461d5bd1773012acef0ba15636752949bd7c2ce50f9172159d9f56cf0dd9af",
                    "height": 2,
                    "image_bytes": 4,
                    "mime_type": "image/jpeg",
                    "sha256": "32461d5bd1773012acef0ba15636752949bd7c2ce50f9172159d9f56cf0dd9af",
                    "width": 2
                },
                "isError": false
            }
        })
    );
    assert!(!temp.path().join(format!("{blob_id}.jpg")).exists());
    Ok(())
}

#[test]
fn rejects_and_consumes_blob_with_mismatched_digest() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let blob_id = "00000000-0000-4000-8000-000000000001";
    let blob_path = temp.path().join(format!("{blob_id}.jpg"));
    std::fs::write(&blob_path, [0xff, 0xd8, 0xff, 0xd9])?;
    let mut response = json!({
        "jsonrpc": "2.0",
        "id": 7,
        "result": {
            "content": [],
            "structuredContent": {
                "image_blob_id": blob_id,
                "image_bytes": 4,
                "mime_type": "image/jpeg",
                "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            },
            "isError": false,
        },
    });

    let error = adopt_spooled_image(&mut response, temp.path()).expect_err("digest must fail");

    assert_eq!(
        error.to_string(),
        "screenshot blob sha256 did not match its metadata"
    );
    assert!(!blob_path.exists());
    Ok(())
}
