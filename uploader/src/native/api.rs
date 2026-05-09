use async_trait::async_trait;
use std::collections::HashMap;

use crate::core::api::{
    complete_upload_body, complete_upload_url, start_upload_body, upload_urls_batch_body,
    ApiClient, StartUploadResponse, UploadUrlsBatchResponse,
};
use crate::core::config::get_config;
use crate::core::retry::{run_with_retry_string, RetryPolicy};
use crate::core::ChunkUploadResult;

pub trait BlockingNetwork {
    fn post_json(&self, url: &str, body_json: String)
        -> Result<String, Box<dyn std::error::Error>>;
}

pub struct NativeApiClient<N: BlockingNetwork> {
    pub network: N,
}

#[async_trait(?Send)]
impl<N: BlockingNetwork> ApiClient for NativeApiClient<N> {
    async fn start_upload(
        &self,
        file_name: &str,
        file_hash: &str,
        file_size: u64,
        user_params: &HashMap<String, String>,
    ) -> Result<StartUploadResponse, String> {
        let url = get_config().start_upload_api.clone();
        let body = start_upload_body(file_name, file_hash, file_size, user_params)?;
        let text = self
            .network
            .post_json(&url, body)
            .map_err(|e| format!("start_upload request failed: {}", e))?;
        serde_json::from_str(&text)
            .map_err(|e| format!("start_upload parse failed: {:?} body={}", e, text))
    }

    async fn fetch_upload_urls_batch(
        &self,
        key: &str,
        upload_id: &str,
        part_numbers: &[u32],
    ) -> Result<HashMap<u32, String>, String> {
        let url = get_config().get_upload_urls_api.clone();
        let body = upload_urls_batch_body(key, upload_id, part_numbers)?;
        let text = self
            .network
            .post_json(&url, body)
            .map_err(|e| format!("fetch_upload_urls_batch request failed: {}", e))?;
        let parsed: UploadUrlsBatchResponse = serde_json::from_str(&text).map_err(|e| {
            format!(
                "fetch_upload_urls_batch parse failed: {:?} body={}",
                e, text
            )
        })?;
        Ok(parsed.into_part_map())
    }

    async fn complete_upload(
        &self,
        key: &str,
        upload_id: &str,
        results: Vec<ChunkUploadResult>,
    ) -> Result<(), String> {
        let base_url = get_config().complete_api.clone();
        let url = complete_upload_url(&base_url, upload_id, key);
        let body = complete_upload_body(results)?;
        let policy = RetryPolicy::new(3);
        run_with_retry_string(
            &policy,
            |_attempt| {
                self.network
                    .post_json(&url, body.clone())
                    .map(|_| ())
                    .map_err(|e| format!("complete_upload request failed: {}", e))
            },
            |_attempt, err, delay_ms| {
                if is_retryable_complete_error(err) {
                    std::thread::sleep(std::time::Duration::from_millis(delay_ms as u64));
                }
            },
        )?;
        Ok(())
    }
}

fn is_retryable_complete_error(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("request is not finished within timeout")
        || lower.contains("network connection was lost")
        || lower.contains("nsurlerrordomain code=-1001")
        || lower.contains("nsurlerrordomain code=-1005")
}
