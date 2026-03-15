use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{Headers, Request, RequestInit, RequestMode, Response};
use crate::core::api::{CompleteRequest, UploadUrlsResponse};
use crate::core::{complete_upload_endpoint, get_urls_endpoint, ChunkUploadResult};

pub async fn fetch_upload_urls(file_size: u64) -> Result<UploadUrlsResponse, String> {
    let url = get_urls_endpoint(file_size);
    fetch_json::<UploadUrlsResponse>(&url, "POST", None)
        .await
        .map_err(|e| format!("Failed to get upload URLs: {}", e))
}

pub async fn complete_upload_async(
    upload_info: &UploadUrlsResponse,
    results: Vec<ChunkUploadResult>,
) -> Result<(), String> {
    let parts = CompleteRequest::from_upload_results(results);
    let complete_url = complete_upload_endpoint(&upload_info.upload_id, &upload_info.key);
    let body = parts.serialize()?;

    fetch_json::<serde_json::Value>(&complete_url, "POST", Some(&body))
        .await
        .map_err(|e| format!("Failed to complete upload: {}", e))?;

    Ok(())
}

async fn fetch_request(request: &Request) -> Result<JsValue, JsValue> {
    // Versuche zuerst WorkerGlobalScope (für Web Worker)
    if let Ok(worker) = js_sys::global().dyn_into::<web_sys::WorkerGlobalScope>() {
        return JsFuture::from(worker.fetch_with_request(request)).await;
    }

    // Fallback auf Window (für Haupt-Thread)
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

    let headers = Headers::new().map_err(|_| "Failed to create headers")?;
    headers.set("Content-Type", "application/json")
        .map_err(|_| "Failed to set content-type")?;

    if let Some(body_str) = body {
        opts.set_body(&JsValue::from_str(body_str));
    }

    let request = Request::new_with_str_and_init(url, &opts)
        .map_err(|e| format!("Failed to create request: {:?}", e))?;

    request.headers().set("Content-Type", "application/json")
        .map_err(|_| "Failed to set header")?;

    let resp_value = fetch_request(&request)
        .await
        .map_err(|e| format!("Fetch failed: {:?}", e))?;

    let resp: Response = resp_value.dyn_into().map_err(|_| "Not a Response")?;

    if !resp.ok() {
        return Err(format!("HTTP error: {}", resp.status()));
    }

    let json = JsFuture::from(resp.json().map_err(|_| "Failed to get json")?)
        .await
        .map_err(|e| format!("JSON parsing failed: {:?}", e))?;

    let json_str = js_sys::JSON::stringify(&json)
        .map_err(|_| "Failed to stringify")?
        .as_string()
        .ok_or("Not a string")?;

    serde_json::from_str(&json_str).map_err(|e| format!("Deserialization failed: {}", e))
}