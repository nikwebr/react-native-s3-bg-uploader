use async_trait::async_trait;
use std::collections::HashMap;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{Request, RequestInit, RequestMode, Response};

use crate::core::api::{
    complete_upload_body, complete_upload_url, start_upload_body, upload_urls_batch_body,
    ApiClient, StartUploadResponse, UploadUrlsBatchResponse,
};
use crate::core::config::get_config;
use crate::core::ChunkUploadResult;

pub struct WasmApiClient;

#[async_trait(?Send)]
impl ApiClient for WasmApiClient {
    async fn start_upload(
        &self,
        file_name: &str,
        file_hash: &str,
        file_size: u64,
        user_params: &HashMap<String, String>,
    ) -> Result<StartUploadResponse, String> {
        let url = get_config().start_upload_api.clone();
        let body = start_upload_body(file_name, file_hash, file_size, user_params)?;
        fetch_json::<StartUploadResponse>(&url, "POST", Some(&body)).await
    }

    async fn fetch_upload_urls_batch(
        &self,
        key: &str,
        upload_id: &str,
        part_numbers: &[u32],
    ) -> Result<HashMap<u32, String>, String> {
        let url = get_config().get_upload_urls_api.clone();
        let body = upload_urls_batch_body(key, upload_id, part_numbers)?;
        let resp: UploadUrlsBatchResponse = fetch_json(&url, "POST", Some(&body)).await?;
        Ok(resp.into_part_map())
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
        fetch_json::<serde_json::Value>(&url, "POST", Some(&body))
            .await
            .map(|_| ())
    }
}

async fn fetch_request(request: &Request) -> Result<JsValue, JsValue> {
    if let Ok(worker) = js_sys::global().dyn_into::<web_sys::WorkerGlobalScope>() {
        return JsFuture::from(worker.fetch_with_request(request)).await;
    }
    if let Some(window) = web_sys::window() {
        return JsFuture::from(window.fetch_with_request(request)).await;
    }
    Err(JsValue::from_str("No global scope available"))
}

async fn fetch_json<T: serde::de::DeserializeOwned>(
    url: &str,
    method: &str,
    body: Option<&str>,
) -> Result<T, String> {
    let opts = RequestInit::new();
    opts.set_method(method);
    opts.set_mode(RequestMode::Cors);
    if let Some(body_str) = body {
        opts.set_body(&JsValue::from_str(body_str));
    }

    let request = Request::new_with_str_and_init(url, &opts)
        .map_err(|e| format!("Failed to create request: {:?}", e))?;
    request
        .headers()
        .set("Content-Type", "application/json")
        .map_err(|_| "Failed to set Content-Type header")?;

    let resp_value = fetch_request(&request)
        .await
        .map_err(|e| format!("Fetch failed: {:?}", e))?;

    let resp: Response = resp_value
        .dyn_into()
        .map_err(|_| "Not a Response".to_string())?;
    if !resp.ok() {
        return Err(format!("HTTP error: {}", resp.status()));
    }

    let json = JsFuture::from(resp.json().map_err(|_| "Failed to get json".to_string())?)
        .await
        .map_err(|e| format!("JSON parsing failed: {:?}", e))?;

    let json_str = js_sys::JSON::stringify(&json)
        .map_err(|_| "Failed to stringify".to_string())?
        .as_string()
        .ok_or("Not a string")?;

    serde_json::from_str(&json_str).map_err(|e| format!("Deserialization failed: {}", e))
}
