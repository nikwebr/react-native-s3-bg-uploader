use std::collections::HashMap;
use std::sync::Arc;

use futures::future::LocalBoxFuture;
use futures::stream::{FuturesUnordered, StreamExt};

use crate::core::chunk::ChunkInfo;
use crate::core::upload;
use crate::core::{ChunkUploadResult, MAX_CONCURRENT_UPLOADS};

pub trait AsyncPlatformAdapter: Send + Sync + 'static {
    fn is_paused(&self) -> bool;

    fn fetch_urls(
        &self,
        parts: Vec<u32>,
    ) -> LocalBoxFuture<'static, Result<HashMap<u32, String>, String>>;

    fn read_chunk(&self, file: &web_sys::File, chunk: &ChunkInfo) -> Result<Vec<u8>, String>;

    fn upload_chunk(
        &self,
        url: String,
        data: Vec<u8>,
        part_number: u32,
        file_key: String,
    ) -> LocalBoxFuture<'static, Result<String, String>>;
}

pub async fn run_async_upload<A>(
    adapter: Arc<A>,
    file: Arc<web_sys::File>,
    file_key: &str,
    part_size: u64,
    file_size: u64,
    parts_to_upload: Vec<u32>,
) -> Result<Vec<ChunkUploadResult>, String>
where
    A: AsyncPlatformAdapter,
{
    let first_batch: Vec<u32> = parts_to_upload
        .iter()
        .take(MAX_CONCURRENT_UPLOADS)
        .cloned()
        .collect();
    let mut url_pool = adapter.fetch_urls(first_batch).await?;
    let mut in_flight = FuturesUnordered::new();
    let mut completed_parts = Vec::new();
    let mut next_idx = 0;
    let parts_len = parts_to_upload.len();

    let push_chunk = |idx: usize,
                      url_pool: &mut HashMap<u32, String>,
                      in_flight: &mut FuturesUnordered<
        LocalBoxFuture<'static, Result<ChunkUploadResult, String>>,
    >|
     -> Result<bool, String> {
        let part_number = parts_to_upload[idx];
        let Some(url) = url_pool.remove(&part_number) else {
            return Ok(false);
        };

        let chunk_info = ChunkInfo {
            part_number,
            start_pos: (part_number as u64 - 1) * part_size,
            chunk_size: upload::part_size_for(part_number, part_size, file_size),
            url: url.clone(),
        };
        let chunk = adapter.read_chunk(&file, &chunk_info)?;
        let file_key = file_key.to_string();
        let adapter = adapter.clone();

        in_flight.push(Box::pin(async move {
            let etag = adapter
                .upload_chunk(url, chunk, part_number, file_key)
                .await?;
            Ok(ChunkUploadResult { part_number, etag })
        }));
        Ok(true)
    };

    while next_idx < parts_len && in_flight.len() < MAX_CONCURRENT_UPLOADS {
        let _ = push_chunk(next_idx, &mut url_pool, &mut in_flight)?;
        next_idx += 1;
    }

    while let Some(result) = in_flight.next().await {
        match result {
            Ok(result) => completed_parts.push(result),
            Err(error) => {
                while in_flight.next().await.is_some() {}
                if adapter.is_paused() {
                    return Err("Upload paused".to_string());
                }
                return Err(error);
            }
        }

        if adapter.is_paused() {
            while in_flight.next().await.is_some() {}
            return Err("Upload paused".to_string());
        }

        if next_idx < parts_len {
            if url_pool.len() < MAX_CONCURRENT_UPLOADS {
                let prefetch_parts: Vec<u32> = parts_to_upload
                    .iter()
                    .skip(next_idx)
                    .take(MAX_CONCURRENT_UPLOADS)
                    .cloned()
                    .collect();
                if !prefetch_parts.is_empty() {
                    url_pool.extend(adapter.fetch_urls(prefetch_parts).await.unwrap_or_default());
                }
            }

            let _ = push_chunk(next_idx, &mut url_pool, &mut in_flight)?;
            next_idx += 1;
        }
    }

    Ok(completed_parts)
}
