use js_sys::Promise;
use std::collections::HashMap;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{Request, RequestInit, RequestMode, Response, XmlHttpRequest};

use crate::core::api::{CompleteRequest, StartUploadResponse, UploadUrlsBatchResponse};
use crate::core::config::get_config;
use crate::core::ChunkUploadResult;

pub async fn start_upload(
    file_name: &str,
    file_hash: &str,
    file_size: u64,
    user_params: &HashMap<String, String>,
) -> Result<StartUploadResponse, String> {
    let url = get_config().start_upload_api.clone();
    let mut body_map = user_params.clone();
    body_map.insert("fileName".to_string(), file_name.to_string());
    body_map.insert("fileHash".to_string(), file_hash.to_string());
    body_map.insert("fileSize".to_string(), file_size.to_string());
    let body = serde_json::to_string(&body_map).map_err(|e| e.to_string())?;
    fetch_json::<StartUploadResponse>(&url, "POST", Some(&body)).await
}

pub async fn fetch_upload_urls_batch(
    file_key: &str,
    upload_id: &str,
    part_numbers: &[u32],
) -> Result<HashMap<u32, String>, String> {
    let url = get_config().get_upload_urls_api.clone();
    let body_map =
        serde_json::json!({ "key": file_key, "uploadId": upload_id, "parts": part_numbers });
    let body = serde_json::to_string(&body_map).map_err(|e| e.to_string())?;
    let resp: UploadUrlsBatchResponse = fetch_json(&url, "POST", Some(&body)).await?;
    Ok(resp.into_part_map())
}

pub async fn complete_upload(
    file_key: &str,
    upload_id: &str,
    results: Vec<ChunkUploadResult>,
) -> Result<(), String> {
    let base_url = get_config().complete_api.clone();
    let url = format!("{}/{}/{}", base_url, upload_id, file_key);
    let parts = CompleteRequest::from_upload_results(results);
    let body = parts.serialize()?;
    xhr_post_json(&url, &body).await
}

/// POST JSON via XHR — avoids the web-sys RequestInit body bug where the body
/// is not reliably forwarded when using the Fetch API wrapper.
async fn xhr_post_json(url: &str, body: &str) -> Result<(), String> {
    let xhr = XmlHttpRequest::new().map_err(|e| format!("{:?}", e))?;
    xhr.open("POST", url).map_err(|e| format!("{:?}", e))?;
    xhr.set_request_header("Content-Type", "application/json")
        .map_err(|e| format!("{:?}", e))?;

    let body_owned = body.to_string();
    let promise = Promise::new(&mut |resolve, reject| {
        let xhr_clone = xhr.clone();
        let reject_clone = reject.clone();

        let onload = Closure::wrap(Box::new(move || {
            let status = xhr_clone.status().unwrap_or(0);
            if status >= 200 && status < 300 {
                resolve.call1(&JsValue::NULL, &JsValue::NULL).ok();
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

        xhr.set_onload(Some(onload.as_ref().unchecked_ref()));
        xhr.set_onerror(Some(onerror.as_ref().unchecked_ref()));
        onload.forget();
        onerror.forget();
    });

    xhr.send_with_opt_str(Some(&body_owned))
        .map_err(|e| format!("{:?}", e))?;

    JsFuture::from(promise)
        .await
        .map(|_| ())
        .map_err(|e| format!("complete_upload XHR failed: {:?}", e))
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
