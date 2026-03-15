pub mod api;
pub mod progress;
pub mod retry;
pub mod chunk;

use std::io::{Read, Seek};
use serde::{Deserialize, Serialize};
use std::future::Future;

pub const MAX_CONCURRENT_UPLOADS: usize = 4;
pub const MAX_RETRIES: usize = 3;
pub const UPLOAD_BASE_URL: &str = "https://development1.ysendit.com/upload/MobileS3";

/// Ergebnis eines Chunk-Uploads
#[derive(Debug, Clone)]
pub struct ChunkUploadResult {
    pub part_number: u32,
    pub etag: String
}



/// Erstellt die URL zum Abrufen der Upload-URLs
pub fn get_urls_endpoint(file_size: u64) -> String {
    format!("{}/getUrls/{}", UPLOAD_BASE_URL, file_size)
}

/// Erstellt die URL zum Abschließen des Uploads
pub fn complete_upload_endpoint(upload_id: &str, key: &str) -> String {
    format!("{}/complete/{}/{}", UPLOAD_BASE_URL, upload_id, key)
}



/// Bereinigt ETag-Werte (entfernt umgebende Anführungszeichen)
pub fn clean_etag(etag: &str) -> String {
    etag.trim_matches('"').to_string()
}

