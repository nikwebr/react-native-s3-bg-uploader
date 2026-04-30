use futures::FutureExt;
use js_sys::Promise;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_file_reader::WebSysFile;
use wasm_bindgen_futures::JsFuture;

use async_trait::async_trait;

use crate::core::chunk::ChunkInfo;
use crate::core::runtime;
use crate::core::upload::PreparedUpload;
use crate::wasm::upload_engine::{self, AsyncPlatformAdapter};
use crate::core::upload_orchestrator::{self, UploadBackend, UploadOutcome};
use crate::core::{clean_etag, MAX_RETRIES};
use crate::wasm::api::WasmApiClient;
use crate::wasm::progress::{self as wasmProgress, WasmProgressNotifier};
use crate::wasm::is_pause_requested;

pub(super) async fn run_upload(file_key: &str, file: web_sys::File) -> UploadOutcome {
    let backend = WasmUploadBackend {
        file_key: file_key.to_string(),
        file: std::sync::Arc::new(file),
        api: WasmApiClient,
    };
    upload_orchestrator::run_upload(&backend).await
}

struct WasmUploadBackend {
    file_key: String,
    file: std::sync::Arc<web_sys::File>,
    api: WasmApiClient,
}

#[async_trait(?Send)]
impl UploadBackend for WasmUploadBackend {
    type Notifier = WasmProgressNotifier;

    fn progress_manager(&self) -> &crate::core::progress::ProgressManager<Self::Notifier> {
        wasmProgress::progress_manager()
    }

    fn file_key(&self) -> &str {
        &self.file_key
    }

    fn is_paused(&self) -> bool {
        is_pause_requested()
    }

    fn on_session_completed(&self) {
        wasmProgress::progress_manager().clear();
    }

    fn total_bytes(&self) -> Result<u64, String> {
        Ok(self.file.size() as u64)
    }

    async fn upload_parts(
        &self,
        prepared: &PreparedUpload,
        total_bytes: u64,
    ) -> Result<Vec<crate::core::ChunkUploadResult>, String> {
        let adapter = std::sync::Arc::new(WasmAdapter {
            file_key: self.file_key.clone(),
            upload_id: prepared.upload_id.clone(),
        });
        upload_engine::run_async_upload(
            adapter,
            self.file.clone(),
            &self.file_key,
            prepared.part_size,
            total_bytes,
            prepared.remaining_parts.clone(),
        )
        .await
    }

    async fn complete_upload(
        &self,
        upload_id: &str,
        results: Vec<crate::core::ChunkUploadResult>,
    ) -> Result<(), String> {
        use crate::core::api::ApiClient;
        self.api.complete_upload(&self.file_key, upload_id, results).await
    }
}

struct WasmAdapter {
    file_key: String,
    upload_id: String,
}

impl AsyncPlatformAdapter for WasmAdapter {
    fn is_paused(&self) -> bool {
        is_pause_requested()
    }

    fn fetch_urls(
        &self,
        parts: Vec<u32>,
    ) -> futures::future::LocalBoxFuture<
        'static,
        Result<std::collections::HashMap<u32, String>, String>,
    > {
        let file_key = self.file_key.clone();
        let upload_id = self.upload_id.clone();
        async move {
            use crate::core::api::ApiClient;
            WasmApiClient.fetch_upload_urls_batch(&file_key, &upload_id, &parts).await
        }
        .boxed_local()
    }

    fn read_chunk(&self, file: &web_sys::File, chunk: &ChunkInfo) -> Result<Vec<u8>, String> {
        read_chunk_from_web_file(file, chunk)
    }

    fn upload_chunk(
        &self,
        url: String,
        data: Vec<u8>,
        part_number: u32,
        file_key: String,
    ) -> futures::future::LocalBoxFuture<'static, Result<String, String>> {
        async move {
            let chunk_size = data.len() as u64;
            let etag = upload_chunk_with_retry(&url, &data, part_number, &file_key).await?;
            runtime::complete_chunk(
                wasmProgress::progress_manager(),
                &file_key,
                part_number,
                etag.clone(),
                chunk_size,
            );
            Ok(etag)
        }
        .boxed_local()
    }
}

async fn upload_chunk_with_retry(
    url: &str,
    data: &[u8],
    part_number: u32,
    file_key: &str,
) -> Result<String, String> {
    let policy = crate::core::retry::RetryPolicy::new(MAX_RETRIES);
    let file_key = file_key.to_string();
    let url = url.to_string();
    let data = data.to_vec();

    crate::core::retry::run_with_retry_string_async(
        &policy,
        |_attempt| {
            let url = url.clone();
            let data = data.clone();
            let fk = file_key.clone();
            async move {
                if is_pause_requested() {
                    return Err("paused".to_string());
                }
                runtime::update_in_flight(wasmProgress::progress_manager(), &fk, part_number, 0);
                upload_chunk_xhr(&url, &data, part_number, &fk).await
            }
        },
        |_attempt, err, delay_ms| {
            let paused = err == "paused" || is_pause_requested();
            async move {
                if !paused {
                    sleep(delay_ms).await;
                }
            }
        },
    )
    .await
}

async fn upload_chunk_xhr(
    url: &str,
    data: &[u8],
    part_number: u32,
    file_key: &str,
) -> Result<String, String> {
    use wasm_bindgen::closure::Closure;
    use web_sys::XmlHttpRequest;

    if is_pause_requested() {
        return Err("paused".into());
    }

    let xhr = XmlHttpRequest::new().map_err(|e| format!("{:?}", e))?;
    xhr.open("PUT", url).map_err(|e| format!("{:?}", e))?;

    let fk = file_key.to_string();
    let promise = Promise::new(&mut |resolve, reject| {
        let resolve_clone = resolve.clone();
        let reject_clone = reject.clone();
        let xhr_clone = xhr.clone();
        let xhr_for_progress = xhr.clone();
        let fk_clone = fk.clone();
        let reject_for_abort = reject.clone();

        let onprogress = Closure::wrap(Box::new(move |event: web_sys::ProgressEvent| {
            if is_pause_requested() {
                let _ = xhr_for_progress.abort();
                return;
            }
            if event.length_computable() {
                runtime::update_in_flight(
                    wasmProgress::progress_manager(),
                    &fk_clone,
                    part_number,
                    event.loaded() as u64,
                );
            }
        }) as Box<dyn FnMut(_)>);

        let onload = Closure::wrap(Box::new(move || {
            let status = xhr_clone.status().unwrap_or(0);
            if status >= 200 && status < 300 {
                let etag = xhr_clone
                    .get_response_header("etag")
                    .ok()
                    .flatten()
                    .unwrap_or_default();
                resolve_clone
                    .call1(&JsValue::NULL, &JsValue::from_str(&etag))
                    .ok();
            } else {
                reject_clone
                    .call1(
                        &JsValue::NULL,
                        &JsValue::from_str(&format!("HTTP {}", status)),
                    )
                    .ok();
            }
        }) as Box<dyn FnMut()>);

        let onerror = Closure::wrap(Box::new(move || {
            reject
                .call1(&JsValue::NULL, &JsValue::from_str("Network error"))
                .ok();
        }) as Box<dyn FnMut()>);

        let onabort = Closure::wrap(Box::new(move || {
            reject_for_abort
                .call1(&JsValue::NULL, &JsValue::from_str("paused"))
                .ok();
        }) as Box<dyn FnMut()>);

        if let Ok(upload) = xhr.upload() {
            upload.set_onprogress(Some(onprogress.as_ref().unchecked_ref()));
        }
        xhr.set_onload(Some(onload.as_ref().unchecked_ref()));
        xhr.set_onerror(Some(onerror.as_ref().unchecked_ref()));
        xhr.set_onabort(Some(onabort.as_ref().unchecked_ref()));
        onprogress.forget();
        onload.forget();
        onerror.forget();
        onabort.forget();
    });

    let uint8_array = js_sys::Uint8Array::new_with_length(data.len() as u32);
    uint8_array.copy_from(data);
    xhr.send_with_opt_buffer_source(Some(&uint8_array))
        .map_err(|e| format!("{:?}", e))?;

    let result = JsFuture::from(promise)
        .await
        .map_err(|e| format!("{:?}", e))?;
    Ok(clean_etag(&result.as_string().unwrap_or_default()))
}

fn read_chunk_from_web_file(
    file: &web_sys::File,
    chunk_info: &ChunkInfo,
) -> Result<Vec<u8>, String> {
    let mut wf = WebSysFile::new(file.clone());
    chunk_info
        .read(&mut wf)
        .map_err(|e| format!("Read failed: {}", e))
}

async fn sleep(ms: u32) {
    let promise = Promise::new(&mut |resolve, _| {
        if let Ok(worker) = js_sys::global().dyn_into::<web_sys::WorkerGlobalScope>() {
            worker
                .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms as i32)
                .ok();
        } else if let Some(window) = web_sys::window() {
            window
                .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms as i32)
                .ok();
        }
    });
    JsFuture::from(promise).await.ok();
}
