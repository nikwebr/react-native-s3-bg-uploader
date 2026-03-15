use std::collections::HashMap;
use serde::{Deserialize, Serialize};

use crate::core::ChunkUploadResult;

#[derive(Debug, Serialize, Deserialize)]
pub struct UploadUrlsResponse {
    pub key: String,
    #[serde(rename = "uploadId")]
    pub upload_id: String,
    #[serde(rename = "partSize")]
    pub part_size: u64,
    pub urls: HashMap<String, String>,
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

impl UploadUrlsResponse {
    pub fn chunk_count(&self) -> u32 {
        self.urls.len() as u32
    }
}

impl CompleteRequest {
    pub fn serialize(&self) -> Result<String, String> {
        serde_json::to_string(&self).map_err(|e| format!("Failed to serialize: {}", e))
    }

    pub fn from_upload_results(results: Vec<ChunkUploadResult>) -> Self {
        let mut parts: Vec<CompletePart> = results
            .into_iter()
            .map(|r| r.to_complete_part())
            .collect();
        parts.sort_by_key(|p| p.part_number);

        CompleteRequest { parts }
    }
}

impl ChunkUploadResult {
    fn to_complete_part(&self) -> CompletePart {
        CompletePart {
            etag: self.etag.clone(),
            part_number: self.part_number,
        }
    }
}

