use crate::core::api::{CompleteRequest, UploadUrlsResponse};
use crate::core::{complete_upload_endpoint, get_urls_endpoint, ChunkUploadResult};

pub fn fetch_upload_urls(
    client: &nyquest::BlockingClient,
    file_size: u64,
) -> Result<UploadUrlsResponse, Box<dyn std::error::Error>> {
    let url = get_urls_endpoint(file_size);
    let request = nyquest::Request::post(url);
    let response = client
        .request(request)
        .map_err(|e| format!("Failed to get upload URLs: {:?}", e))?;

    let response_body = response
        .text()
        .map_err(|e| format!("Failed to read response body: {:?}", e))?;

    serde_json::from_str(&response_body)
        .map_err(|e| format!("Failed to parse upload URLs response: {:?}", e).into())
}


pub fn complete_upload(
    client: &nyquest::BlockingClient,
    upload_info: &UploadUrlsResponse,
    results: Vec<ChunkUploadResult>,
) -> Result<(), Box<dyn std::error::Error>> {
    let parts = CompleteRequest::from_upload_results(results);
    let complete_url = complete_upload_endpoint(&upload_info.upload_id, &upload_info.key);
    let body_json = parts.serialize()
        .map_err(|e| format!("Failed to serialize complete request: {}", e))?;

    let body = nyquest::Body::bytes(body_json.into_bytes(), "application/json");
    let request = nyquest::Request::post(complete_url).with_body(body);

    client
        .request(request)
        .map_err(|e| format!("Failed to complete upload: {:?}", e))?;

    Ok(())
}