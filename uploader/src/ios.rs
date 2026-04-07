pub mod progress;
pub mod api;

use std::os::raw::{c_char, c_ulonglong};
use std::ffi::{CStr, CString};
use std::io::{BufReader, Read, Seek, SeekFrom, Cursor};
use std::fs::File;
use std::sync::{Arc, Mutex};
use std::thread;
use std::sync::Once;

use crate::core::{
    ChunkUploadResult,
    MAX_CONCURRENT_UPLOADS, MAX_RETRIES,
    get_urls_endpoint,
    complete_upload_endpoint,
    clean_etag
};
use crate::core::api::{CompleteRequest, UploadUrlsResponse};
use crate::core::chunk::{self, ChunkInfo};
use crate::core::progress::{ProgressNotifier, UploadStatus};
use crate::core::retry::{self, RetryPolicy};
use crate::ios::progress::{self as iosProgress, ProgressReader};

static INIT: Once = Once::new();

fn init_nyquest() {
    INIT.call_once(|| {
        nyquest_backend_nsurlsession::register();
    });
}

#[no_mangle]
pub extern "C" fn add(one: i32, two: i32) -> i32 {
    one + two
}

#[no_mangle]
pub extern "C" fn upload_file(path: *const c_char) -> i32 {
    if path.is_null() {
        return -1;
    }

    let c_str = unsafe { CStr::from_ptr(path) };
    let file_path = match c_str.to_str() {
        Ok(s) => s.to_string(),
        Err(_) => return -1,
    };

    match upload_file_internal(&file_path) {
        Ok(_) => 0,
        Err(_) => -1,
    }
}

fn upload_file_internal(file_path: &str) -> Result<(), Box<dyn std::error::Error>> {
    init_nyquest();

    let file = File::open(file_path)?;
    let file_size = file.metadata()?.len();

    // HTTP Client erstellen
    let client = nyquest::ClientBuilder::default()
        .build_blocking()
        .map_err(|e| format!("Failed to create client: {:?}", e))?;

    // Upload URLs abrufen
    let upload_info = api::fetch_upload_urls(&client, file_size)?;

    // Progress initialisieren
    iosProgress::update_progress(|m| m.init(file_size, upload_info.chunk_count()));

    // Chunk-Infos generieren
    let chunk_infos = chunk::generate_chunk_infos(file_size, &upload_info)
        .map_err(|e| format!("Failed to generate chunk infos: {}", e))?;

    // Chunks parallel hochladen
    let completed_parts = upload_chunks_parallel(file_path, &chunk_infos)
        .map_err(|e| {
            iosProgress::update_progress(|m| m.set_status(UploadStatus::Failed));
            e
        })?;

    // Upload abschließen
    api::complete_upload(&client, &upload_info, completed_parts)
        .map_err(|e| {
            iosProgress::update_progress(|m| m.set_status(UploadStatus::Failed));
            e
        })?;

    iosProgress::update_progress(|m| m.set_status(UploadStatus::Finished));
    Ok(())
}

fn upload_chunks_parallel(
    file_path: &str,
    chunk_infos: &[ChunkInfo],
) -> Result<Vec<ChunkUploadResult>, Box<dyn std::error::Error>> {
    let completed_parts = Arc::new(Mutex::new(Vec::new()));
    let file_path_arc = Arc::new(file_path.to_string());
    let chunk_infos_arc = Arc::new(chunk_infos.to_vec());

    // Channel für Worker-Threads
    let (tx, rx) = std::sync::mpsc::sync_channel::<usize>(MAX_CONCURRENT_UPLOADS);
    let rx = Arc::new(Mutex::new(rx));

    // Worker Threads erstellen
    let mut handles = vec![];
    for _ in 0..MAX_CONCURRENT_UPLOADS {
        let rx = rx.clone();
        let file_path = file_path_arc.clone();
        let chunk_infos = chunk_infos_arc.clone();
        let completed_parts = completed_parts.clone();

        let handle = thread::spawn(move || {
            let client = nyquest::ClientBuilder::default()
                .build_blocking()
                .expect("Failed to create client in worker thread");

            loop {
                let work = {
                    let receiver = rx.lock().unwrap();
                    receiver.recv()
                };

                let chunk_index = match work {
                    Ok(i) => i,
                    Err(_) => break, // Channel closed
                };

                let chunk_info = &chunk_infos[chunk_index];

                // Chunk aus Datei lesen
                let chunk = match read_chunk_from_file(&file_path, &chunk_info) {
                    Ok(data) => data,
                    Err(e) => {
                        eprintln!("Failed to read chunk {}: {:?}", chunk_info.part_number, e);
                        continue;
                    }
                };

                // Upload mit Retry und Progress-Tracking
                let etag = match upload_chunk_with_retry(&client, &chunk_info.url, &chunk, chunk_info.part_number) {
                    Ok(tag) => tag,
                    Err(e) => {
                        eprintln!("Failed to upload part {}: {:?}", chunk_info.part_number, e);
                        continue;
                    }
                };

                // Completed part speichern
                {
                    let mut parts = completed_parts.lock().unwrap();
                    parts.push(ChunkUploadResult {
                        part_number: chunk_info.part_number,
                        etag
                    });
                }
            }
        });

        handles.push(handle);
    }

    // Chunk-Indizes an Worker senden
    for i in 0..chunk_infos.len() {
        tx.send(i).ok();
    }
    drop(tx); // Sender closen

    // Auf alle Worker warten
    for handle in handles {
        handle.join().ok();
    }

    let parts = completed_parts.lock().unwrap().clone();
    Ok(parts)
}

fn upload_chunk_with_retry(
    client: &nyquest::BlockingClient,
    url: &str,
    data: &[u8],
    part_number: u32,
) -> Result<String, Box<dyn std::error::Error>> {
    let chunk_size = data.len() as u64;
    let policy = RetryPolicy::new(MAX_RETRIES);

    let result = retry::run_with_retry_string(
        &policy,
        |_attempt| {
            // Reset in-flight progress für diesen Chunk bei jedem Versuch
            iosProgress::update_progress(|m| m.update_in_flight(part_number, 0));

            // ProgressReader für diesen Chunk erstellen
            let progress_reader = ProgressReader::new(data.to_vec(), part_number);

            // Body::stream mit dem ProgressReader verwenden
            let body = nyquest::blocking::Body::stream(progress_reader, "application/octet-stream", chunk_size);
            let request = nyquest::Request::put(url.to_string()).with_body(body);

            match client.request(request) {
                Ok(response) => {
                    // ETag aus Response Headers extrahieren
                    match response.get_header("etag") {
                        Ok(etag_vec) if !etag_vec.is_empty() => {
                            // Chunk erst nach erfolgreich extrahiertem ETag als abgeschlossen markieren
                            iosProgress::update_progress(|m| m.complete_chunk(part_number, chunk_size));
                            Ok(clean_etag(&etag_vec[0]))
                        }
                        _ => Err("No ETag in response".to_string()),
                    }
                }
                Err(e) => Err(format!("{:?}", e)),
            }
        },
        |attempt, err, delay_ms| {
            eprintln!(
                "Upload attempt {} failed for part {}: {}, retrying in {}ms",
                attempt, part_number, err, delay_ms
            );
            thread::sleep(std::time::Duration::from_millis(delay_ms as u64));
        },
    );

    result.map_err(|e| e.into())
}

fn read_chunk_from_file(path: &str, chunk: &ChunkInfo) -> std::io::Result<Vec<u8>> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    chunk.read(&mut reader)
}