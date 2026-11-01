use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::core::ChunkUploadResult;
// ---------------------------------------------------------------------------
// Request / response types
// ---------------------------------------------------------------------------

/// Response from startUploadApi. Contains the S3 key and multipart upload ID
/// that must be used for all subsequent presign and complete calls.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartUploadResponse {
    pub key: String,
    #[serde(rename = "uploadId")]
    pub upload_id: String,
    #[serde(rename = "partSize")]
    pub part_size: u64,
}

/// Response from getUploadUrlsApi for a batch of parts.
/// Maps part_number (as string key in JSON) → presigned PUT URL.
#[derive(Debug, Serialize, Deserialize)]
pub struct UploadUrlsBatchResponse {
    pub urls: HashMap<String, String>,
}

impl UploadUrlsBatchResponse {
    /// Convert string-keyed map to u32-keyed map.
    pub fn into_part_map(self) -> HashMap<u32, String> {
        self.urls
            .into_iter()
            .filter_map(|(k, v)| k.parse::<u32>().ok().map(|n| (n, v)))
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompleteRequest {
    pub parts: Vec<CompletePart>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CompletePart {
    #[serde(rename = "ETag")]
    pub etag: String,
    #[serde(rename = "PartNumber")]
    pub part_number: u32,
}

impl CompleteRequest {
    pub fn serialize(&self) -> Result<String, String> {
        serde_json::to_string(&self).map_err(|e| format!("Failed to serialize: {}", e))
    }

    pub fn from_upload_results(results: Vec<ChunkUploadResult>) -> Self {
        let mut parts: Vec<CompletePart> =
            results.into_iter().map(|r| r.to_complete_part()).collect();
        parts.sort_by_key(|p| p.part_number);
        CompleteRequest { parts }
    }
}

/// Build the startUpload request body.
///
/// `file_size` is the size of the object that will be stored (the ciphertext size
/// for encrypted uploads), not the plaintext size.
/// `send_file_hash` omits `fileHash` from the body while the hash is still
/// computed and kept locally for dedup/resume.
/// Reserved keys (`fileName`, `fileHash`, `fileSize`) always win over
/// `user_params`, so a caller cannot accidentally send a plaintext `fileSize`.
pub fn start_upload_body(
    file_name: &str,
    file_hash: &str,
    file_size: u64,
    send_file_hash: bool,
    user_params: &HashMap<String, String>,
) -> Result<String, String> {
    let mut body_map: serde_json::Map<String, serde_json::Value> = user_params
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
        .collect();
    body_map.remove("fileName");
    body_map.remove("fileHash");
    body_map.remove("fileSize");
    body_map.insert(
        "fileName".to_string(),
        serde_json::Value::String(file_name.to_string()),
    );
    if send_file_hash {
        body_map.insert(
            "fileHash".to_string(),
            serde_json::Value::String(file_hash.to_string()),
        );
    }
    body_map.insert(
        "fileSize".to_string(),
        serde_json::Value::Number(file_size.into()),
    );
    serde_json::to_string(&body_map)
        .map_err(|e| format!("Failed to serialize start_upload body: {}", e))
}

pub fn upload_urls_batch_body(
    key: &str,
    upload_id: &str,
    part_numbers: &[u32],
) -> Result<String, String> {
    serde_json::to_string(&serde_json::json!({
        "key": key,
        "uploadId": upload_id,
        "parts": part_numbers
    }))
    .map_err(|e| format!("Failed to serialize batch url body: {}", e))
}

pub fn complete_upload_body(results: Vec<ChunkUploadResult>) -> Result<String, String> {
    CompleteRequest::from_upload_results(results).serialize()
}

pub fn complete_upload_url(base_url: &str, upload_id: &str, key: &str) -> String {
    format!("{}/{}/{}", base_url, upload_id, key)
}

impl ChunkUploadResult {
    fn to_complete_part(&self) -> CompletePart {
        CompletePart {
            etag: self.etag.clone(),
            part_number: self.part_number,
        }
    }
}

#[async_trait(?Send)]
pub trait ApiClient {
    /// `file_size` is the wire size of the object (ciphertext size for encrypted
    /// uploads). Implementations decide whether to include `fileHash` from config.
    async fn start_upload(
        &self,
        file_name: &str,
        file_hash: &str,
        file_size: u64,
        user_params: &HashMap<String, String>,
    ) -> Result<StartUploadResponse, String>;

    async fn fetch_upload_urls_batch(
        &self,
        key: &str,
        upload_id: &str,
        part_numbers: &[u32],
    ) -> Result<HashMap<u32, String>, String>;

    async fn complete_upload(
        &self,
        key: &str,
        upload_id: &str,
        results: Vec<ChunkUploadResult>,
    ) -> Result<(), String>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_upload_body_sends_file_hash_by_default() {
        let body = start_upload_body("a.bin", "hash-1", 1234, true, &HashMap::new()).unwrap();
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["fileName"], "a.bin");
        assert_eq!(json["fileHash"], "hash-1");
        assert_eq!(json["fileSize"], 1234);
    }

    #[test]
    fn start_upload_body_can_omit_the_file_hash() {
        let body = start_upload_body("a.bin", "hash-1", 1234, false, &HashMap::new()).unwrap();
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["fileName"], "a.bin");
        assert!(
            json.get("fileHash").is_none(),
            "fileHash must not be sent when disabled: {body}"
        );
        assert_eq!(json["fileSize"], 1234);
    }

    #[test]
    fn start_upload_body_does_not_let_user_params_override_sizes() {
        let mut params = HashMap::new();
        params.insert("fileSize".to_string(), "1".to_string());
        params.insert("fileHash".to_string(), "wrong".to_string());
        params.insert("userId".to_string(), "42".to_string());
        let body = start_upload_body("a.bin", "right", 999, true, &params).unwrap();
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["fileHash"], "right");
        assert_eq!(json["fileSize"], 999);
        assert_eq!(json["userId"], "42");
    }

    #[test]
    fn start_upload_body_keeps_file_hash_in_user_params_out_when_disabled() {
        let mut params = HashMap::new();
        params.insert("fileHash".to_string(), "wrong".to_string());
        let body = start_upload_body("a.bin", "right", 999, false, &params).unwrap();
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(json.get("fileHash").is_none());
    }
}
