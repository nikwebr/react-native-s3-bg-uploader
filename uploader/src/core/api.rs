use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[cfg(any(feature = "ios", feature = "android"))]
use crate::core::config::get_config;
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

pub fn start_upload_body(
    file_name: &str,
    file_hash: &str,
    file_size: u64,
    user_params: &HashMap<String, String>,
) -> Result<String, String> {
    let mut body_map = user_params.clone();
    body_map.insert("fileName".to_string(), file_name.to_string());
    body_map.insert("fileHash".to_string(), file_hash.to_string());
    body_map.insert("fileSize".to_string(), file_size.to_string());
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

// ---------------------------------------------------------------------------
// iOS / blocking HTTP helpers (compiled only for the `ios` feature)
// ---------------------------------------------------------------------------

#[cfg(feature = "ios")]
pub fn start_upload(
    client: &nyquest::BlockingClient,
    file_name: &str,
    file_hash: &str,
    file_size: u64,
    user_params: &HashMap<String, String>,
) -> Result<StartUploadResponse, Box<dyn std::error::Error>> {
    let url = get_config().start_upload_api.clone();
    let body_json = start_upload_body(file_name, file_hash, file_size, user_params)?;

    let body = nyquest::Body::bytes(body_json.into_bytes(), "application/json");
    let request = nyquest::Request::post(url).with_body(body);
    let response = client
        .request(request)
        .map_err(|e| format!("start_upload request failed: {:?}", e))?;
    let text = response
        .text()
        .map_err(|e| format!("start_upload read body failed: {:?}", e))?;
    serde_json::from_str(&text)
        .map_err(|e| format!("start_upload parse failed: {:?} body={}", e, text).into())
}

#[cfg(feature = "ios")]
pub fn fetch_upload_urls_batch(
    client: &nyquest::BlockingClient,
    key: &str,
    upload_id: &str,
    part_numbers: &[u32],
) -> Result<HashMap<u32, String>, Box<dyn std::error::Error>> {
    let url = get_config().get_upload_urls_api.clone();
    let body_json = upload_urls_batch_body(key, upload_id, part_numbers)?;

    let body = nyquest::Body::bytes(body_json.into_bytes(), "application/json");
    let request = nyquest::Request::post(url).with_body(body);
    let response = client
        .request(request)
        .map_err(|e| format!("fetch_upload_urls_batch request failed: {:?}", e))?;
    let text = response
        .text()
        .map_err(|e| format!("fetch_upload_urls_batch read body failed: {:?}", e))?;
    let parsed: UploadUrlsBatchResponse = serde_json::from_str(&text).map_err(|e| {
        format!(
            "fetch_upload_urls_batch parse failed: {:?} body={}",
            e, text
        )
    })?;
    Ok(parsed.into_part_map())
}

#[cfg(feature = "ios")]
pub fn complete_upload(
    client: &nyquest::BlockingClient,
    key: &str,
    upload_id: &str,
    results: Vec<ChunkUploadResult>,
) -> Result<(), Box<dyn std::error::Error>> {
    let url = get_config().complete_api.clone();
    let body_json = complete_upload_body(results)?;
    let body = nyquest::Body::bytes(body_json.into_bytes(), "application/json");
    let request = nyquest::Request::post(complete_upload_url(&url, upload_id, key)).with_body(body);
    client
        .request(request)
        .map_err(|e| format!("complete_upload request failed: {:?}", e))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Android / reqwest blocking helpers
// ---------------------------------------------------------------------------

#[cfg(feature = "android")]
pub fn start_upload_android(
    client: &reqwest::blocking::Client,
    file_name: &str,
    file_hash: &str,
    file_size: u64,
    user_params: &HashMap<String, String>,
) -> Result<StartUploadResponse, Box<dyn std::error::Error>> {
    let url = get_config().start_upload_api.clone();
    let response = client
        .post(&url)
        .body(start_upload_body(
            file_name,
            file_hash,
            file_size,
            user_params,
        )?)
        .header("Content-Type", "application/json")
        .send()
        .map_err(|e| format!("start_upload request failed: {:?}", e))?;
    let text = response
        .text()
        .map_err(|e| format!("start_upload read body failed: {:?}", e))?;
    serde_json::from_str(&text)
        .map_err(|e| format!("start_upload parse failed: {:?} body={}", e, text).into())
}

#[cfg(feature = "android")]
pub fn fetch_upload_urls_batch_android(
    client: &reqwest::blocking::Client,
    key: &str,
    upload_id: &str,
    part_numbers: &[u32],
) -> Result<HashMap<u32, String>, Box<dyn std::error::Error>> {
    let url = get_config().get_upload_urls_api.clone();
    let response = client
        .post(&url)
        .body(upload_urls_batch_body(key, upload_id, part_numbers)?)
        .header("Content-Type", "application/json")
        .send()
        .map_err(|e| format!("fetch_upload_urls_batch request failed: {:?}", e))?;
    let text = response
        .text()
        .map_err(|e| format!("fetch_upload_urls_batch read body failed: {:?}", e))?;
    let parsed: UploadUrlsBatchResponse = serde_json::from_str(&text).map_err(|e| {
        format!(
            "fetch_upload_urls_batch parse failed: {:?} body={}",
            e, text
        )
    })?;
    Ok(parsed.into_part_map())
}

#[cfg(feature = "android")]
pub fn complete_upload_android(
    client: &reqwest::blocking::Client,
    key: &str,
    upload_id: &str,
    results: Vec<ChunkUploadResult>,
) -> Result<(), Box<dyn std::error::Error>> {
    let url = get_config().complete_api.clone();
    let body_json = complete_upload_body(results)?;
    client
        .post(complete_upload_url(&url, upload_id, key))
        .header("Content-Type", "application/json")
        .body(body_json)
        .send()
        .map_err(|e| format!("complete_upload request failed: {:?}", e))?;
    Ok(())
}
