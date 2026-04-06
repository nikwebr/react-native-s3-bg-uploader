pub mod progress;
pub mod api;

use std::fs::File;
use std::io::BufReader;
use std::os::unix::io::{FromRawFd, RawFd};
use std::sync::{Arc, Mutex};
use std::thread;

use jni::JNIEnv;
use jni::objects::{JClass, JObject};
use jni::sys::{jint, jlong};

use crate::core::{
    ChunkUploadResult,
    MAX_CONCURRENT_UPLOADS, MAX_RETRIES,
    clean_etag,
};
use crate::core::chunk::{self, ChunkInfo};
use crate::core::progress::UploadStatus;
use crate::core::retry::{self, RetryPolicy};
use crate::android::progress::{self as androidProgress, ProgressReader};

fn init_logging() {
    let _ = android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Debug)
            .with_tag("S3Uploader"),
    );
}

/// Dupliziert einen File Descriptor damit File::from_raw_fd Ownership übernehmen kann
/// ohne den Original-fd zu schließen.
fn dup_fd(fd: RawFd) -> std::io::Result<RawFd> {
    let new_fd = unsafe { libc::dup(fd) };
    if new_fd == -1 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(new_fd)
    }
}

/// JNI entry point: Kotlin öffnet die Datei (auch content://) via ContentResolver
/// und übergibt den File Descriptor — kein Kopieren nötig.
#[no_mangle]
pub extern "system" fn Java_com_s3bguploader_HybridS3BgUploader_nativeUploadFileStatic(
    env: JNIEnv,
    _class: JClass,
    fd: jint,
    callback: JObject,
) -> jint {
    init_logging();
    log::debug!("nativeUploadFile called with fd={}", fd);

    let jvm = match env.get_java_vm() {
        Ok(jvm) => jvm,
        Err(e) => { log::error!("get_java_vm failed: {:?}", e); return -1; }
    };
    let callback_global = match env.new_global_ref(callback) {
        Ok(g) => g,
        Err(e) => { log::error!("new_global_ref failed: {:?}", e); return -1; }
    };

    androidProgress::set_progress_callback(Some(Box::new(move |total, uploaded, completed_parts, total_parts, pct, state| {
        // get_env() falls der Thread schon attached ist (kein erneutes Attach/Detach),
        // sonst permanent attachen — bleibt bis Thread-Ende, kein Detach pro Aufruf.
        let mut env = match jvm.get_env() {
            Ok(e) => e,
            Err(_) => match jvm.attach_current_thread_permanently() {
                Ok(e) => e,
                Err(e) => { log::error!("attach_current_thread failed: {:?}", e); return; }
            },
        };
        let state_jstr = match env.new_string(state) {
            Ok(s) => s,
            Err(e) => { log::error!("new_string failed: {:?}", e); return; }
        };
        if let Err(e) = env.call_method(
            &callback_global,
            "onProgress",
            "(JJIIDLjava/lang/String;)V",
            &[
                jni::objects::JValueGen::Long(total as jlong),
                jni::objects::JValueGen::Long(uploaded as jlong),
                jni::objects::JValueGen::Int(completed_parts as jint),
                jni::objects::JValueGen::Int(total_parts as jint),
                jni::objects::JValueGen::Double(pct),
                jni::objects::JValueGen::Object(&state_jstr),
            ],
        ) {
            log::error!("call_method onProgress failed: {:?}", e);
        }
    })));

    androidProgress::update_progress(|m| m.init(0, 0));

    let raw_fd = fd as RawFd;
    let result = upload_file_internal(raw_fd);

    // Original fd schließen
    unsafe { libc::close(raw_fd); }

    match result {
        Ok(_) => 0,
        Err(e) => {
            log::error!("upload_file_internal failed: {}", e);
            androidProgress::update_progress(|m| m.set_status(UploadStatus::Failed));
            -1
        }
    }
}

fn build_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        // without that android uses a lower default
        .timeout(std::time::Duration::from_secs(300))
        .connect_timeout(std::time::Duration::from_secs(30))
        .tcp_keepalive(std::time::Duration::from_secs(30))
        .build()
        .expect("Failed to build HTTP client")
}

fn upload_file_internal(raw_fd: RawFd) -> Result<(), Box<dyn std::error::Error>> {
    // Dateigröße via dup'd fd ermitteln
    let file = unsafe { File::from_raw_fd(dup_fd(raw_fd)?) };
    let file_size = file.metadata()?.len();
    log::debug!("File size: {} bytes", file_size);

    androidProgress::update_progress(|m| m.init(file_size, 0));

    let client = build_client();

    log::debug!("Fetching upload URLs...");
    let upload_info = api::fetch_upload_urls(&client, file_size)
        .map_err(|e| format!("fetch_upload_urls failed: {}", e))?;
    log::debug!("Got {} parts", upload_info.chunk_count());

    androidProgress::update_progress(|m| m.init(file_size, upload_info.chunk_count()));

    let chunk_infos = chunk::generate_chunk_infos(file_size, &upload_info)
        .map_err(|e| format!("generate_chunk_infos failed: {}", e))?;

    let completed_parts = upload_chunks_parallel(raw_fd, &chunk_infos)?;

    api::complete_upload(&client, &upload_info, completed_parts)?;

    androidProgress::update_progress(|m| m.set_status(UploadStatus::Finished));
    log::debug!("Upload finished");
    Ok(())
}

fn upload_chunks_parallel(
    raw_fd: RawFd,
    chunk_infos: &[ChunkInfo],
) -> Result<Vec<ChunkUploadResult>, Box<dyn std::error::Error>> {
    let completed_parts = Arc::new(Mutex::new(Vec::new()));
    let chunk_infos_arc = Arc::new(chunk_infos.to_vec());

    let (tx, rx) = std::sync::mpsc::sync_channel::<usize>(MAX_CONCURRENT_UPLOADS);
    let rx = Arc::new(Mutex::new(rx));

    let mut handles = vec![];
    for _ in 0..MAX_CONCURRENT_UPLOADS {
        let rx = rx.clone();
        let chunk_infos = chunk_infos_arc.clone();
        let completed_parts = completed_parts.clone();

        let handle = thread::spawn(move || {
            let client = build_client();

            loop {
                let chunk_index = match rx.lock().unwrap().recv() {
                    Ok(i) => i,
                    Err(_) => break,
                };

                let chunk_info = &chunk_infos[chunk_index];

                let chunk = match read_chunk_from_fd(raw_fd, chunk_info) {
                    Ok(data) => data,
                    Err(e) => {
                        log::error!("Failed to read chunk {}: {:?}", chunk_info.part_number, e);
                        continue;
                    }
                };

                let etag = match upload_chunk_with_retry(&client, &chunk_info.url, &chunk, chunk_info.part_number) {
                    Ok(tag) => tag,
                    Err(e) => {
                        log::error!("Failed to upload part {}: {:?}", chunk_info.part_number, e);
                        continue;
                    }
                };

                completed_parts.lock().unwrap().push(ChunkUploadResult {
                    part_number: chunk_info.part_number,
                    etag,
                });
            }
        });

        handles.push(handle);
    }

    for i in 0..chunk_infos.len() {
        tx.send(i).ok();
    }
    drop(tx);

    for handle in handles {
        handle.join().ok();
    }

    let parts = completed_parts.lock().unwrap().clone();
    if parts.len() != chunk_infos.len() {
        return Err(format!(
            "Only {}/{} parts uploaded successfully",
            parts.len(), chunk_infos.len()
        ).into());
    }
    Ok(parts)
}

fn upload_chunk_with_retry(
    client: &reqwest::blocking::Client,
    url: &str,
    data: &[u8],
    part_number: u32,
) -> Result<String, Box<dyn std::error::Error>> {
    let chunk_size = data.len() as u64;
    let policy = RetryPolicy::new(MAX_RETRIES);

    retry::run_with_retry_string(
        &policy,
        |_attempt| {
            let progress_reader = ProgressReader::new(data.to_vec(), part_number);
            let response = client
                .put(url)
                .header("Content-Length", chunk_size.to_string())
                .body(reqwest::blocking::Body::new(progress_reader))
                .send()
                .map_err(|e| format!("{:?}", e))?;

            let etag = response
                .headers()
                .get("etag")
                .and_then(|v| v.to_str().ok())
                .map(clean_etag)
                .ok_or_else(|| "No ETag in response".to_string())?;

            androidProgress::update_progress(|m| m.complete_chunk(part_number, chunk_size));

            Ok(etag)
        },
        |attempt, err, delay_ms| {
            log::warn!("Upload attempt {} failed for part {}: {}, retrying in {}ms", attempt, part_number, err, delay_ms);
            thread::sleep(std::time::Duration::from_millis(delay_ms as u64));
        },
    ).map_err(|e| e.into())
}

/// Liest einen Chunk aus dem File Descriptor (dup für unabhängiges Seeking).
fn read_chunk_from_fd(raw_fd: RawFd, chunk: &ChunkInfo) -> std::io::Result<Vec<u8>> {
    let file = unsafe { File::from_raw_fd(dup_fd(raw_fd)?) };
    let mut reader = BufReader::new(file);
    chunk.read(&mut reader)
}
