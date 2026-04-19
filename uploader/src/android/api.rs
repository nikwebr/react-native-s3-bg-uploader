use std::collections::HashMap;
use crate::core::api::{self, StartUploadResponse};
use crate::core::ChunkUploadResult;

pub fn start_upload_android(
    client: &reqwest::blocking::Client,
    file_name: &str,
    file_hash: &str,
    file_size: u64,
    user_params: &HashMap<String, String>,
) -> Result<StartUploadResponse, Box<dyn std::error::Error>> {
    api::start_upload_android(client, file_name, file_hash, file_size, user_params)
}

pub fn fetch_upload_urls_batch_android(
    client: &reqwest::blocking::Client,
    file_key: &str,
    upload_id: &str,
    part_numbers: &[u32],
) -> Result<HashMap<u32, String>, Box<dyn std::error::Error>> {
    api::fetch_upload_urls_batch_android(client, file_key, upload_id, part_numbers)
}

pub fn complete_upload_android(
    client: &reqwest::blocking::Client,
    file_key: &str,
    upload_id: &str,
    results: Vec<ChunkUploadResult>,
) -> Result<(), Box<dyn std::error::Error>> {
    api::complete_upload_android(client, file_key, upload_id, results)
}
